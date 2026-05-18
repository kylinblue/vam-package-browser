//! Dependency-key parsing and resolution into local `packages` rows.
//!
//! Inputs:
//!   - `package_dependencies(package_id, dep_key)` — every raw key the scanner
//!     pulled from each .var's meta.json.
//!   - `packages(id, creator, package_name, version)` — every package the user
//!     has locally.
//!
//! Output:
//!   - `package_dep_links(src_package_id, dst_package_id, raw_dep_key)` —
//!     resolved edges. `dst_package_id` is NULL when no local package matches
//!     (user doesn't have the dependency installed).
//!
//! Version policy: `Author.Pkg.latest` resolves to the locally-installed
//! package with the highest numeric version. `Author.Pkg.<n>` resolves to the
//! exact local row with that version string. Creator + package name are
//! matched case-insensitively.

use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection};

/// Wipe `package_dep_links` and rebuild it from `package_dependencies` joined
/// against the current `packages` table. Called at the end of every scan; cheap
/// because `package_dependencies` is small (~tens of thousands of rows even on
/// a large library).
pub fn resolve_all(conn: &Connection) -> Result<()> {
    // 1. Build a (creator_lc, package_lc) -> Vec<(version_string, id)> index.
    let mut by_pkg: HashMap<(String, String), Vec<(String, i64)>> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, creator, package_name, version
             FROM packages
             WHERE creator <> '' AND package_name <> ''",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (id, creator, package, version) = row?;
            by_pkg
                .entry((creator.to_lowercase(), package.to_lowercase()))
                .or_default()
                .push((version, id));
        }
    }

    // 2. Load every raw dependency key, resolve, accumulate rows to insert.
    let mut to_insert: Vec<(i64, Option<i64>, String)> = Vec::new();
    {
        let mut stmt = conn.prepare("SELECT package_id, dep_key FROM package_dependencies")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (src_id, dep_key) = row?;
            let dst_id = match parse_dep_key(&dep_key) {
                Some((creator, package, version)) => {
                    let key = (creator.to_lowercase(), package.to_lowercase());
                    by_pkg.get(&key).and_then(|candidates| {
                        if version.eq_ignore_ascii_case("latest") {
                            pick_latest(candidates)
                        } else {
                            pick_exact(candidates, &version)
                        }
                    })
                }
                None => None,
            };
            to_insert.push((src_id, dst_id, dep_key));
        }
    }

    // 3. Rebuild the table in one transaction-friendly batch. Caller wraps in a
    // transaction (we're called from inside scanner::scan's tx).
    conn.execute("DELETE FROM package_dep_links", [])?;
    let mut ins = conn.prepare(
        "INSERT INTO package_dep_links (src_package_id, dst_package_id, raw_dep_key)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(src_package_id, raw_dep_key) DO UPDATE SET
            dst_package_id = excluded.dst_package_id",
    )?;
    for (src, dst, key) in &to_insert {
        ins.execute(params![src, dst, key])?;
    }
    Ok(())
}

/// `Author.Package.<version>` → `(creator, package, version)`. Splits on the
/// last two `.` so creators or package names that happen to contain dots are
/// handled correctly (uncommon, but it happens). Returns None on malformed keys.
fn parse_dep_key(key: &str) -> Option<(String, String, String)> {
    let last = key.rfind('.')?;
    let version = key[last + 1..].to_string();
    let head = &key[..last];
    let prev = head.rfind('.')?;
    let package = head[prev + 1..].to_string();
    let creator = head[..prev].to_string();
    if creator.is_empty() || package.is_empty() || version.is_empty() {
        return None;
    }
    Some((creator, package, version))
}

/// Pick the locally-installed version that best satisfies `Author.Pkg.latest`.
/// Highest numeric version wins; non-numeric versions are treated as 0 so
/// they only get picked when nothing better is available.
fn pick_latest(candidates: &[(String, i64)]) -> Option<i64> {
    candidates
        .iter()
        .max_by_key(|(v, _)| v.parse::<i64>().unwrap_or(0))
        .map(|(_, id)| *id)
}

/// Pick the locally-installed row matching the exact version string.
/// Comparison is case-insensitive in case meta authors get creative.
fn pick_exact(candidates: &[(String, i64)], version: &str) -> Option<i64> {
    candidates
        .iter()
        .find(|(v, _)| v.eq_ignore_ascii_case(version))
        .map(|(_, id)| *id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_key() {
        let r = parse_dep_key("AcidBubbles.Timeline.289").unwrap();
        assert_eq!(r, ("AcidBubbles".into(), "Timeline".into(), "289".into()));
    }

    #[test]
    fn parse_latest_key() {
        let r = parse_dep_key("Foo.Bar.latest").unwrap();
        assert_eq!(r.2, "latest");
    }

    #[test]
    fn parse_dotted_creator() {
        let r = parse_dep_key("Some.Dotted.Creator.PackageName.5").unwrap();
        assert_eq!(r.0, "Some.Dotted.Creator");
        assert_eq!(r.1, "PackageName");
        assert_eq!(r.2, "5");
    }

    #[test]
    fn parse_malformed() {
        assert!(parse_dep_key("nodots").is_none());
        assert!(parse_dep_key("only.one").is_none());
        // Empty creator/package/version rejected.
        assert!(parse_dep_key(".a.b").is_none());
        assert!(parse_dep_key("a..b").is_none());
        assert!(parse_dep_key("a.b.").is_none());
    }

    #[test]
    fn latest_picks_highest_numeric() {
        let cands = vec![
            ("2".into(), 100),
            ("10".into(), 200),
            ("5".into(), 150),
        ];
        assert_eq!(pick_latest(&cands), Some(200));
    }

    #[test]
    fn exact_matches_string() {
        let cands = vec![("3".into(), 10), ("4".into(), 20)];
        assert_eq!(pick_exact(&cands, "4"), Some(20));
        assert_eq!(pick_exact(&cands, "9"), None);
    }
}
