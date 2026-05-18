//! Taxonomy seeder. Reads tagging/taxonomy-vX.json off disk, inserts each
//! tag entry into the `taxonomy` table. Idempotent — uses INSERT OR IGNORE
//! so re-running can add tags introduced by a newer version JSON without
//! clobbering existing rows. Idempotent at the per-tag level: a tag already
//! in the table is never updated by this seeder (description/examples
//! changes should go through an explicit re-version step, not silent edits).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct TaxonomyFile {
    pub utility_plugins: Vec<TagEntry>,
    pub location_scenes: Vec<TagEntry>,
    pub speculative: Vec<SpeculativeEntry>,
}

#[derive(Debug, Deserialize)]
pub struct TagEntry {
    pub tag: String,
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct SpeculativeEntry {
    pub tag: String,
    pub category: String,
    pub description: String,
    #[serde(default)]
    pub reason_speculative: Option<String>,
}

#[derive(Debug)]
pub struct SeedStats {
    pub utility_added: usize,
    pub utility_existing: usize,
    pub location_added: usize,
    pub location_existing: usize,
    pub speculative_added: usize,
    pub speculative_existing: usize,
}

impl SeedStats {
    pub fn total_added(&self) -> usize {
        self.utility_added + self.location_added + self.speculative_added
    }
    pub fn total_existing(&self) -> usize {
        self.utility_existing + self.location_existing + self.speculative_existing
    }
}

/// Read a v3 taxonomy JSON file (flat utility_plugins/location_scenes/
/// speculative) and INSERT OR IGNORE each tag. For v4 (namespaced) use
/// `seed_v4_from_file`; the CLI dispatches by inspecting the file shape.
pub fn seed_from_file(
    conn: &Connection,
    path: &Path,
    version_label: &str,
) -> Result<SeedStats> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read taxonomy file {}", path.display()))?;
    let file: TaxonomyFile = serde_json::from_str(&content)
        .with_context(|| format!("parse taxonomy JSON {}", path.display()))?;
    seed_from_parsed(conn, &file, version_label)
}

pub fn seed_from_parsed(
    conn: &Connection,
    file: &TaxonomyFile,
    version_label: &str,
) -> Result<SeedStats> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut stmt = conn.prepare_cached(
        "INSERT OR IGNORE INTO taxonomy
            (tag, category, description, examples_json, state, reason_speculative,
             version_added, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;

    let mut stats = SeedStats {
        utility_added: 0,
        utility_existing: 0,
        location_added: 0,
        location_existing: 0,
        speculative_added: 0,
        speculative_existing: 0,
    };

    for entry in &file.utility_plugins {
        let examples_json = serde_json::to_string(&entry.examples)?;
        let inserted = stmt.execute(params![
            entry.tag,
            "utility_plugins",
            entry.description,
            examples_json,
            "confirmed",
            Option::<String>::None,
            version_label,
            now,
        ])?;
        if inserted == 1 {
            stats.utility_added += 1;
        } else {
            stats.utility_existing += 1;
        }
    }

    for entry in &file.location_scenes {
        let examples_json = serde_json::to_string(&entry.examples)?;
        let inserted = stmt.execute(params![
            entry.tag,
            "location_scenes",
            entry.description,
            examples_json,
            "confirmed",
            Option::<String>::None,
            version_label,
            now,
        ])?;
        if inserted == 1 {
            stats.location_added += 1;
        } else {
            stats.location_existing += 1;
        }
    }

    for entry in &file.speculative {
        if entry.category != "utility_plugins" && entry.category != "location_scenes" {
            return Err(anyhow!(
                "speculative entry {} has unknown category: {}",
                entry.tag,
                entry.category
            ));
        }
        let inserted = stmt.execute(params![
            entry.tag,
            entry.category,
            entry.description,
            "[]",
            "speculative",
            entry.reason_speculative,
            version_label,
            now,
        ])?;
        if inserted == 1 {
            stats.speculative_added += 1;
        } else {
            stats.speculative_existing += 1;
        }
    }

    Ok(stats)
}

// ============================== v4 (namespaced) ==============================

#[derive(Debug, Deserialize)]
pub struct V4TaxonomyFile {
    #[serde(default)]
    pub version: String,
    pub namespaces: BTreeMap<String, V4Namespace>,
}

#[derive(Debug, Deserialize)]
pub struct V4Namespace {
    pub applies_to: V4AppliesTo,
    pub cardinality: String,
    #[serde(default)]
    pub description: String,
    pub values: Vec<V4Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum V4AppliesTo {
    Single(String),
    Multiple(Vec<String>),
}

impl V4AppliesTo {
    fn to_json(&self) -> String {
        match self {
            V4AppliesTo::Single(s) => serde_json::to_string(s).unwrap_or_else(|_| "\"any\"".to_string()),
            V4AppliesTo::Multiple(v) => serde_json::to_string(v).unwrap_or_else(|_| "[]".to_string()),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct V4Value {
    pub value: String,
    pub description: String,
    #[serde(default)]
    pub examples: Vec<String>,
}

#[derive(Debug, Default)]
pub struct V4SeedStats {
    pub namespaces: usize,
    pub tags_added: usize,
    pub tags_reactivated: usize,
    pub tags_updated: usize,
    pub tags_deprecated: usize,
}

impl V4SeedStats {
    pub fn total_active(&self) -> usize {
        self.tags_added + self.tags_reactivated + self.tags_updated
    }
}

/// Seed taxonomy from a v4-format JSON. Inserts every (namespace, value) as
/// a row with `tag = namespace:value`. Any pre-existing taxonomy row whose
/// tag is NOT in the v4 file gets `is_active = 0` (deprecated). This is the
/// one-way migration path from v3's flat taxonomy to v4's namespaced one;
/// historic v3 rows persist for audit but stop appearing in active prompts.
pub fn seed_v4_from_parsed(
    conn: &Connection,
    file: &V4TaxonomyFile,
    version_label: &str,
) -> Result<V4SeedStats> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let mut stats = V4SeedStats::default();

    // Build the active-tag set so we can deprecate everything else.
    let mut active_tags: Vec<(String, String, String, String, String, String)> = Vec::new();
    // (tag, namespace, applies_to_json, cardinality, description, examples_json)
    for (ns_name, ns) in &file.namespaces {
        for value in &ns.values {
            let tag = format!("{ns_name}:{}", value.value);
            let examples_json = serde_json::to_string(&value.examples)?;
            active_tags.push((
                tag,
                ns_name.clone(),
                ns.applies_to.to_json(),
                ns.cardinality.clone(),
                value.description.clone(),
                examples_json,
            ));
        }
        stats.namespaces += 1;
    }

    let tx = conn.unchecked_transaction()?;

    // 1. Deprecate all currently-active rows; we'll reactivate matches below.
    let deprecated = tx.execute(
        "UPDATE taxonomy SET is_active = 0 WHERE is_active = 1",
        [],
    )?;
    stats.tags_deprecated = deprecated;

    // 2. Upsert v4 tags (insert if new, update metadata + reactivate if exists).
    let mut ins = tx.prepare_cached(
        "INSERT INTO taxonomy
            (tag, category, description, examples_json, state, reason_speculative,
             version_added, created_at, namespace, applies_to_json, cardinality, is_active)
         VALUES (?1, ?2, ?3, ?4, 'confirmed', NULL, ?5, ?6, ?7, ?8, ?9, 1)
         ON CONFLICT(tag) DO UPDATE SET
            description = excluded.description,
            examples_json = excluded.examples_json,
            namespace = excluded.namespace,
            applies_to_json = excluded.applies_to_json,
            cardinality = excluded.cardinality,
            is_active = 1",
    )?;

    let mut existed_stmt = tx.prepare_cached(
        "SELECT is_active FROM taxonomy WHERE tag = ?1",
    )?;

    for (tag, namespace, applies_to_json, cardinality, description, examples_json) in &active_tags
    {
        // Track whether the row exists pre-upsert (and its active state) so
        // we can categorize the change: added vs reactivated vs updated.
        let prior: Option<i64> = existed_stmt
            .query_row(params![tag], |r| r.get(0))
            .ok();
        ins.execute(params![
            tag,
            namespace,           // category column doubles as namespace for compat
            description,
            examples_json,
            version_label,
            now,
            namespace,
            applies_to_json,
            cardinality,
        ])?;
        match prior {
            None => stats.tags_added += 1,
            Some(0) => stats.tags_reactivated += 1,
            Some(_) => stats.tags_updated += 1,
        }
    }
    drop(ins);
    drop(existed_stmt);

    // Adjust deprecated count: we counted ALL active rows as deprecated above,
    // but reactivated + updated entries were re-set to 1. Subtract those.
    stats.tags_deprecated = stats
        .tags_deprecated
        .saturating_sub(stats.tags_reactivated + stats.tags_updated);

    tx.commit()?;
    Ok(stats)
}

pub fn seed_v4_from_file(
    conn: &Connection,
    path: &Path,
    version_label: &str,
) -> Result<V4SeedStats> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read v4 taxonomy file {}", path.display()))?;
    let file: V4TaxonomyFile = serde_json::from_str(&content)
        .with_context(|| format!("parse v4 taxonomy {}", path.display()))?;
    seed_v4_from_parsed(conn, &file, version_label)
}
