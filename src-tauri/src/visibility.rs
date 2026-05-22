//! Visibility presets — closure algorithm.
//!
//! Given a `SeedSpec` (a bag of author names + explicit package IDs),
//! resolve the transitive dependency closure: the smallest set containing
//! every seed and every locally-resolved dep of every package in the set.
//!
//! Output drives the Load/Unload feature: the active folder (= addon_root
//! post-setup) is kept equal to this closure as hardlinks.
//!
//! See TODO-visibility-presets.md for the full design context.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

/// What the user wants visible. Creators resolve dynamically against
/// `packages.creator` at closure time, so a new .var by a seeded author
/// joins the closure on the next compute. Package IDs are explicit
/// hand-picks that bypass the `is_hidden` filter.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SeedSpec {
    /// Creator names to include. Matched case-insensitively against
    /// `packages.creator` (mirroring `list_creators_with_counts`).
    pub creators: Vec<String>,
    /// Explicit package IDs to include regardless of creator/hidden flag.
    pub package_ids: Vec<i64>,
}

impl SeedSpec {
    pub fn is_empty(&self) -> bool {
        self.creators.is_empty() && self.package_ids.is_empty()
    }
}

/// Counts surfaced to the UI before any FS write.
#[derive(Debug, Clone, Serialize)]
pub struct ClosurePreview {
    /// Packages contributed by author seeds (intersected with closure).
    pub from_authors: i64,
    /// Packages contributed by explicit package seeds, minus those
    /// already covered by authors.
    pub from_packages: i64,
    /// Packages pulled in only by transitive dep resolution, not directly
    /// seeded.
    pub from_deps: i64,
    /// Total resolved IDs in the closure.
    pub total: i64,
    /// All package IDs in the closure (caller usually wants this for the
    /// diff against `active_folder_state`).
    pub package_ids: Vec<i64>,
    /// Dep keys that referenced a non-installed package, paired with the
    /// id of the package that referenced them. Surfaced as "missing
    /// dependency" warnings in the UI.
    pub unresolved: Vec<UnresolvedDep>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UnresolvedDep {
    pub src_package_id: i64,
    pub raw_dep_key: String,
}

/// Resolve the closure of `seeds` against the package_dep_links graph.
/// Returns the sorted list of package IDs in the closure.
///
/// Implemented via a SQLite recursive CTE over two temp tables (one per
/// seed kind). The temp tables let us bind variable-size seed lists
/// without smacking into SQLite's bound-param limit and without escaping
/// creator names into the SQL string by hand.
pub fn compute_closure(conn: &Connection, seeds: &SeedSpec) -> Result<Vec<i64>> {
    populate_seed_tables(conn, seeds)?;

    let mut stmt = conn.prepare(CLOSURE_SQL)?;
    let ids: Vec<i64> = stmt
        .query_map([], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}

/// Full preview struct including author/package/dep breakdown and
/// unresolved-dep list. More expensive than `compute_closure` (extra
/// queries for the breakdown); use it when you actually need the
/// counts to render in the UI.
pub fn compute_preview(conn: &Connection, seeds: &SeedSpec) -> Result<ClosurePreview> {
    populate_seed_tables(conn, seeds)?;

    // Closure itself.
    let closure_ids: Vec<i64> = {
        let mut stmt = conn.prepare(CLOSURE_SQL)?;
        let ids = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        ids
    };

    // Direct seeds from author table (intersected with closure means
    // "matched a package row and survived is_hidden"). Counted before we
    // walk deps so the breakdown reflects intent, not the dep closure.
    let from_authors: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT p.id)
           FROM packages p
           JOIN seed_creators sc ON sc.creator = p.creator COLLATE NOCASE
          WHERE p.is_hidden = 0",
        [],
        |r| r.get(0),
    )?;

    // Explicit package seeds NOT already covered by authors. "Net new"
    // hand-picks.
    let from_packages: i64 = conn.query_row(
        "SELECT COUNT(*)
           FROM seed_packages sp
          WHERE NOT EXISTS (
              SELECT 1 FROM packages p
                JOIN seed_creators sc ON sc.creator = p.creator COLLATE NOCASE
               WHERE p.id = sp.package_id AND p.is_hidden = 0
          )",
        [],
        |r| r.get(0),
    )?;

    let total: i64 = closure_ids.len() as i64;
    let from_deps = (total - from_authors - from_packages).max(0);

    // Unresolved deps within the closure: dep keys whose dst is NULL but
    // whose src is in our closure (so the user would actually notice
    // them). Stage closure_ids in a temp table so the IN-subquery
    // doesn't need a dynamic param list.
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS closure_ids (id INTEGER PRIMARY KEY);
         DELETE FROM closure_ids;",
    )?;
    {
        let mut ins = conn.prepare("INSERT INTO closure_ids(id) VALUES (?1)")?;
        for &id in &closure_ids {
            ins.execute(params![id])?;
        }
    }
    let unresolved: Vec<UnresolvedDep> = {
        let mut stmt = conn.prepare(
            "SELECT l.src_package_id, l.raw_dep_key
               FROM package_dep_links l
              WHERE l.dst_package_id IS NULL
                AND l.src_package_id IN (SELECT id FROM closure_ids)
              ORDER BY l.raw_dep_key",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(UnresolvedDep {
                    src_package_id: r.get(0)?,
                    raw_dep_key: r.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };

    Ok(ClosurePreview {
        from_authors,
        from_packages,
        from_deps,
        total,
        package_ids: closure_ids,
        unresolved,
    })
}

/// Wipe and repopulate the temp seed tables from `seeds`. Idempotent
/// across calls on the same connection (the temp tables persist for the
/// life of the connection).
fn populate_seed_tables(conn: &Connection, seeds: &SeedSpec) -> Result<()> {
    conn.execute_batch(
        "CREATE TEMP TABLE IF NOT EXISTS seed_creators (creator TEXT NOT NULL);
         CREATE TEMP TABLE IF NOT EXISTS seed_packages (package_id INTEGER NOT NULL);
         DELETE FROM seed_creators;
         DELETE FROM seed_packages;",
    )?;
    {
        let mut ins = conn.prepare("INSERT INTO seed_creators (creator) VALUES (?1)")?;
        for c in &seeds.creators {
            ins.execute(params![c])?;
        }
    }
    {
        let mut ins = conn.prepare("INSERT INTO seed_packages (package_id) VALUES (?1)")?;
        for &id in &seeds.package_ids {
            ins.execute(params![id])?;
        }
    }
    Ok(())
}

/// Recursive CTE: closure starts from
///   (a) packages whose creator matches a seed_creators row AND aren't hidden
///   (b) every explicit seed_packages row (no is_hidden filter — user opt-in)
/// then walks `package_dep_links` until the working set stops growing.
///
/// `UNION` (not `UNION ALL`) gives the dedupe-and-terminate property for
/// cycles. Output is sorted by id for stable test assertions.
const CLOSURE_SQL: &str = "
    WITH RECURSIVE
    seeds(id) AS (
        SELECT p.id
          FROM packages p
          JOIN seed_creators sc ON sc.creator = p.creator COLLATE NOCASE
         WHERE p.is_hidden = 0
        UNION
        SELECT package_id FROM seed_packages
    ),
    closure(id) AS (
        SELECT id FROM seeds
        UNION
        SELECT l.dst_package_id
          FROM package_dep_links l
          JOIN closure cl ON cl.id = l.src_package_id
         WHERE l.dst_package_id IS NOT NULL
    )
    SELECT id FROM closure ORDER BY id
";

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// Build a minimal schema for closure testing. Just the columns the
    /// closure CTE reads — full migration not needed.
    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE packages (
                id           INTEGER PRIMARY KEY,
                creator      TEXT NOT NULL,
                package_name TEXT NOT NULL DEFAULT '',
                version      TEXT NOT NULL DEFAULT '1',
                is_hidden    INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE package_dep_links (
                 src_package_id INTEGER NOT NULL,
                 dst_package_id INTEGER,
                 raw_dep_key    TEXT NOT NULL,
                 PRIMARY KEY (src_package_id, raw_dep_key)
             );",
        )
        .unwrap();
        conn
    }

    fn add_pkg(conn: &Connection, id: i64, creator: &str, name: &str) {
        conn.execute(
            "INSERT INTO packages (id, creator, package_name) VALUES (?1, ?2, ?3)",
            params![id, creator, name],
        )
        .unwrap();
    }

    fn add_pkg_hidden(conn: &Connection, id: i64, creator: &str, name: &str) {
        conn.execute(
            "INSERT INTO packages (id, creator, package_name, is_hidden)
             VALUES (?1, ?2, ?3, 1)",
            params![id, creator, name],
        )
        .unwrap();
    }

    fn add_dep(conn: &Connection, src: i64, dst: Option<i64>, key: &str) {
        conn.execute(
            "INSERT INTO package_dep_links (src_package_id, dst_package_id, raw_dep_key)
             VALUES (?1, ?2, ?3)",
            params![src, dst, key],
        )
        .unwrap();
    }

    #[test]
    fn empty_seeds_yields_empty_closure() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Foo");
        let ids = compute_closure(&conn, &SeedSpec::default()).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn author_seed_picks_up_all_creators_packages() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Foo");
        add_pkg(&conn, 2, "Alice", "Bar");
        add_pkg(&conn, 3, "Bob", "Baz");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let ids = compute_closure(&conn, &seeds).unwrap();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn author_seed_is_case_insensitive() {
        let conn = setup_db();
        add_pkg(&conn, 1, "AcidBubbles", "Timeline");
        add_pkg(&conn, 2, "acidbubbles", "OldVer"); // different casing
        add_pkg(&conn, 3, "MeshedVR", "Atom");

        let seeds = SeedSpec {
            creators: vec!["ACIDBUBBLES".into()],
            package_ids: vec![],
        };
        let mut ids = compute_closure(&conn, &seeds).unwrap();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn package_seed_picks_explicit_ids() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Foo");
        add_pkg(&conn, 2, "Bob", "Bar");
        add_pkg(&conn, 3, "Carol", "Baz");

        let seeds = SeedSpec {
            creators: vec![],
            package_ids: vec![1, 3],
        };
        let ids = compute_closure(&conn, &seeds).unwrap();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn mixed_seeds_union_correctly() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Foo");
        add_pkg(&conn, 2, "Alice", "Bar");
        add_pkg(&conn, 3, "Bob", "Baz");
        add_pkg(&conn, 4, "Carol", "Qux");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![4],
        };
        let mut ids = compute_closure(&conn, &seeds).unwrap();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 4]);
    }

    #[test]
    fn dep_closure_walks_one_hop() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Scene");
        add_pkg(&conn, 2, "Bob", "Plugin");
        add_dep(&conn, 1, Some(2), "Bob.Plugin.1");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let mut ids = compute_closure(&conn, &seeds).unwrap();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn dep_closure_walks_chains() {
        let conn = setup_db();
        // 1 -> 2 -> 3 -> 4
        add_pkg(&conn, 1, "Alice", "Scene");
        add_pkg(&conn, 2, "Bob", "PluginA");
        add_pkg(&conn, 3, "Carol", "PluginB");
        add_pkg(&conn, 4, "Dave", "PluginC");
        add_dep(&conn, 1, Some(2), "Bob.PluginA.1");
        add_dep(&conn, 2, Some(3), "Carol.PluginB.1");
        add_dep(&conn, 3, Some(4), "Dave.PluginC.1");
        // unrelated dep tree should NOT be pulled in
        add_pkg(&conn, 5, "Eve", "Unrelated");
        add_pkg(&conn, 6, "Frank", "Lonely");
        add_dep(&conn, 5, Some(6), "Frank.Lonely.1");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let mut ids = compute_closure(&conn, &seeds).unwrap();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn unresolved_deps_do_not_block_closure() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Scene");
        add_pkg(&conn, 2, "Bob", "Plugin");
        add_dep(&conn, 1, Some(2), "Bob.Plugin.1");
        // dst NULL = user doesn't own this package
        add_dep(&conn, 1, None, "Missing.Author.5");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let mut ids = compute_closure(&conn, &seeds).unwrap();
        ids.sort();
        // Closure still includes the resolvable side.
        assert_eq!(ids, vec![1, 2]);

        let preview = compute_preview(&conn, &seeds).unwrap();
        assert_eq!(preview.total, 2);
        assert_eq!(preview.unresolved.len(), 1);
        assert_eq!(preview.unresolved[0].src_package_id, 1);
        assert_eq!(preview.unresolved[0].raw_dep_key, "Missing.Author.5");
    }

    #[test]
    fn cycles_terminate() {
        let conn = setup_db();
        // Pathological: 1 -> 2 -> 3 -> 1
        add_pkg(&conn, 1, "Alice", "A");
        add_pkg(&conn, 2, "Bob", "B");
        add_pkg(&conn, 3, "Carol", "C");
        add_dep(&conn, 1, Some(2), "Bob.B.1");
        add_dep(&conn, 2, Some(3), "Carol.C.1");
        add_dep(&conn, 3, Some(1), "Alice.A.1");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let mut ids = compute_closure(&conn, &seeds).unwrap();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn hidden_packages_excluded_from_author_seed() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Visible");
        add_pkg_hidden(&conn, 2, "Alice", "Hidden");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let ids = compute_closure(&conn, &seeds).unwrap();
        assert_eq!(ids, vec![1]);
    }

    #[test]
    fn hidden_packages_included_via_explicit_package_seed() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Visible");
        add_pkg_hidden(&conn, 2, "Alice", "Hidden");

        // User explicitly opted into the hidden one.
        let seeds = SeedSpec {
            creators: vec![],
            package_ids: vec![2],
        };
        let ids = compute_closure(&conn, &seeds).unwrap();
        assert_eq!(ids, vec![2]);
    }

    #[test]
    fn duplicate_seeds_are_deduped() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Foo");
        add_pkg(&conn, 2, "Alice", "Bar");

        let seeds = SeedSpec {
            // Author + explicit pkg that's also covered by author = no dupes.
            creators: vec!["Alice".into(), "Alice".into()],
            package_ids: vec![1, 1, 2],
        };
        let ids = compute_closure(&conn, &seeds).unwrap();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn repeated_calls_on_same_connection() {
        // Temp tables persist for the life of the connection; second call
        // must reset cleanly without leaking rows from the first.
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "Foo");
        add_pkg(&conn, 2, "Bob", "Bar");

        let s1 = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let ids1 = compute_closure(&conn, &s1).unwrap();
        assert_eq!(ids1, vec![1]);

        let s2 = SeedSpec {
            creators: vec!["Bob".into()],
            package_ids: vec![],
        };
        let ids2 = compute_closure(&conn, &s2).unwrap();
        assert_eq!(ids2, vec![2]); // No carryover from s1.
    }

    #[test]
    fn preview_breakdown_attributes_seeds_correctly() {
        let conn = setup_db();
        add_pkg(&conn, 1, "Alice", "A1");
        add_pkg(&conn, 2, "Alice", "A2");
        add_pkg(&conn, 3, "Bob", "B1"); // explicit package seed
        add_pkg(&conn, 4, "Carol", "C1"); // pulled in by dep
        add_dep(&conn, 1, Some(4), "Carol.C1.1");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![3],
        };
        let preview = compute_preview(&conn, &seeds).unwrap();
        assert_eq!(preview.from_authors, 2); // 1, 2
        assert_eq!(preview.from_packages, 1); // 3 (not covered by Alice)
        assert_eq!(preview.from_deps, 1); // 4
        assert_eq!(preview.total, 4);
        assert_eq!(preview.unresolved.len(), 0);
    }
}
