//! Package-family computation. Groups packages by (creator, package_name)
//! so multi-version sets count as one logical plugin. Idempotent — safe to
//! re-run after every scan; only the first run inherits v3-era tagging data
//! from the package level into the family level.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::Connection;

#[derive(Debug, Default)]
pub struct RecomputeStats {
    pub families_before: i64,
    pub families_after: i64,
    pub families_added: i64,
    pub packages_linked_this_run: usize,
    pub families_with_latest: i64,
    pub families_inheriting_tags: usize,
    pub family_tag_rows_added: usize,
}

/// Walk `packages` and reconcile the `package_family` table:
///   1. Insert family rows for any new (creator, package_name) pairs.
///   2. Link unlinked `packages.family_id` rows to their family.
///   3. Recompute each family's `latest_package_id` — highest integer
///      version, ties broken by most-recent scan, then highest id.
///   4. For families with no tagging state yet, copy state from their
///      current latest package (one-shot v3→family inheritance).
///   5. Mirror `package_tags` of the latest package into `family_tags`
///      (idempotent via INSERT OR IGNORE; never deletes).
pub fn recompute(conn: &Connection) -> Result<RecomputeStats> {
    let families_before: i64 =
        conn.query_row("SELECT COUNT(*) FROM package_family", [], |r| r.get(0))?;

    let tx = conn.unchecked_transaction()?;

    // 1. Add families for any new (creator, package_name) we haven't seen.
    let added = tx.execute(
        "INSERT OR IGNORE INTO package_family (creator, package_name)
         SELECT DISTINCT creator, package_name FROM packages
         WHERE creator <> '' AND package_name <> ''",
        [],
    )?;

    // 2. Link packages.family_id where it's still NULL. Earlier scans + new
    //    scans both flow through this single update.
    let linked = tx.execute(
        "UPDATE packages SET family_id = (
            SELECT pf.id FROM package_family pf
             WHERE pf.creator = packages.creator
               AND pf.package_name = packages.package_name
         )
         WHERE family_id IS NULL
           AND creator <> '' AND package_name <> ''",
        [],
    )?;

    // 3. Recompute latest_package_id every run — accommodates new versions
    //    added after the previous recompute. CAST AS INTEGER treats
    //    non-numeric versions as 0; tied versions break on scanned_at then
    //    id to keep the choice stable.
    tx.execute(
        "UPDATE package_family SET latest_package_id = (
            SELECT p.id FROM packages p
            WHERE p.family_id = package_family.id
              AND p.error IS NULL
            ORDER BY CAST(p.version AS INTEGER) DESC,
                     p.scanned_at DESC,
                     p.id DESC
            LIMIT 1
         )",
        [],
    )?;

    // 4. One-shot tagging-state inheritance. Only families that don't yet
    //    have their own tagging state inherit from the package row. This
    //    means subsequent re-runs don't clobber family-level work done by
    //    the tag_library runner.
    let inherited = tx.execute(
        "UPDATE package_family SET
            purpose                   = (SELECT purpose                   FROM packages WHERE id = package_family.latest_package_id),
            out_of_scope              = COALESCE((SELECT out_of_scope     FROM packages WHERE id = package_family.latest_package_id), 0),
            tagging_state             = (SELECT tagging_state             FROM packages WHERE id = package_family.latest_package_id),
            tagging_model             = (SELECT tagging_model             FROM packages WHERE id = package_family.latest_package_id),
            taxonomy_version          = (SELECT taxonomy_version          FROM packages WHERE id = package_family.latest_package_id),
            tagged_at                 = (SELECT tagged_at                 FROM packages WHERE id = package_family.latest_package_id),
            tagging_suggested_new_tag = (SELECT tagging_suggested_new_tag FROM packages WHERE id = package_family.latest_package_id),
            tagging_notes             = (SELECT tagging_notes             FROM packages WHERE id = package_family.latest_package_id)
         WHERE tagging_state IS NULL
           AND latest_package_id IS NOT NULL",
        [],
    )?;

    // 5. Mirror package_tags into family_tags. INSERT OR IGNORE means we
    //    only add; this is correct because a family's tags are managed by
    //    the tag_library runner going forward (which writes via
    //    DELETE-and-reinsert per family). The first run picks up v3 tags
    //    from packages.
    let tag_rows = tx.execute(
        "INSERT OR IGNORE INTO family_tags (family_id, tag)
         SELECT pf.id, pt.tag
         FROM package_family pf
         JOIN package_tags pt ON pt.package_id = pf.latest_package_id
         WHERE pf.latest_package_id IS NOT NULL",
        [],
    )?;

    tx.commit()?;

    let families_after: i64 =
        conn.query_row("SELECT COUNT(*) FROM package_family", [], |r| r.get(0))?;
    let families_with_latest: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family WHERE latest_package_id IS NOT NULL",
        [],
        |r| r.get(0),
    )?;

    // Touch unix time so callers can know when this ran (not stored yet, but
    // could land in a future `family_runs` audit table if needed).
    let _now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    Ok(RecomputeStats {
        families_before,
        families_after,
        families_added: added as i64,
        packages_linked_this_run: linked,
        families_with_latest,
        families_inheriting_tags: inherited,
        family_tag_rows_added: tag_rows,
    })
}
