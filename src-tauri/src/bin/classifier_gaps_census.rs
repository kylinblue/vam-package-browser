//! One-shot census of the classifier's residual gaps. Read-only.
//!
//! Buckets the 4353 packages into four mutually-exclusive groups and prints
//! counts + first-N examples per bucket:
//!
//!   A. Labeled (hub truth)            hub_category IS NOT NULL
//!   B. Predicted                      hub IS NULL AND predicted IS NOT NULL
//!   C. Unpredicted with family        hub IS NULL AND predicted IS NULL AND family_id IS NOT NULL
//!   D. No family_id                   family_id IS NULL
//!
//! Then for B and C, also break down by whether the row's family has a
//! labeled sibling (the "unlabeled-sibling-in-labeled-family" case I
//! estimated at ~150 earlier — let's get the actual number).

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OpenFlags};

const SAMPLE_PER_BUCKET: usize = 8;

fn main() -> Result<()> {
    let db_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!("index db not found at {}", db_path.display()));
    }
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open read-only {}", db_path.display()))?;

    let total: i64 = conn.query_row("SELECT COUNT(*) FROM packages", [], |r| r.get(0))?;
    println!("# Classifier residual census");
    println!();
    println!("Total packages: {}", total);
    println!();

    // Families that have at least one labeled package — used to identify
    // "unlabeled sibling in labeled family" rows.
    let mut labeled_families: HashSet<i64> = HashSet::new();
    {
        let mut stmt = conn.prepare(
            "SELECT DISTINCT family_id FROM packages
             WHERE hub_category IS NOT NULL AND family_id IS NOT NULL",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            labeled_families.insert(row.get::<_, i64>(0)?);
        }
    }
    println!("Labeled families: {}", labeled_families.len());
    println!();

    bucket_report(
        &conn,
        "A. Labeled (hub_category set)",
        "hub_category IS NOT NULL",
    )?;
    bucket_report(
        &conn,
        "B. Predicted (hub IS NULL, predicted set)",
        "hub_category IS NULL AND predicted_hub_category IS NOT NULL",
    )?;
    bucket_report(
        &conn,
        "C. Unpredicted with family_id",
        "hub_category IS NULL AND predicted_hub_category IS NULL AND family_id IS NOT NULL",
    )?;
    bucket_report(
        &conn,
        "D. No family_id (any state)",
        "family_id IS NULL",
    )?;

    // Cross-cut: unlabeled rows whose family IS labeled (other versions of
    // same package matched the hub). This is the family-sibling gap.
    println!();
    println!("--- Cross-cut: unlabeled rows in *labeled* families ---");
    let unlabeled_in_labeled_fam_total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packages
         WHERE hub_category IS NULL
           AND family_id IS NOT NULL
           AND family_id IN (
               SELECT family_id FROM packages
               WHERE hub_category IS NOT NULL AND family_id IS NOT NULL
           )",
        [],
        |r| r.get(0),
    )?;
    println!(
        "  count (any prediction state): {}",
        unlabeled_in_labeled_fam_total
    );

    let same_with_predicted: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packages
         WHERE hub_category IS NULL
           AND predicted_hub_category IS NOT NULL
           AND family_id IS NOT NULL
           AND family_id IN (
               SELECT family_id FROM packages
               WHERE hub_category IS NOT NULL AND family_id IS NOT NULL
           )",
        [],
        |r| r.get(0),
    )?;
    println!(
        "    of which have a prediction (from kind-vote/graph-prop): {}",
        same_with_predicted
    );

    let same_with_unpredicted: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packages
         WHERE hub_category IS NULL
           AND predicted_hub_category IS NULL
           AND family_id IS NOT NULL
           AND family_id IN (
               SELECT family_id FROM packages
               WHERE hub_category IS NOT NULL AND family_id IS NOT NULL
           )",
        [],
        |r| r.get(0),
    )?;
    println!(
        "    of which have NO prediction (the cheapest gap to close): {}",
        same_with_unpredicted
    );

    // Examples of the cheapest gap.
    println!();
    println!("    Examples of unlabeled+unpredicted siblings in labeled families:");
    let mut stmt = conn.prepare(
        "SELECT p.id, p.creator, p.package_name, p.version,
                (SELECT hub_category FROM packages p2
                 WHERE p2.family_id = p.family_id AND p2.hub_category IS NOT NULL
                 LIMIT 1) AS sibling_hub
         FROM packages p
         WHERE p.hub_category IS NULL
           AND p.predicted_hub_category IS NULL
           AND p.family_id IS NOT NULL
           AND p.family_id IN (
               SELECT family_id FROM packages
               WHERE hub_category IS NOT NULL AND family_id IS NOT NULL
           )
         ORDER BY p.creator, p.package_name, p.version
         LIMIT 20",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let creator: String = row.get(1)?;
        let pkg: String = row.get(2)?;
        let ver: String = row.get(3)?;
        let sib: Option<String> = row.get(4)?;
        println!(
            "      [{id}] {creator}.{pkg}.{ver}  → sibling hub: {}",
            sib.unwrap_or_else(|| "(none)".to_string())
        );
    }

    Ok(())
}

fn bucket_report(conn: &Connection, label: &str, where_clause: &str) -> Result<()> {
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM packages WHERE {where_clause}"),
        [],
        |r| r.get(0),
    )?;
    println!("--- {label} ---");
    println!("  count: {count}");

    let sql = format!(
        "SELECT id, creator, package_name, version, package_type,
                hub_category, predicted_hub_category, predicted_method,
                predicted_confidence
         FROM packages WHERE {where_clause}
         ORDER BY RANDOM() LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![SAMPLE_PER_BUCKET as i64])?;
    println!("  random examples:");
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let creator: String = row.get(1)?;
        let pkg: String = row.get(2)?;
        let ver: String = row.get(3)?;
        let scan_type: String = row.get(4)?;
        let hub: Option<String> = row.get(5)?;
        let pred: Option<String> = row.get(6)?;
        let method: Option<String> = row.get(7)?;
        let conf: Option<f64> = row.get(8)?;
        let label = hub
            .map(|h| format!("truth={h}"))
            .or_else(|| {
                pred.map(|p| {
                    format!(
                        "pred={p} (conf {:.2}, {})",
                        conf.unwrap_or(0.0),
                        method.unwrap_or_else(|| "?".to_string()),
                    )
                })
            })
            .unwrap_or_else(|| "(no label)".to_string());
        println!("    [{id}] {creator}.{pkg}.{ver}  scan='{scan_type}'  {label}");
    }
    println!();
    Ok(())
}

fn default_db_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata)
        .join("com.github.kylinblue.vam-package-browser")
        .join("index.sqlite")
}
