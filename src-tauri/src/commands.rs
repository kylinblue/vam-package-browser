use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::prelude::*;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::embedding::{self, InputKind, ModelChoice};
use crate::{hub, index, scanner, thumbnails, AppState};

/// Default model/input variant for semantic search. Per the embedding
/// handoff doc, nomic + purpose has the best recall on natural-language
/// queries in our corpus. Switch here if the UI ever exposes variant choice.
const SEARCH_MODEL: ModelChoice = ModelChoice::NomicEmbedTextV15;
const SEARCH_INPUT: InputKind = InputKind::Purpose;

const SETTING_ADDON_ROOT: &str = "addon_root";

/// Correlated subquery returning a `\u{1f}`-separated list of family-level tag
/// assignments for the current packages row, or NULL when the package has no
/// family or no tags. The separator is ASCII 0x1F (Unit Separator) — outside
/// the printable range so it can't collide with any tag value. Decoded into
/// `Vec<String>` by `split_tags`.
const TAGS_SUBQUERY: &str = "(SELECT group_concat(ft.tag, char(31)) \
                              FROM family_tags ft \
                              WHERE ft.family_id = packages.family_id)";

fn split_tags(blob: Option<String>) -> Vec<String> {
    match blob {
        None => Vec::new(),
        Some(s) if s.is_empty() => Vec::new(),
        Some(s) => s.split('\u{1F}').map(|t| t.to_string()).collect(),
    }
}

#[derive(Debug, Serialize)]
pub struct PackageRow {
    pub id: i64,
    pub creator: String,
    pub package_name: String,
    pub version: String,
    pub license: Option<String>,
    pub program_version: Option<String>,
    pub description: Option<String>,
    pub package_type: String,
    pub content_count: i64,
    pub dep_count: i64,
    pub file_size: i64,
    pub file_mtime: i64,
    pub package_mtime: i64,
    pub var_path: String,
    pub has_preview: bool,
    pub is_favorite: bool,
    pub is_hidden: bool,
    pub hub_resource_id: Option<i64>,
    pub hub_url: Option<String>,
    pub hub_title: Option<String>,
    pub hub_author: Option<String>,
    pub hub_category: Option<String>,
    pub hub_preview_url: Option<String>,
    pub hub_synced_at: Option<i64>,
    pub hub_sync_state: Option<String>,
    pub scene_count: i64,
    pub look_count: i64,
    pub plugin_count: i64,
    pub clothing_count: i64,
    pub hair_count: i64,
    pub pose_count: i64,
    pub subscene_count: i64,
    pub error: Option<String>,
    /// Family-level v4 tags (namespaced strings like "kind:character-look").
    /// Empty when the package has no family_id (unscanned legacy rows) or the
    /// family has no tag assignments yet. Joined in via a correlated subquery
    /// on packages.family_id, so this is cheap and avoids an extra round-trip.
    pub tags: Vec<String>,
    /// Hub v14/v15 fields. NULL until a hub sync has resolved this package.
    pub hub_billing_tier: Option<String>,
    pub hub_is_hub_hosted: Option<i64>,
    pub hub_license: Option<String>,
    pub hub_lastmod: Option<i64>,
    pub hub_external_url: Option<String>,
    pub hub_match_method: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct QueryFilter {
    pub search: Option<String>,
    pub creator: Option<String>,
    pub package_type: Option<String>,
    /// When true, only packages with `has_preview = 0` are returned.
    #[serde(default)]
    pub missing_preview: bool,
    /// When true, only packages with `is_favorite = 1` are returned.
    #[serde(default)]
    pub favorites_only: bool,
    /// When false (default), packages with `is_hidden = 1` are excluded.
    #[serde(default)]
    pub include_hidden: bool,
    /// File size bounds in bytes (inclusive). Both sides optional.
    pub min_size: Option<i64>,
    pub max_size: Option<i64>,
    /// File mtime bounds as unix seconds (inclusive). Both sides optional.
    pub min_mtime: Option<i64>,
    pub max_mtime: Option<i64>,
    /// Package mtime bounds as unix seconds (inclusive). Filters on the
    /// max-entry-mtime captured from inside the .var (when the author zipped it).
    pub min_package_mtime: Option<i64>,
    pub max_package_mtime: Option<i64>,
    /// Sort field: "name" | "creator" | "size" | "mtime" | "package_mtime" | "scanned".
    /// Defaults to "creator". Unknown values fall back to default.
    pub sort_by: Option<String>,
    /// Sort order: "asc" | "desc". Defaults to "asc" for name/creator,
    /// "desc" for size/mtime/package_mtime/scanned.
    pub sort_order: Option<String>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
    /// Family-level v4 tag filter. Tags are namespaced strings
    /// (e.g. "kind:character-look", "body:curvy"). Semantics:
    ///   - Tags sharing a namespace are OR'd (union within a facet column).
    ///   - Tags across namespaces are AND'd (intersection — must match every
    ///     selected facet column).
    /// Packages without a family_id can never match a non-empty tag filter.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Hub category filter (Fetched mode's primary axis). Matches exactly
    /// (case-sensitive — hub categories are canonical display strings like
    /// "Looks", "Plugins + Scripts").
    pub hub_category: Option<String>,
    /// When true, restrict to packages with no hub category (= not currently
    /// matched). Acts as a "virtual" chip alongside hub_category so the user
    /// can browse the residual that the sync didn't resolve. Mutually
    /// exclusive with hub_category — if both are set, hub_category wins
    /// (real category implies matched, so unmatched=true contradicts it).
    #[serde(default)]
    pub hub_unmatched: bool,
}

#[tauri::command]
pub async fn scan_library(
    state: State<'_, AppState>,
    addon_root: String,
    limit: Option<usize>,
) -> Result<scanner::ScanResult, String> {
    let path = PathBuf::from(&addon_root);
    let db = state.db.clone();
    let thumbs_dir = state.thumbs_dir();
    tauri::async_runtime::spawn_blocking(move || {
        let mut conn = db.lock();
        index::set_setting(&conn, SETTING_ADDON_ROOT, &addon_root)
            .map_err(|e| format!("save setting: {e:#}"))?;
        scanner::scan(&mut conn, &path, &thumbs_dir, limit)
            .map_err(|e| format!("scan failed: {e:#}"))
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[tauri::command]
pub fn query_packages(
    state: State<'_, AppState>,
    filter: QueryFilter,
) -> Result<Vec<PackageRow>, String> {
    let conn = state.db.lock();
    let (where_clause, binds) = build_where(&filter);
    let limit = filter.limit.unwrap_or(2000).clamp(1, 50_000);
    let offset = filter.offset.unwrap_or(0).max(0);
    let order_clause = build_order_clause(&filter);
    let sql = format!(
        "SELECT id, creator, package_name, version, license, program_version, description,
                package_type, content_count, dep_count, file_size, file_mtime, var_path,
                has_preview, is_favorite, is_hidden,
                hub_resource_id, hub_url, hub_title, hub_author, hub_category,
                hub_preview_url, hub_synced_at, hub_sync_state,
                scene_count, look_count, plugin_count, clothing_count,
                hair_count, pose_count, subscene_count,
                error, package_mtime,
                hub_billing_tier, hub_is_hub_hosted, hub_license,
                hub_lastmod, hub_external_url, hub_match_method,
                {TAGS_SUBQUERY} AS tags
         FROM packages
         {where_clause}
         {order_clause}
         LIMIT ?{lp} OFFSET ?{op}",
        lp = binds.len() + 1,
        op = binds.len() + 2,
    );

    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let mut all_binds = binds;
    all_binds.push(rusqlite::types::Value::Integer(limit));
    all_binds.push(rusqlite::types::Value::Integer(offset));
    let params_ref: Vec<&dyn rusqlite::ToSql> =
        all_binds.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    let rows = stmt
        .query_map(params_ref.as_slice(), |row| {
            Ok(PackageRow {
                id: row.get(0)?,
                creator: row.get(1)?,
                package_name: row.get(2)?,
                version: row.get(3)?,
                license: row.get(4)?,
                program_version: row.get(5)?,
                description: row.get(6)?,
                package_type: row.get(7)?,
                content_count: row.get(8)?,
                dep_count: row.get(9)?,
                file_size: row.get(10)?,
                file_mtime: row.get(11)?,
                var_path: row.get(12)?,
                has_preview: row.get::<_, i64>(13)? != 0,
                is_favorite: row.get::<_, i64>(14)? != 0,
                is_hidden: row.get::<_, i64>(15)? != 0,
                hub_resource_id: row.get(16)?,
                hub_url: row.get(17)?,
                hub_title: row.get(18)?,
                hub_author: row.get(19)?,
                hub_category: row.get(20)?,
                hub_preview_url: row.get(21)?,
                hub_synced_at: row.get(22)?,
                hub_sync_state: row.get(23)?,
                scene_count: row.get(24)?,
                look_count: row.get(25)?,
                plugin_count: row.get(26)?,
                clothing_count: row.get(27)?,
                hair_count: row.get(28)?,
                pose_count: row.get(29)?,
                subscene_count: row.get(30)?,
                error: row.get(31)?,
                package_mtime: row.get(32)?,
                hub_billing_tier: row.get(33)?,
                hub_is_hub_hosted: row.get(34)?,
                hub_license: row.get(35)?,
                hub_lastmod: row.get(36)?,
                hub_external_url: row.get(37)?,
                hub_match_method: row.get(38)?,
                tags: split_tags(row.get::<_, Option<String>>(39)?),
            })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

#[tauri::command]
pub fn count_packages(
    state: State<'_, AppState>,
    filter: QueryFilter,
) -> Result<i64, String> {
    let conn = state.db.lock();
    let (where_clause, binds) = build_where(&filter);
    let sql = format!("SELECT COUNT(*) FROM packages {where_clause}");
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let ref_vec: Vec<&dyn rusqlite::ToSql> =
        binds.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
    let n: i64 = stmt
        .query_row(ref_vec.as_slice(), |row| row.get(0))
        .map_err(map_err)?;
    Ok(n)
}

#[tauri::command]
pub fn list_creators(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let conn = state.db.lock();
    let mut stmt = conn
        .prepare("SELECT DISTINCT creator FROM packages WHERE creator <> '' ORDER BY creator COLLATE NOCASE")
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

#[derive(Debug, Serialize)]
pub struct CreatorCount {
    pub creator: String,
    pub count: i64,
}

/// Creators with per-creator package counts (hidden excluded). Used by the
/// custom author picker dropdown.
#[tauri::command]
pub fn list_creators_with_counts(
    state: State<'_, AppState>,
) -> Result<Vec<CreatorCount>, String> {
    let conn = state.db.lock();
    let mut stmt = conn
        .prepare(
            "SELECT creator, COUNT(*)
             FROM packages
             WHERE creator <> '' AND is_hidden = 0
             GROUP BY creator COLLATE NOCASE
             ORDER BY creator COLLATE NOCASE",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(CreatorCount {
                creator: r.get(0)?,
                count: r.get(1)?,
            })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

/// Open a URL in the OS default browser. Windows-only for now via the cmd
/// `start ""` idiom (the empty-string arg is the window title — `start`'s
/// first quoted positional, otherwise it would consume the URL).
#[tauri::command]
pub fn open_external_url(url: String) -> Result<(), String> {
    // Cheap allow-list — refuse anything that doesn't look like a URL we
    // intend to open, so a future bug can't trick this into running shell.
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("refused non-http url: {url}"));
    }
    // Defensive: a `"` in the URL would break the quoting below and could
    // let extra cmd tokens through. Hub URLs never contain one.
    if url.contains('"') {
        return Err(format!("refused url containing quote: {url}"));
    }
    #[cfg(target_os = "windows")]
    {
        // Use raw_arg so we control the exact cmdline. The URL MUST be
        // wrapped in literal double quotes — otherwise cmd parses `&` in
        // query strings (e.g. `?q=foo&t=resource`) as command separators
        // and tries to execute `t=resource` as a command.
        use std::os::windows::process::CommandExt;
        std::process::Command::new("cmd")
            .raw_arg("/c")
            .raw_arg("start")
            .raw_arg("\"\"")
            .raw_arg(format!("\"{url}\""))
            .spawn()
            .map_err(|e| format!("spawn cmd start: {e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = url;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Settings {
    pub addon_root: Option<String>,
}

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings, String> {
    let conn = state.db.lock();
    Ok(Settings {
        addon_root: index::get_setting(&conn, SETTING_ADDON_ROOT).map_err(map_err)?,
    })
}

#[tauri::command]
pub fn set_addon_root(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let conn = state.db.lock();
    index::set_setting(&conn, SETTING_ADDON_ROOT, &path).map_err(map_err)?;
    Ok(())
}

#[tauri::command]
pub fn set_favorite(
    state: State<'_, AppState>,
    id: i64,
    value: bool,
) -> Result<(), String> {
    let conn = state.db.lock();
    conn.execute(
        "UPDATE packages SET is_favorite = ?1 WHERE id = ?2",
        params![value as i64, id],
    )
    .map_err(map_err)?;
    Ok(())
}

#[tauri::command]
pub fn set_hidden(
    state: State<'_, AppState>,
    id: i64,
    value: bool,
) -> Result<(), String> {
    let conn = state.db.lock();
    conn.execute(
        "UPDATE packages SET is_hidden = ?1 WHERE id = ?2",
        params![value as i64, id],
    )
    .map_err(map_err)?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct ImageEntry {
    pub path: String,
    pub size: i64,
}

#[derive(Debug, Serialize)]
pub struct PackageDetail {
    pub package: PackageRow,
    /// Full meta.json contentList (file paths inside the .var).
    pub content_list: Vec<String>,
    /// Top-level dependency keys (`Author.Package.Version|latest`). Recursive
    /// dependency trees aren't resolved here.
    pub dependencies: Vec<String>,
    /// Author-provided usage notes from meta.json (`instructions`). Pulled live
    /// from the .var each detail open rather than indexed, since only the
    /// detail view consumes it.
    pub instructions: Option<String>,
    /// Every image entry in the .var archive (any path, any extension matching
    /// jpg/jpeg/png), sorted by path.
    pub images: Vec<ImageEntry>,
    pub preview_path: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct HubSyncOptions {
    /// If set, sync only packages whose creator matches (case-insensitive).
    pub creator: Option<String>,
    /// If set, sync only packages whose package_name matches (case-insensitive).
    pub package_name: Option<String>,
    /// If true (default), skip packages already synced.
    #[serde(default = "default_true")]
    pub only_missing: bool,
    /// If true (default), for packages whose local picker found nothing, also
    /// download the hub preview icon and turn it into our primary thumbnail.
    #[serde(default = "default_true")]
    pub pull_preview_for_no_thumb: bool,
    /// Milliseconds between hub requests *per worker*. Default 700 — Cowork
    /// recon 2026-05-17 confirmed sub-second loads with no 429/503, and the
    /// hub publishes a sitemap (explicit "scrape me" invitation). At
    /// `hub_sync_workers = 3`, this is roughly 4.3 req/s aggregate. If a
    /// rate-limit hits, bump this back up.
    #[serde(default = "default_rate")]
    pub rate_limit_ms: u64,
    /// Number of creator-level workers running in parallel. Default 3.
    /// Each worker walks one creator's Phase B + B2 end-to-end. The full-
    /// catalog refresh in Phase A is sequential (only 8 fetches anyway).
    #[serde(default = "default_workers")]
    pub workers: usize,
}
fn default_true() -> bool { true }
fn default_rate() -> u64 { 700 }
fn default_workers() -> usize { 3 }

#[derive(Debug, Serialize, Clone)]
pub struct HubSyncProgress {
    pub done: usize,
    pub total: usize,
    pub matched: usize,
    pub not_found: usize,
    pub failed: usize,
    pub previews_pulled: usize,
    pub current: String,
    pub current_status: String, // "matched" | "not_found" | "failed" | "gate"
}

#[derive(Debug, Serialize)]
pub struct HubSyncSummary {
    pub considered: usize,
    pub matched: usize,
    pub not_found: usize,
    pub failed: usize,
    pub previews_pulled: usize,
    pub elapsed_ms: u128,
    pub gated: bool,
}

#[derive(Debug, Serialize)]
pub struct HubCatalogRefreshSummary {
    /// Number of resource entries pulled from the sitemap (sum across the 7
    /// child sitemaps after filtering to /resources/{slug}.{id}/ URLs).
    pub total_fetched: usize,
    pub elapsed_ms: u128,
}

/// Fetch /sitemap.xml + 7 child sitemaps, parse every <url> entry whose
/// <loc> is a resource page, and upsert `(resource_id, slug, lastmod)` into
/// the `hub_resources` table. Subsequent calls overwrite — `fetched_at` is
/// always bumped to "now", and stale entries (resources removed from the
/// hub) survive harmlessly; sync logic ignores them.
///
/// ~8 HTTP requests total. Cheap and idempotent.
#[tauri::command]
pub async fn hub_catalog_refresh(
    state: State<'_, AppState>,
) -> Result<HubCatalogRefreshSummary, String> {
    let db = state.db.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let start = Instant::now();
        let client = hub::HubClient::new();
        let entries = client
            .fetch_sitemap_catalog()
            .map_err(|e| format!("sitemap fetch: {e:#}"))?;
        let total = entries.len();
        let now = unix_now();

        let mut conn = db.lock();
        let tx = conn.transaction().map_err(map_err)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO hub_resources (resource_id, slug, lastmod, fetched_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(resource_id) DO UPDATE SET
                       slug = excluded.slug,
                       lastmod = excluded.lastmod,
                       fetched_at = excluded.fetched_at",
                )
                .map_err(map_err)?;
            for entry in entries {
                stmt.execute(params![
                    entry.resource_id,
                    entry.slug,
                    entry.lastmod,
                    now,
                ])
                .map_err(map_err)?;
            }
        }
        tx.commit().map_err(map_err)?;

        Ok(HubCatalogRefreshSummary {
            total_fetched: total,
            elapsed_ms: start.elapsed().as_millis(),
        })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Local package candidate captured under one DB lock at the start of sync.
struct LocalPkg {
    id: i64,
    creator: String,
    package_name: String,
    has_preview: bool,
}

/// Parse a .var filename `Creator.PackageName.Version.var` (with `Version`
/// being an integer or non-empty string) into its three parts. Returns
/// `None` on malformed names. The split is on the LAST two dots, so package
/// names containing dots survive (rare but possible).
fn parse_var_filename(name: &str) -> Option<(String, String, String)> {
    let stem = name.strip_suffix(".var").unwrap_or(name);
    // rsplitn(3, '.') yields up to 3 segments from the right: [version,
    // package, creator]. Anything before is concatenated back into "creator"
    // — but in practice creator never has dots.
    let mut parts = stem.rsplitn(3, '.');
    let version = parts.next()?.to_string();
    let package = parts.next()?.to_string();
    let creator = parts.next()?.to_string();
    if creator.is_empty() || package.is_empty() || version.is_empty() {
        return None;
    }
    Some((creator, package, version))
}

/// Normalize a `(creator, package_name)` pair into a single string key for
/// HashMap lookups. Case-insensitive on both sides; whitespace and most
/// punctuation are NOT stripped because the .var filename is the canonical
/// form on both ends (the user's local file and the hub's published file
/// should use the same characters).
fn norm_key(creator: &str, package: &str) -> String {
    format!("{}|{}", creator.to_lowercase(), package.to_lowercase())
}

/// Per-creator: run one search, HEAD-probe every result row, build the
/// canonical lookup map, and persist per-local-package matches. Returns
/// running counters that the caller adds to the global progress.
///
/// Aborts the entire sync (returns `true`) if the hub gate is hit.
#[allow(clippy::too_many_arguments)]
/// Skip the per-creator broad search when a creator has very few local
/// packages. For L=1 or L=2 the broad search amortizes poorly — a prolific
/// hub author can have 50+ hub resources, and HEADing all of them to
/// match 1-2 locals wastes time vs. just running targeted keyword
/// searches per local. Break-even is roughly H/L > 6, but L≤2 is a
/// conservative heuristic that covers the dominant long-tail bad case
/// without misjudging medium creators.
const L_BROADCAST_SHORTCUT: usize = 2;

/// Look up resources in the cached `hub_resources` sitemap catalog whose
/// normalized slug matches any of `locals`' normalized package names,
/// skipping resources already covered by `excluded_ids` (typically the XF
/// search result set so we don't double-fetch).
///
/// Why this exists: XF's per-author search returns its own ranked listing
/// that buries older resources in deep pagination tails. Resources we
/// know about via the sitemap can be `not_found` after the regular sync
/// purely because the search didn't surface them — not because they
/// aren't matchable. The slug-match tier hits the resource page directly
/// (one GET per candidate) and adds it to the same HEAD-probe pipeline
/// the broad search feeds, so verification (CDN filename must equal
/// `Creator.Package.Version.var`) is identical.
///
/// Only **unique** slug → package-name matches are returned. If two
/// hub resources normalize to the same slug we treat it as ambiguous
/// and skip both — caller can fall back to the existing search-based
/// flow without a wrong-author false positive.
///
/// Errors fetching a resource page (network / parse) are logged but
/// non-fatal; the function keeps going so one bad slug doesn't stall a
/// whole creator.
fn slug_cache_extras_for_creator(
    creator: &str,
    locals: &[LocalPkg],
    client: &hub::HubClient,
    db: &Arc<parking_lot::Mutex<rusqlite::Connection>>,
    excluded_ids: &std::collections::HashSet<i64>,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    rate_limit_ms: u64,
) -> Vec<hub::HubMatch> {
    use std::collections::{HashMap, HashSet};

    // Normalize-pkg → list of locals using that name. (Locals are already
    // grouped by creator at the caller, so name collisions are the only
    // dedup work here.)
    let mut by_norm: HashMap<String, ()> = HashMap::new();
    for l in locals {
        by_norm.insert(hub::normalize_compare(&l.package_name), ());
    }
    if by_norm.is_empty() {
        return Vec::new();
    }

    // Scan the cached catalog for unique-slug hits. Bounded by hub_resources
    // table size (~45k rows) -- fast enough at per-creator granularity, and
    // avoids the complexity of a globally-shared slug index.
    let candidates: Vec<(i64, String)> = {
        let conn = db.lock();
        let Ok(mut stmt) = conn.prepare("SELECT resource_id, slug FROM hub_resources") else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        }) else {
            return Vec::new();
        };
        let mut hits: HashMap<String, Vec<(i64, String)>> = HashMap::new();
        for row in rows.flatten() {
            let norm = hub::normalize_compare(&row.1);
            if by_norm.contains_key(&norm) {
                hits.entry(norm).or_default().push(row);
            }
        }
        // Keep only norms that mapped to exactly one slug; drop ambiguous.
        hits.into_iter()
            .filter_map(|(_k, v)| {
                if v.len() == 1 {
                    Some(v.into_iter().next().unwrap())
                } else {
                    None
                }
            })
            .filter(|(id, _)| !excluded_ids.contains(id))
            .collect()
    };

    if candidates.is_empty() {
        return Vec::new();
    }

    let mut seen_ids: HashSet<i64> = HashSet::new();
    let mut out = Vec::with_capacity(candidates.len());
    for (id, slug) in candidates {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !seen_ids.insert(id) {
            continue;
        }
        match client.fetch_resource_page(&slug, id) {
            Ok(hm) => out.push(hm),
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.starts_with("gate:") {
                    eprintln!("hub sync slug-cache: gated on {slug}.{id} — aborting");
                    return out;
                }
                eprintln!(
                    "hub sync slug-cache: fetch_resource_page failed for {slug}.{id} (creator {creator}): {msg}"
                );
            }
        }
        sleep_with_cancel(rate_limit_ms, cancel);
    }
    out
}

fn sync_one_creator(
    creator: &str,
    locals: &[LocalPkg],
    client: &hub::HubClient,
    db: &Arc<parking_lot::Mutex<rusqlite::Connection>>,
    thumbs_dir: &std::path::Path,
    app: &AppHandle,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    rate_limit_ms: u64,
    pull_preview_for_no_thumb: bool,
    counters: &Arc<parking_lot::Mutex<SyncCounters>>,
) -> bool {
    // L≤2 shortcut: skip the broad per-creator search and route every
    // local straight to the per-package keyword search. The targeted
    // search is much cheaper per local — and for prolific hub authors
    // where the user only owns 1-2 packages, the broad search would HEAD
    // dozens of irrelevant resources before matching ours.
    if locals.len() <= L_BROADCAST_SHORTCUT {
        emit_log(
            app,
            "info",
            format!("▶ {creator}: L={} ≤ {L_BROADCAST_SHORTCUT}, using per-package shortcut", locals.len()),
        );
        let mut b2_cache: std::collections::HashMap<(String, String), Vec<hub::HubMatch>> =
            std::collections::HashMap::new();
        for local in locals {
            if cancel.load(Ordering::Relaxed) || counters.lock().gated {
                break;
            }
            let abort = retry_one_keyword(
                local, client, db, app, cancel, rate_limit_ms, counters, &mut b2_cache,
            );
            if abort {
                return true;
            }
        }
        // Optional preview pull: still useful for small-L creators. Skipped
        // here because retry_one_keyword's hits aren't retained — preview
        // URLs require either holding onto them per local (more state) or
        // re-fetching. For consistency with the deferred manual-paste UX,
        // we defer preview pull entirely on shortcut path. Users who care
        // can run a separate sync pass with the L>2 path via creator filter.
        let _ = thumbs_dir;
        let _ = pull_preview_for_no_thumb;
        return false;
    }

    emit_log(
        app,
        "info",
        format!("▶ {creator}: searching ({} local)…", locals.len()),
    );
    let mut hub_resources = match client.search_resources_by_user(creator) {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.starts_with("gate:") {
                emit_log(
                    app,
                    "error",
                    format!("✕ {creator}: hit hub gate — aborting sync"),
                );
                counters.lock().gated = true;
                return true;
            }
            emit_log(
                app,
                "error",
                format!("✕ {creator}: search failed — {msg}"),
            );
            mark_local_failed(db, locals, "failed");
            {
                let mut c = counters.lock();
                c.failed += locals.len();
                c.done += locals.len();
            }
            emit_progress(app, counters, creator);
            sleep_with_cancel(rate_limit_ms, cancel);
            return false;
        }
    };
    emit_log(
        app,
        "info",
        format!("  {creator}: {} hub resources found", hub_resources.len()),
    );
    sleep_with_cancel(rate_limit_ms, cancel);
    if cancel.load(Ordering::Relaxed) {
        return false;
    }

    // Slug-cache augmentation: pull in resources whose cached sitemap slug
    // normalizes to one of this creator's local package names but which the
    // XF per-author search didn't return (search-tail issue). Each extra
    // candidate goes through the SAME HEAD-probe verification path below,
    // so a wrong-author slug collision (e.g. local "Eloise" matching some
    // other creator's "eloise" resource) is rejected when the CDN filename
    // doesn't carry the expected Creator.Package prefix.
    let xf_ids: std::collections::HashSet<i64> =
        hub_resources.iter().map(|hr| hr.resource_id).collect();
    let slug_extras = slug_cache_extras_for_creator(
        creator,
        locals,
        client,
        db,
        &xf_ids,
        cancel,
        rate_limit_ms,
    );
    let slug_match_ids: std::collections::HashSet<i64> =
        slug_extras.iter().map(|hr| hr.resource_id).collect();
    if !slug_extras.is_empty() {
        emit_log(
            app,
            "info",
            format!(
                "  {creator}: +{} slug-cache candidate(s) (XF search missed)",
                slug_extras.len()
            ),
        );
    }
    hub_resources.extend(slug_extras);

    // Build the canonical-filename map (hub-hosted) and a fuzzy fallback set
    // (paid). For each hub resource:
    //   - hub-hosted: HEAD the download URL, read Content-Disposition, parse
    //     Creator.Package.Version.var → key the resource by (creator, package).
    //   - paid: HEAD captures the offsite 301 Location, we keep the resource
    //     in a list for fuzzy-title match against any unmatched local pkg.
    let mut filename_map: std::collections::HashMap<String, (hub::HubMatch, Option<String>)> =
        std::collections::HashMap::new();
    // (hub_match, external_url) for paid candidates.
    let mut paid_fallback: Vec<(hub::HubMatch, Option<String>)> = Vec::new();

    for hr in hub_resources {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let Some((slug, _id)) = hub::extract_slug_and_id_from_url(&hr.url) else {
            continue;
        };

        let probe = match client.head_download(&slug, hr.resource_id) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.starts_with("gate:") {
                    counters.lock().gated = true;
                    return true;
                }
                eprintln!(
                    "HEAD probe failed for {}/{}: {msg}",
                    hr.title, hr.resource_id
                );
                sleep_with_cancel(rate_limit_ms, cancel);
                continue;
            }
        };
        sleep_with_cancel(rate_limit_ms, cancel);

        match probe {
            hub::DownloadProbe::Hosted { filename } => {
                if let Some((c, p, _v)) = parse_var_filename(&filename) {
                    let key = norm_key(&c, &p);
                    filename_map.insert(key, (hr, None));
                } else {
                    eprintln!(
                        "unparseable .var filename from CDN: {filename} (res {})",
                        hr.resource_id
                    );
                }
            }
            hub::DownloadProbe::Offsite { url } => {
                paid_fallback.push((hr, Some(url)));
            }
            hub::DownloadProbe::NotFound => {
                // Resource removed since sitemap snapshot. Skip.
            }
        }
    }

    // Pair each local package with a hub resource. Layer 0 wins by exact
    // (creator, package_name) key; Layer 1 (paid fuzzy_title) takes anything
    // unmatched. Both layers persist the FULL HubMatch fields plus the
    // new ones (billing_tier, is_hub_hosted, license, external_url, etc.).
    let now = unix_now();
    let mut conn_for_writes = db.lock();
    let tx = match conn_for_writes.transaction() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("hub sync tx open failed: {e}");
            let mut c = counters.lock();
            c.failed += locals.len();
            c.done += locals.len();
            return false;
        }
    };

    // Indices into `locals` of packages that ended up not_found after the
    // per-creator pass. Phase B2 retries these inline below so each
    // creator's worker is fully self-contained.
    let mut unmatched_in_b1: Vec<usize> = Vec::new();

    for (idx, local) in locals.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let key = norm_key(&local.creator, &local.package_name);
        let (state_str, method, hub_match, external_url): (
            &str,
            Option<&str>,
            Option<&hub::HubMatch>,
            Option<&str>,
        ) = if let Some((hm, _)) = filename_map.get(&key) {
            // Filename-tier match: but if the candidate originated from the
            // slug-cache augmentation (XF search didn't return it), tag it
            // as `slug_match` so we can measure recovery from that tier
            // separately in downstream histograms.
            let m = if slug_match_ids.contains(&hm.resource_id) {
                "slug_match"
            } else {
                "filename"
            };
            ("matched", Some(m), Some(hm), None)
        } else {
            // Fuzzy fallback over paid candidates.
            let best = paid_fallback
                .iter()
                .map(|(hm, ex)| {
                    (
                        hub::score_match(&local.creator, &local.package_name, hm),
                        hm,
                        ex.as_deref(),
                    )
                })
                .filter(|(s, _, _)| *s > 0)
                .max_by_key(|(s, _, _)| *s);
            match best {
                Some((_, hm, ex)) => ("matched", Some("fuzzy_title"), Some(hm), ex),
                None => ("not_found", None, None, None),
            }
        };

        // hub_lastmod comes from the sitemap; cheap join.
        let hub_lastmod: Option<i64> = match hub_match {
            Some(hm) => tx
                .query_row(
                    "SELECT lastmod FROM hub_resources WHERE resource_id = ?1",
                    params![hm.resource_id],
                    |r| r.get::<_, i64>(0),
                )
                .ok(),
            None => None,
        };

        let r = tx.execute(
            "UPDATE packages SET
               hub_resource_id    = ?2,
               hub_url            = ?3,
               hub_title          = ?4,
               hub_author         = ?5,
               hub_category       = ?6,
               hub_preview_url    = ?7,
               hub_synced_at      = ?8,
               hub_sync_state     = ?9,
               hub_billing_tier   = ?10,
               hub_is_hub_hosted  = ?11,
               hub_license        = ?12,
               hub_lastmod        = ?13,
               hub_external_url   = ?14,
               hub_match_method   = ?15
             WHERE id = ?1",
            params![
                local.id,
                hub_match.map(|b| b.resource_id),
                hub_match.map(|b| b.url.as_str()),
                hub_match.map(|b| b.title.as_str()),
                hub_match.map(|b| b.author.as_str()),
                hub_match.and_then(|b| b.category.as_deref()),
                hub_match.and_then(|b| b.preview_url.as_deref()),
                now,
                state_str,
                hub_match.and_then(|b| b.billing_tier.as_deref()),
                hub_match.map(|b| if b.is_hub_hosted { 1 } else { 0 }),
                hub_match.and_then(|b| b.license.as_deref()),
                hub_lastmod,
                external_url,
                method,
            ],
        );
        // Counter accuracy + progress accuracy:
        //   - "matched" is terminal → counters.matched++, done++
        //   - "not_found" is NOT terminal — Phase B2 will retry below.
        //     Track it (counters.not_found++) but defer the done++ until
        //     retry_one_keyword finishes, otherwise the progress bar maxes
        //     out at the end of B1 while B2 silently grinds.
        //   - SQL error is terminal → counters.failed++, done++
        match r {
            Ok(_) => {
                let mut c = counters.lock();
                match state_str {
                    "matched" => {
                        c.matched += 1;
                        c.done += 1;
                    }
                    _ => {
                        c.not_found += 1;
                        unmatched_in_b1.push(idx);
                        // No done++ — B2 will finish accounting for this row.
                    }
                }
            }
            Err(e) => {
                eprintln!("hub sync DB update failed for id {}: {e}", local.id);
                let mut c = counters.lock();
                c.failed += 1;
                c.done += 1;
            }
        }
        emit_progress(app, counters, &format!("{}.{}", local.creator, local.package_name));
    }
    if let Err(e) = tx.commit() {
        eprintln!("hub sync tx commit failed for creator {creator}: {e}");
    }
    drop(conn_for_writes);

    // Optional preview pull pass for newly-matched packages that still have
    // no local thumbnail. Done after the tx commits to avoid holding the
    // write lock across HTTP. Each pull is rate-limited like any other hub
    // request.
    if pull_preview_for_no_thumb {
        for local in locals {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if local.has_preview {
                continue;
            }
            let key = norm_key(&local.creator, &local.package_name);
            let preview_url = filename_map
                .get(&key)
                .and_then(|(hm, _)| hm.preview_url.clone())
                .or_else(|| {
                    paid_fallback
                        .iter()
                        .map(|(hm, ex)| {
                            (
                                hub::score_match(&local.creator, &local.package_name, hm),
                                hm,
                                ex,
                            )
                        })
                        .filter(|(s, _, _)| *s > 0)
                        .max_by_key(|(s, _, _)| *s)
                        .and_then(|(_, hm, _)| hm.preview_url.clone())
                });
            let Some(purl) = preview_url else { continue };
            match client.download_bytes(&purl) {
                Ok(bytes) => {
                    let out = thumbnails::thumb_path(thumbs_dir, local.id);
                    match thumbnails::generate_from_bytes(&bytes, &out) {
                        Ok(()) => {
                            counters.lock().previews_pulled += 1;
                            let _ = db.lock().execute(
                                "UPDATE packages SET has_preview = 1 WHERE id = ?1",
                                params![local.id],
                            );
                        }
                        Err(e) => {
                            eprintln!("hub preview convert failed for {}: {e:#}", local.id);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("hub preview download failed for {}: {e:#}", local.id);
                }
            }
            sleep_with_cancel(rate_limit_ms, cancel);
        }
    }

    let b1_matched = locals.len() - unmatched_in_b1.len();
    emit_log(
        app,
        "info",
        format!(
            "  {creator}: Phase B done ({b1_matched}/{} matched, {} entering B2)",
            locals.len(),
            unmatched_in_b1.len(),
        ),
    );

    // Phase B2 for this creator's unmatched packages. Done inline so each
    // worker is self-contained — the parallel outer loop can complete one
    // creator's work end-to-end without coordinating with others.
    if !unmatched_in_b1.is_empty() && !cancel.load(Ordering::Relaxed) && !counters.lock().gated {
        {
            let mut c = counters.lock();
            c.phase = "fallback".to_string();
        }
        let mut b2_cache: std::collections::HashMap<(String, String), Vec<hub::HubMatch>> =
            std::collections::HashMap::new();
        for idx in unmatched_in_b1 {
            if cancel.load(Ordering::Relaxed) || counters.lock().gated {
                break;
            }
            let local = &locals[idx];
            let abort = retry_one_keyword(
                local,
                client,
                db,
                app,
                cancel,
                rate_limit_ms,
                counters,
                &mut b2_cache,
            );
            if abort {
                break;
            }
        }
    }

    false
}

enum KwSearchError {
    /// Hub returned the age-gate. Whole sync aborts; caller propagates.
    Gated,
    /// Non-gate error (HTTP error, parse failure, etc.). Caller usually
    /// logs and moves on to the next local.
    Other(String),
}

/// Run a per-user keyword search, caching by `(creator, keyword)` so that
/// shared short fallback tokens (`morphs`, `normals`, `textures`, ...)
/// don't re-fetch dozens of paginated pages once per unmatched package.
///
/// Cache key is lowercased on both sides so `Morphs` and `morphs` (case
/// variants from different .var filenames) share one HTTP fetch.
///
/// `max_pages = None` walks all pages (used for targeted full-package-name
/// queries we expect to return small specific sets). `Some(N)` caps at N
/// pages (used for fallback tokens that hit huge generic result sets).
///
/// Rate-limit sleep happens only on the cache-miss path.
fn cached_keyword_search(
    client: &hub::HubClient,
    cache: &mut std::collections::HashMap<(String, String), Vec<hub::HubMatch>>,
    creator: &str,
    keyword: &str,
    max_pages: Option<u32>,
    rate_limit_ms: u64,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
) -> std::result::Result<Vec<hub::HubMatch>, KwSearchError> {
    let key = (creator.to_lowercase(), keyword.to_lowercase());
    if let Some(cached) = cache.get(&key) {
        return Ok(cached.clone());
    }
    let result = client.search_resources_for_user_keyword(creator, keyword, max_pages);
    sleep_with_cancel(rate_limit_ms, cancel);
    match result {
        Ok(h) => {
            cache.insert(key, h.clone());
            Ok(h)
        }
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.starts_with("gate:") {
                Err(KwSearchError::Gated)
            } else {
                Err(KwSearchError::Other(msg))
            }
        }
    }
}

/// Split a package_name on `_`, `-`, `.` and return the longest resulting
/// token of at least 3 characters. Returns `None` if the name has no
/// separators OR no token meets the length bar. Used by Phase B2 as a
/// fallback keyword when the full package_name returns 0 search rows due
/// to XF's tokenize-and-AND behavior.
fn longest_token(name: &str) -> Option<String> {
    let tokens: Vec<&str> = name.split(|c: char| c == '_' || c == '-' || c == '.').collect();
    if tokens.len() <= 1 {
        return None;
    }
    tokens
        .into_iter()
        .filter(|t| t.len() >= 3)
        .max_by_key(|t| t.len())
        .map(|t| t.to_string())
}

/// Phase B2 inner loop: one keyword-scoped search per still-unmatched
/// package. Mirrors `sync_one_creator`'s match logic but on a per-package
/// scale: fewer hub results, no creator-wide rebuild, just one local row to
/// update. Returns `true` to signal global abort (e.g. gate hit).
fn retry_one_keyword(
    local: &LocalPkg,
    client: &hub::HubClient,
    db: &Arc<parking_lot::Mutex<rusqlite::Connection>>,
    app: &AppHandle,
    cancel: &Arc<std::sync::atomic::AtomicBool>,
    rate_limit_ms: u64,
    counters: &Arc<parking_lot::Mutex<SyncCounters>>,
    cache: &mut std::collections::HashMap<(String, String), Vec<hub::HubMatch>>,
) -> bool {
    // XF tokenizes the keyword on `_` `-` `.` as word separators and AND's
    // the resulting tokens. So `damar_forest` becomes "damar" AND "forest",
    // which AND-fails when the title is just "Forest". Try the full keyword
    // first; if zero rows, retry with the longest individual token (≥3
    // chars) to broaden recall. The Content-Disposition filename gate still
    // rules out false positives — only the right resource has a matching
    // canonical .var filename.
    //
    // Cached: short fallback tokens like `morphs`, `normals`, `textures`
    // are shared across many unmatched packages — each XF search can span
    // dozens of pages, so a single corpus-wide sync would re-do them
    // dozens of times without this.
    let mut hits = match cached_keyword_search(
        client,
        cache,
        &local.creator,
        &local.package_name,
        None, // full-keyword search: walk all pages (typically small)
        rate_limit_ms,
        cancel,
    ) {
        Ok(h) => h,
        Err(KwSearchError::Gated) => {
            counters.lock().gated = true;
            return true;
        }
        Err(KwSearchError::Other(msg)) => {
            eprintln!(
                "hub sync B2: keyword search failed for {}.{}: {msg}",
                local.creator, local.package_name
            );
            emit_progress(
                app,
                counters,
                &format!("{}.{}", local.creator, local.package_name),
            );
            return false;
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return false;
    }

    if hits.is_empty() {
        if let Some(alt) = longest_token(&local.package_name) {
            if alt != local.package_name {
                eprintln!(
                    "hub sync B2: 0 rows for full keyword '{}'; retrying with longest token '{}'",
                    local.package_name, alt
                );
                match cached_keyword_search(
                    // Fallback token: cap at page 1. The token is a guess
                    // by definition (longest individual word from the
                    // package_name), so generic tokens like "Morphs" or
                    // "Character" can return 50-80+ rows. Top relevance-
                    // sorted hits on page 1 are the only realistic
                    // candidates; deeper pages are noise.
                    client, cache, &local.creator, &alt, Some(1), rate_limit_ms, cancel,
                ) {
                    Ok(h) => hits = h,
                    Err(KwSearchError::Gated) => {
                        counters.lock().gated = true;
                        return true;
                    }
                    Err(KwSearchError::Other(msg)) => {
                        eprintln!("hub sync B2: alt-keyword search failed: {msg}");
                    }
                }
                if cancel.load(Ordering::Relaxed) {
                    return false;
                }
            }
        }
    }

    // Slug-cache augmentation for B2: if the cached sitemap catalog has a
    // unique slug-normalize match for this local, append it to `hits` so it
    // gets HEAD-probed alongside keyword-search results. Necessary for the
    // L≤2 shortcut path (which never runs the broad B1 search) and useful
    // belt-and-braces on the post-B1 path too — XF's keyword search has its
    // own ranking blind spots.
    let xf_kw_ids: std::collections::HashSet<i64> =
        hits.iter().map(|h| h.resource_id).collect();
    let slug_extras = slug_cache_extras_for_creator(
        &local.creator,
        std::slice::from_ref(local),
        client,
        db,
        &xf_kw_ids,
        cancel,
        rate_limit_ms,
    );
    let slug_match_ids: std::collections::HashSet<i64> =
        slug_extras.iter().map(|h| h.resource_id).collect();
    if !slug_extras.is_empty() {
        hits.extend(slug_extras);
    }

    let target_key = norm_key(&local.creator, &local.package_name);
    let mut filename_hit: Option<hub::HubMatch> = None;
    let mut paid_candidates: Vec<(hub::HubMatch, String)> = Vec::new();

    // Cap HEAD probes per local. Search results are relevance-sorted, so
    // the right resource (if any) is among the top hits. Past ~15 we're
    // burning ~22 s/local for low-yield checks. The cap matters most for
    // generic fallback tokens whose result set can be 80+ rows.
    const MAX_HEAD_PROBES: usize = 15;
    for hr in hits.into_iter().take(MAX_HEAD_PROBES) {
        if cancel.load(Ordering::Relaxed) {
            return false;
        }
        let Some((slug, _id)) = hub::extract_slug_and_id_from_url(&hr.url) else {
            continue;
        };
        match client.head_download(&slug, hr.resource_id) {
            Ok(hub::DownloadProbe::Hosted { filename }) => {
                if let Some((c, p, _v)) = parse_var_filename(&filename) {
                    if norm_key(&c, &p) == target_key {
                        filename_hit = Some(hr);
                        sleep_with_cancel(rate_limit_ms, cancel);
                        break;
                    }
                }
            }
            Ok(hub::DownloadProbe::Offsite { url }) => {
                paid_candidates.push((hr, url));
            }
            Ok(hub::DownloadProbe::NotFound) => {}
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.starts_with("gate:") {
                    counters.lock().gated = true;
                    return true;
                }
                eprintln!(
                    "hub sync B2: HEAD probe failed for {}/{}: {msg}",
                    hr.title, hr.resource_id
                );
            }
        }
        sleep_with_cancel(rate_limit_ms, cancel);
    }

    let (hub_match, method, external_url) = if let Some(hm) = filename_hit.as_ref() {
        let m = if slug_match_ids.contains(&hm.resource_id) {
            "slug_match"
        } else {
            "filename"
        };
        (Some(hm), Some(m), None)
    } else {
        let best = paid_candidates
            .iter()
            .map(|(hm, ex)| {
                (
                    hub::score_match(&local.creator, &local.package_name, hm),
                    hm,
                    ex.as_str(),
                )
            })
            .filter(|(s, _, _)| *s > 0)
            .max_by_key(|(s, _, _)| *s);
        match best {
            Some((_, hm, ex)) => (Some(hm), Some("fuzzy_title"), Some(ex)),
            None => (None, None, None),
        }
    };

    if hub_match.is_some() {
        let now = unix_now();
        let conn = db.lock();
        let hub_lastmod: Option<i64> = hub_match
            .and_then(|hm| {
                conn.query_row(
                    "SELECT lastmod FROM hub_resources WHERE resource_id = ?1",
                    params![hm.resource_id],
                    |r| r.get::<_, i64>(0),
                )
                .ok()
            });
        let r = conn.execute(
            "UPDATE packages SET
               hub_resource_id    = ?2,
               hub_url            = ?3,
               hub_title          = ?4,
               hub_author         = ?5,
               hub_category       = ?6,
               hub_preview_url    = ?7,
               hub_synced_at      = ?8,
               hub_sync_state     = ?9,
               hub_billing_tier   = ?10,
               hub_is_hub_hosted  = ?11,
               hub_license        = ?12,
               hub_lastmod        = ?13,
               hub_external_url   = ?14,
               hub_match_method   = ?15
             WHERE id = ?1",
            params![
                local.id,
                hub_match.map(|b| b.resource_id),
                hub_match.map(|b| b.url.as_str()),
                hub_match.map(|b| b.title.as_str()),
                hub_match.map(|b| b.author.as_str()),
                hub_match.and_then(|b| b.category.as_deref()),
                hub_match.and_then(|b| b.preview_url.as_deref()),
                now,
                "matched",
                hub_match.and_then(|b| b.billing_tier.as_deref()),
                hub_match.map(|b| if b.is_hub_hosted { 1 } else { 0 }),
                hub_match.and_then(|b| b.license.as_deref()),
                hub_lastmod,
                external_url,
                method,
            ],
        );
        match r {
            Ok(_) => {
                // Flip the counter: this was previously not_found, now matched.
                let mut c = counters.lock();
                c.matched += 1;
                if c.not_found > 0 {
                    c.not_found -= 1;
                }
            }
            Err(e) => {
                eprintln!("hub sync B2 DB update failed for id {}: {e}", local.id);
                let mut c = counters.lock();
                if c.not_found > 0 {
                    c.not_found -= 1;
                }
                c.failed += 1;
            }
        }
    }

    // B2 is the terminal step for any package that landed here. Whether
    // we matched, stayed not_found, or hit a DB error, this package is
    // now done as far as the sync is concerned — tick `done` once so the
    // progress bar reflects the truth across both phases.
    counters.lock().done += 1;
    emit_progress(
        app,
        counters,
        &format!("{}.{}", local.creator, local.package_name),
    );
    false
}

fn mark_local_failed(
    db: &Arc<parking_lot::Mutex<rusqlite::Connection>>,
    locals: &[LocalPkg],
    state_str: &str,
) {
    let conn = db.lock();
    let now = unix_now();
    for local in locals {
        let _ = conn.execute(
            "UPDATE packages SET hub_synced_at = ?2, hub_sync_state = ?3 WHERE id = ?1",
            params![local.id, now, state_str],
        );
    }
}

struct SyncCounters {
    done: usize,
    total: usize,
    matched: usize,
    not_found: usize,
    failed: usize,
    previews_pulled: usize,
    gated: bool,
    /// Current phase label surfaced via progress events: `"pin"` during the
    /// per-creator search pass, `"fallback"` during the keyword-fallback
    /// retry pass.
    phase: String,
}

fn emit_progress(app: &AppHandle, counters: &Arc<parking_lot::Mutex<SyncCounters>>, current: &str) {
    let snap = counters.lock();
    let _ = app.emit(
        "hub-sync-progress",
        &HubSyncProgress {
            done: snap.done,
            total: snap.total,
            matched: snap.matched,
            not_found: snap.not_found,
            failed: snap.failed,
            previews_pulled: snap.previews_pulled,
            current: current.to_string(),
            current_status: if snap.gated { "gate".into() } else { snap.phase.clone() },
        },
    );
}

fn sleep_with_cancel(ms: u64, cancel: &Arc<std::sync::atomic::AtomicBool>) {
    let mut remaining = ms;
    while remaining > 0 && !cancel.load(Ordering::Relaxed) {
        let chunk = remaining.min(250);
        std::thread::sleep(Duration::from_millis(chunk));
        remaining = remaining.saturating_sub(chunk);
    }
}

/// Hub sync v2: per-creator search + HEAD-based canonical filename matching.
///
/// Phases (all gated by `cancel`):
///   A. Catalog refresh — if the `hub_resources` table is empty or older
///      than 24 h, refresh from the sitemap (8 fetches).
///   B. Per-creator pin — one `c[users]={creator}` search per distinct
///      creator, then HEAD each hub-hosted candidate to read the
///      Content-Disposition .var filename and match against local packages
///      by `(creator, package_name)`. Paid resources match by fuzzy title
///      and persist the offsite URL captured from the 301 Location header.
///   C. Optional preview pull for matched packages with no local thumbnail.
///
/// Subsequent syncs are delta-driven: `only_missing = true` excludes the
/// pinned-and-still-current packages, so the per-creator search only fires
/// for creators with unmatched/changed packages.
#[tauri::command]
pub fn hub_sync_active(state: State<'_, AppState>) -> bool {
    state.hub_sync_running.load(Ordering::Relaxed)
}

#[derive(Debug, Serialize, Clone)]
pub struct HubSyncLog {
    pub level: String,
    pub message: String,
    pub timestamp: i64,
}

fn emit_log(app: &AppHandle, level: &'static str, message: impl Into<String>) {
    let msg = message.into();
    eprintln!("[{level}] {msg}");
    let _ = app.emit(
        "hub-sync-log",
        &HubSyncLog {
            level: level.to_string(),
            message: msg,
            timestamp: unix_now(),
        },
    );
}

#[tauri::command]
pub async fn start_hub_sync(
    state: State<'_, AppState>,
    app: AppHandle,
    options: HubSyncOptions,
) -> Result<HubSyncSummary, String> {
    let db = state.db.clone();
    let thumbs_dir = state.thumbs_dir();
    let cancel = state.hub_sync_cancel.clone();
    let running_flag = state.hub_sync_running.clone();
    cancel.store(false, Ordering::Relaxed);
    running_flag.store(true, Ordering::Relaxed);

    let app_for_log = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let start = Instant::now();
        let client = hub::HubClient::new();

        emit_log(
            &app_for_log,
            "info",
            format!(
                "Sync starting. workers={}, rate_limit_ms={}, only_missing={}, creator={:?}",
                options.workers,
                options.rate_limit_ms,
                options.only_missing,
                options.creator.as_deref(),
            ),
        );

        // Phase A: ensure the hub_resources catalog is current. We don't
        // hard-fail if this errors — the sync can run without it; we just
        // lose the hub_lastmod field per package.
        if catalog_stale(&db, 24 * 3600) {
            emit_log(&app_for_log, "info", "Refreshing sitemap catalog (8 fetches)…");
            match catalog_refresh_inline(&db, &client) {
                Ok(n) => emit_log(
                    &app_for_log,
                    "info",
                    format!("Catalog refresh: {n} resources upserted."),
                ),
                Err(e) => emit_log(
                    &app_for_log,
                    "warn",
                    format!("Catalog refresh failed (non-fatal): {e:#}"),
                ),
            }
        }

        // Phase B inputs: build the local-package queue from DB.
        let locals: Vec<LocalPkg> = {
            let conn = db.lock();
            let mut clauses: Vec<&'static str> = vec!["creator <> ''"];
            if options.only_missing {
                clauses.push(
                    "(hub_synced_at IS NULL OR hub_sync_state IS NULL OR hub_sync_state <> 'matched')",
                );
            }
            let mut binds: Vec<rusqlite::types::Value> = Vec::new();
            if let Some(c) = &options.creator {
                clauses.push("creator = ? COLLATE NOCASE");
                binds.push(rusqlite::types::Value::Text(c.clone()));
            }
            if let Some(p) = &options.package_name {
                clauses.push("package_name = ? COLLATE NOCASE");
                binds.push(rusqlite::types::Value::Text(p.clone()));
            }
            let where_clause = clauses.join(" AND ");
            let sql = format!(
                "SELECT id, creator, package_name, has_preview FROM packages
                 WHERE {where_clause}
                 ORDER BY creator COLLATE NOCASE, package_name COLLATE NOCASE",
            );
            let mut stmt = match conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => return Err(map_err(e)),
            };
            let params_ref: Vec<&dyn rusqlite::ToSql> =
                binds.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
            let rows = stmt
                .query_map(params_ref.as_slice(), |r| {
                    Ok(LocalPkg {
                        id: r.get(0)?,
                        creator: r.get(1)?,
                        package_name: r.get(2)?,
                        has_preview: r.get::<_, i64>(3)? != 0,
                    })
                })
                .map_err(map_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_err)?;
            rows
        };

        let total = locals.len();
        let mut by_creator: std::collections::BTreeMap<String, Vec<LocalPkg>> =
            std::collections::BTreeMap::new();
        for l in locals {
            by_creator.entry(l.creator.clone()).or_default().push(l);
        }

        let counters = Arc::new(parking_lot::Mutex::new(SyncCounters {
            done: 0,
            total,
            matched: 0,
            not_found: 0,
            failed: 0,
            previews_pulled: 0,
            gated: false,
            phase: "pin".to_string(),
        }));

        // Parallelize across creators. Each worker handles one creator's
        // full Phase B + B2 inline. Counters/cancel/db are shared via Arc.
        // rayon's custom pool caps concurrency without touching the global
        // thread pool (which is also used by thumbnail generation).
        let workers = options.workers.clamp(1, 16);
        let client = Arc::new(client);
        let creators_vec: Vec<(String, Vec<LocalPkg>)> = by_creator.into_iter().collect();

        let pool = match rayon::ThreadPoolBuilder::new().num_threads(workers).build() {
            Ok(p) => p,
            Err(e) => return Err(format!("rayon pool build: {e}")),
        };
        pool.install(|| {
            use rayon::iter::{IntoParallelIterator, ParallelIterator};
            creators_vec.into_par_iter().for_each(|(creator, locals)| {
                if cancel.load(Ordering::Relaxed) || counters.lock().gated {
                    return;
                }
                sync_one_creator(
                    &creator,
                    &locals,
                    client.as_ref(),
                    &db,
                    &thumbs_dir,
                    &app,
                    &cancel,
                    options.rate_limit_ms,
                    options.pull_preview_for_no_thumb,
                    &counters,
                );
            });
        });

        let final_counters = std::mem::replace(
            &mut *counters.lock(),
            SyncCounters {
                done: 0, total: 0, matched: 0, not_found: 0, failed: 0,
                previews_pulled: 0, gated: false, phase: String::new(),
            },
        );
        emit_log(
            &app_for_log,
            "info",
            format!(
                "Sync finished. matched={} not_found={} failed={} gated={}",
                final_counters.matched,
                final_counters.not_found,
                final_counters.failed,
                final_counters.gated,
            ),
        );
        Ok(start_hub_sync_finish(final_counters, start.elapsed().as_millis()))
    })
    .await
    .map_err(|e| format!("join error: {e}"));

    // Clear the running flag regardless of how the task ended (success,
    // error, or panic propagated). The frontend uses this to know whether
    // a backend sync is still in progress after an HMR/page reload.
    running_flag.store(false, Ordering::Relaxed);
    result?
}

fn start_hub_sync_finish(c: SyncCounters, elapsed_ms: u128) -> HubSyncSummary {
    HubSyncSummary {
        considered: c.total,
        matched: c.matched,
        not_found: c.not_found,
        failed: c.failed,
        previews_pulled: c.previews_pulled,
        elapsed_ms,
        gated: c.gated,
    }
}

#[derive(Debug, Serialize)]
pub struct HubDebugFetch {
    pub status: u16,
    pub final_url: String,
    pub body_len: usize,
    /// First 60 KB of the body. Truncated to keep the Tauri bridge happy.
    pub body_head: String,
    /// Substring presence flags requested by the caller.
    pub contains: std::collections::HashMap<String, bool>,
}

/// Generic Rust-side GET for diagnostic probing of hub URLs from the
/// devtools console. Uses the same agent (so vamhubconsent cookie is set)
/// and follows redirects. Returns status, final URL after redirects, body
/// length, the first 60 KB of body, and a presence-check map for each
/// `needle` substring (case-sensitive).
#[tauri::command]
pub async fn hub_debug_fetch(
    url: String,
    needles: Option<Vec<String>>,
) -> Result<HubDebugFetch, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = hub::HubClient::new();
        let (status, final_url, body) = client
            .debug_get(&url)
            .map_err(|e| format!("{e:#}"))?;
        let mut contains = std::collections::HashMap::new();
        if let Some(needles) = needles {
            for n in needles {
                let present = body.contains(&n);
                contains.insert(n, present);
            }
        }
        let body_head = body.chars().take(60_000).collect::<String>();
        Ok(HubDebugFetch {
            status,
            final_url,
            body_len: body.len(),
            body_head,
            contains,
        })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[derive(Debug, Serialize)]
pub struct HubDebugSearchResult {
    /// Number of result rows returned by the per-creator XF search across
    /// all pages walked.
    pub total_rows: usize,
    /// Last page number we saw in the pagenav. None = no pagenav found
    /// (single page only).
    pub last_page: Option<u32>,
    pub rows: Vec<HubDebugSearchRow>,
}

#[derive(Debug, Serialize)]
pub struct HubDebugSearchRow {
    pub resource_id: i64,
    pub title: String,
    pub author: String,
    pub url: String,
    pub category: Option<String>,
    pub billing_tier: Option<String>,
    pub is_hub_hosted: bool,
    pub license: Option<String>,
    /// HEAD-probe outcome. Either the canonical .var filename (Hosted),
    /// the offsite URL (Offsite), the string "NotFound", or an error
    /// message if the probe itself failed.
    pub probe: String,
    /// For Hosted probes only: the parsed (creator, package_name) pair we'd
    /// use as the canonical match key. Useful for verifying filename
    /// normalization is doing what we think.
    pub probe_parsed_key: Option<String>,
}

/// Dev-time probe of the per-creator search + HEAD-probe pipeline WITHOUT
/// touching the packages table. Returns every row the search found plus
/// the HEAD-probe outcome for each. If a known hub resource (e.g. a
/// recon-validated one) is missing from `rows`, pagination or search
/// filtering is to blame; if it's in `rows` but `probe_parsed_key` doesn't
/// match the local file's (creator, package_name), filename parsing is.
// Args:
//   creator       — XF username (case-insensitive on the hub side)
//   keyword       — Some(kw): use Phase-B2-style targeted search
//                   None: use empty-keyword per-author search (Phase B)
//   rate_limit_ms — inter-request delay; default 2000
//   skip_probe    — when true, skip HEAD-probing each row (much faster)
#[tauri::command]
pub async fn hub_debug_search(
    _state: State<'_, AppState>,
    creator: String,
    keyword: Option<String>,
    rate_limit_ms: Option<u64>,
    skip_probe: Option<bool>,
) -> Result<HubDebugSearchResult, String> {
    let rate = rate_limit_ms.unwrap_or(2000);
    let skip_probe = skip_probe.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || {
        let client = hub::HubClient::new();
        let hits = match keyword.as_deref() {
            Some(kw) if !kw.is_empty() => client
                .search_resources_for_user_keyword(&creator, kw, None)
                .map_err(|e| format!("search: {e:#}"))?,
            _ => client
                .search_resources_by_user(&creator)
                .map_err(|e| format!("search: {e:#}"))?,
        };
        let total_rows = hits.len();
        // last_page reporting is best-effort: search_resources_by_user
        // already eprintln's its pagination decision to the dev terminal,
        // and the row count below is the authoritative diagnostic.
        let last_page: Option<u32> = None;

        let mut rows = Vec::with_capacity(hits.len());
        for hr in hits {
            if skip_probe {
                rows.push(HubDebugSearchRow {
                    resource_id: hr.resource_id,
                    title: hr.title,
                    author: hr.author,
                    url: hr.url,
                    category: hr.category,
                    billing_tier: hr.billing_tier,
                    is_hub_hosted: hr.is_hub_hosted,
                    license: hr.license,
                    probe: "<skipped>".to_string(),
                    probe_parsed_key: None,
                });
                continue;
            }
            let probe_str;
            let probe_parsed_key;
            let Some((slug, _id)) = hub::extract_slug_and_id_from_url(&hr.url) else {
                rows.push(HubDebugSearchRow {
                    resource_id: hr.resource_id,
                    title: hr.title,
                    author: hr.author,
                    url: hr.url,
                    category: hr.category,
                    billing_tier: hr.billing_tier,
                    is_hub_hosted: hr.is_hub_hosted,
                    license: hr.license,
                    probe: "<bad url>".to_string(),
                    probe_parsed_key: None,
                });
                continue;
            };
            match client.head_download(&slug, hr.resource_id) {
                Ok(hub::DownloadProbe::Hosted { filename }) => {
                    probe_parsed_key = parse_var_filename(&filename)
                        .map(|(c, p, _)| norm_key(&c, &p));
                    probe_str = format!("Hosted: {filename}");
                }
                Ok(hub::DownloadProbe::Offsite { url }) => {
                    probe_parsed_key = None;
                    probe_str = format!("Offsite: {url}");
                }
                Ok(hub::DownloadProbe::NotFound) => {
                    probe_parsed_key = None;
                    probe_str = "NotFound".to_string();
                }
                Err(e) => {
                    probe_parsed_key = None;
                    probe_str = format!("ProbeError: {e}");
                }
            }
            rows.push(HubDebugSearchRow {
                resource_id: hr.resource_id,
                title: hr.title,
                author: hr.author,
                url: hr.url,
                category: hr.category,
                billing_tier: hr.billing_tier,
                is_hub_hosted: hr.is_hub_hosted,
                license: hr.license,
                probe: probe_str,
                probe_parsed_key,
            });
            std::thread::sleep(Duration::from_millis(rate));
        }

        Ok(HubDebugSearchResult { total_rows, last_page, rows })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

#[derive(Debug, Serialize)]
pub struct HubDebugDump {
    /// PRAGMA table_info() column names on packages — sanity-confirms the
    /// v14 ALTER actually applied.
    pub packages_columns: Vec<String>,
    /// Total rows in hub_resources (sitemap cache).
    pub hub_resources_count: i64,
    /// Per-row hub_* state for every package by this creator. Pulled with
    /// a raw SELECT, bypassing PackageRow, so we see the columns that aren't
    /// yet surfaced.
    pub rows: Vec<HubDebugRow>,
}

#[derive(Debug, Serialize)]
pub struct HubDebugRow {
    pub id: i64,
    pub creator: String,
    pub package_name: String,
    pub var_path: String,
    pub hub_resource_id: Option<i64>,
    pub hub_sync_state: Option<String>,
    pub hub_synced_at: Option<i64>,
    pub hub_category: Option<String>,
    pub hub_billing_tier: Option<String>,
    pub hub_is_hub_hosted: Option<i64>,
    pub hub_license: Option<String>,
    pub hub_lastmod: Option<i64>,
    pub hub_external_url: Option<String>,
    pub hub_match_method: Option<String>,
}

/// Dev-time inspection of the hub_* state for a given creator + sanity-check
/// that the v14 migration actually applied. Returns raw column values
/// straight from SQL so we can diagnose data-vs-deserialization questions
/// without involving PackageRow.
#[tauri::command]
pub fn hub_debug_dump(
    state: State<'_, AppState>,
    creator: String,
) -> Result<HubDebugDump, String> {
    let conn = state.db.lock();

    let mut col_stmt = conn
        .prepare("PRAGMA table_info(packages)")
        .map_err(map_err)?;
    let cols: Vec<String> = col_stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;

    let hub_resources_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM hub_resources", [], |r| r.get(0))
        .map_err(map_err)?;

    let mut stmt = conn
        .prepare(
            "SELECT id, creator, package_name, var_path,
                    hub_resource_id, hub_sync_state, hub_synced_at,
                    hub_category, hub_billing_tier, hub_is_hub_hosted,
                    hub_license, hub_lastmod, hub_external_url, hub_match_method
             FROM packages
             WHERE creator = ?1 COLLATE NOCASE
             ORDER BY package_name COLLATE NOCASE",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map(params![creator], |r| {
            Ok(HubDebugRow {
                id: r.get(0)?,
                creator: r.get(1)?,
                package_name: r.get(2)?,
                var_path: r.get(3)?,
                hub_resource_id: r.get(4)?,
                hub_sync_state: r.get(5)?,
                hub_synced_at: r.get(6)?,
                hub_category: r.get(7)?,
                hub_billing_tier: r.get(8)?,
                hub_is_hub_hosted: r.get(9)?,
                hub_license: r.get(10)?,
                hub_lastmod: r.get(11)?,
                hub_external_url: r.get(12)?,
                hub_match_method: r.get(13)?,
            })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;

    Ok(HubDebugDump {
        packages_columns: cols,
        hub_resources_count,
        rows,
    })
}

#[derive(Debug, Serialize)]
pub struct HubStatus {
    pub catalog_rows: i64,
    pub catalog_latest_fetched_at: Option<i64>,
    pub catalog_latest_lastmod: Option<i64>,
    pub total_packages: i64,
    pub matched: i64,
    pub matched_by_filename: i64,
    pub matched_by_fuzzy_title: i64,
    pub matched_by_slug_match: i64,
    pub not_found: i64,
    pub failed: i64,
    pub never_synced: i64,
    /// `(category, count)` pairs for matched packages, sorted by count desc.
    pub top_categories: Vec<(String, i64)>,
    /// `(billing_tier, count)` for matched packages.
    /// `null` tier appears as `"free"` in the output for UI legibility.
    pub by_billing_tier: Vec<(String, i64)>,
}

/// One-shot status snapshot for the hub-sync dashboard. Cheap (a handful of
/// aggregate SELECTs) — UI may refetch on a short interval or after sync
/// events without worrying about cost.
#[tauri::command]
pub fn hub_status(state: State<'_, AppState>) -> Result<HubStatus, String> {
    let conn = state.db.lock();

    let catalog_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM hub_resources", [], |r| r.get(0))
        .unwrap_or(0);
    let catalog_latest_fetched_at: Option<i64> = conn
        .query_row("SELECT MAX(fetched_at) FROM hub_resources", [], |r| r.get(0))
        .ok()
        .flatten();
    let catalog_latest_lastmod: Option<i64> = conn
        .query_row("SELECT MAX(lastmod) FROM hub_resources", [], |r| r.get(0))
        .ok()
        .flatten();

    let total_packages: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM packages WHERE creator <> ''",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    fn count_where(conn: &rusqlite::Connection, clause: &str) -> i64 {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM packages WHERE creator <> '' AND {clause}"),
            [],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    let matched = count_where(&conn, "hub_sync_state = 'matched'");
    let matched_by_filename =
        count_where(&conn, "hub_sync_state = 'matched' AND hub_match_method = 'filename'");
    let matched_by_fuzzy_title = count_where(
        &conn,
        "hub_sync_state = 'matched' AND hub_match_method = 'fuzzy_title'",
    );
    let matched_by_slug_match = count_where(
        &conn,
        "hub_sync_state = 'matched' AND hub_match_method = 'slug_match'",
    );
    let not_found = count_where(&conn, "hub_sync_state = 'not_found'");
    let failed = count_where(&conn, "hub_sync_state = 'failed'");
    let never_synced = count_where(&conn, "hub_sync_state IS NULL");

    let top_categories: Vec<(String, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT hub_category, COUNT(*) FROM packages
                 WHERE hub_category IS NOT NULL AND hub_sync_state = 'matched'
                 GROUP BY hub_category
                 ORDER BY COUNT(*) DESC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(map_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_err)?;
        rows
    };

    let by_billing_tier: Vec<(String, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT COALESCE(hub_billing_tier, 'free'), COUNT(*) FROM packages
                 WHERE hub_sync_state = 'matched'
                 GROUP BY COALESCE(hub_billing_tier, 'free')
                 ORDER BY COUNT(*) DESC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))
            .map_err(map_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_err)?;
        rows
    };

    Ok(HubStatus {
        catalog_rows,
        catalog_latest_fetched_at,
        catalog_latest_lastmod,
        total_packages,
        matched,
        matched_by_filename,
        matched_by_fuzzy_title,
        matched_by_slug_match,
        not_found,
        failed,
        never_synced,
        top_categories,
        by_billing_tier,
    })
}

fn catalog_refresh_inline(
    db: &Arc<parking_lot::Mutex<rusqlite::Connection>>,
    client: &hub::HubClient,
) -> anyhow::Result<usize> {
    let entries = client.fetch_sitemap_catalog()?;
    let total = entries.len();
    let now = unix_now();
    let mut conn = db.lock();
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO hub_resources (resource_id, slug, lastmod, fetched_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(resource_id) DO UPDATE SET
               slug = excluded.slug,
               lastmod = excluded.lastmod,
               fetched_at = excluded.fetched_at",
        )?;
        for e in entries {
            stmt.execute(params![e.resource_id, e.slug, e.lastmod, now])?;
        }
    }
    tx.commit()?;
    Ok(total)
}

fn catalog_stale(db: &Arc<parking_lot::Mutex<rusqlite::Connection>>, max_age_secs: i64) -> bool {
    let conn = db.lock();
    let recent: Option<i64> = conn
        .query_row(
            "SELECT MAX(fetched_at) FROM hub_resources",
            [],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let now = unix_now();
    match recent {
        None => true,
        Some(t) => now - t > max_age_secs,
    }
}

#[tauri::command]
pub fn stop_hub_sync(state: State<'_, AppState>) {
    state
        .hub_sync_cancel
        .store(true, Ordering::Relaxed);
}

/// Open the .var, read meta.json, enumerate image entries. Single command for
/// the detail-view to populate everything it needs.
#[tauri::command]
pub fn get_package_detail(
    state: State<'_, AppState>,
    id: i64,
) -> Result<PackageDetail, String> {
    use std::io::Read;

    // 1. Fetch the package row + preview_path + var_path from DB.
    let (row, preview_path) = {
        let conn = state.db.lock();
        let detail_sql = format!(
            "SELECT id, creator, package_name, version, license, program_version, description,
                    package_type, content_count, dep_count, file_size, file_mtime, var_path,
                    has_preview, is_favorite, is_hidden,
                    hub_resource_id, hub_url, hub_title, hub_author, hub_category,
                    hub_preview_url, hub_synced_at, hub_sync_state,
                    scene_count, look_count, plugin_count, clothing_count,
                    hair_count, pose_count, subscene_count,
                    error, package_mtime, preview_path,
                    hub_billing_tier, hub_is_hub_hosted, hub_license,
                    hub_lastmod, hub_external_url, hub_match_method,
                    {TAGS_SUBQUERY} AS tags
             FROM packages WHERE id = ?1",
        );
        let mut stmt = conn
            .prepare(&detail_sql)
            .map_err(map_err)?;
        stmt.query_row(params![id], |r| {
            let row = PackageRow {
                id: r.get(0)?,
                creator: r.get(1)?,
                package_name: r.get(2)?,
                version: r.get(3)?,
                license: r.get(4)?,
                program_version: r.get(5)?,
                description: r.get(6)?,
                package_type: r.get(7)?,
                content_count: r.get(8)?,
                dep_count: r.get(9)?,
                file_size: r.get(10)?,
                file_mtime: r.get(11)?,
                var_path: r.get(12)?,
                has_preview: r.get::<_, i64>(13)? != 0,
                is_favorite: r.get::<_, i64>(14)? != 0,
                is_hidden: r.get::<_, i64>(15)? != 0,
                hub_resource_id: r.get(16)?,
                hub_url: r.get(17)?,
                hub_title: r.get(18)?,
                hub_author: r.get(19)?,
                hub_category: r.get(20)?,
                hub_preview_url: r.get(21)?,
                hub_synced_at: r.get(22)?,
                hub_sync_state: r.get(23)?,
                scene_count: r.get(24)?,
                look_count: r.get(25)?,
                plugin_count: r.get(26)?,
                clothing_count: r.get(27)?,
                hair_count: r.get(28)?,
                pose_count: r.get(29)?,
                subscene_count: r.get(30)?,
                error: r.get(31)?,
                package_mtime: r.get(32)?,
                hub_billing_tier: r.get(34)?,
                hub_is_hub_hosted: r.get(35)?,
                hub_license: r.get(36)?,
                hub_lastmod: r.get(37)?,
                hub_external_url: r.get(38)?,
                hub_match_method: r.get(39)?,
                tags: split_tags(r.get::<_, Option<String>>(40)?),
            };
            let preview: Option<String> = r.get(33)?;
            Ok((row, preview))
        })
        .map_err(|e| format!("package id {id}: {e}"))?
    };

    // 2. Open zip read-only, parse meta.json, enumerate images.
    let var_path = std::path::PathBuf::from(&row.var_path);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .open(&var_path)
        .map_err(|e| format!("open {}: {e}", var_path.display()))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|e| format!("read zip {}: {e}", var_path.display()))?;

    let (content_list, dependencies, instructions) = match zip.by_name("meta.json") {
        Ok(mut entry) => {
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            let _ = entry.read_to_end(&mut bytes);
            let trimmed = if bytes.len() >= 3 && &bytes[..3] == [0xEF, 0xBB, 0xBF] {
                &bytes[3..]
            } else {
                &bytes[..]
            };
            match crate::meta::parse_meta(trimmed) {
                Ok(m) => (m.content_list, m.dependencies, m.instructions),
                Err(_) => (vec![], vec![], None),
            }
        }
        Err(_) => (vec![], vec![], None),
    };
    let instructions = instructions.filter(|s| !s.trim().is_empty());

    let mut images: Vec<ImageEntry> = Vec::new();
    for i in 0..zip.len() {
        if let Ok(entry) = zip.by_index_raw(i) {
            let name = entry.name().to_string();
            let lower = name.to_lowercase();
            if lower.ends_with(".jpg")
                || lower.ends_with(".jpeg")
                || lower.ends_with(".png")
            {
                images.push(ImageEntry {
                    path: name,
                    size: entry.size() as i64,
                });
            }
        }
    }
    images.sort_by(|a, b| a.path.cmp(&b.path));

    Ok(PackageDetail {
        package: row,
        content_list,
        dependencies,
        instructions,
        images,
        preview_path,
    })
}

#[derive(Debug, Serialize)]
pub struct RelatedPackage {
    /// `id` is set when the dep key resolved to a locally-installed package.
    /// `None` means the user doesn't have this package — `raw_dep_key` still
    /// carries enough info to display.
    pub id: Option<i64>,
    pub raw_dep_key: String,
    pub creator: Option<String>,
    pub package_name: Option<String>,
    pub version: Option<String>,
    pub package_type: Option<String>,
    pub has_preview: bool,
    pub is_hidden: bool,
}

#[derive(Debug, Serialize)]
pub struct PackageRelationships {
    /// Outgoing edges: packages this one depends on. One row per raw dep key.
    pub depends_on: Vec<RelatedPackage>,
    /// Incoming edges: other packages that depend on this one. Always resolved
    /// (only included when src side exists locally).
    pub used_by: Vec<RelatedPackage>,
}

/// Forward + reverse dependency lookup for a single package. Reads from
/// `package_dep_links` (populated by the scanner) so this is a cheap couple of
/// indexed lookups, no .var I/O.
#[tauri::command]
pub fn get_package_relationships(
    state: State<'_, AppState>,
    id: i64,
) -> Result<PackageRelationships, String> {
    let conn = state.db.lock();

    // Outgoing: every raw dep key for this package, left-joined to the resolved
    // dst row when present. Sorted by creator/name so missing entries (which
    // have NULLs) sort together at the end.
    let depends_on: Vec<RelatedPackage> = {
        let mut stmt = conn
            .prepare(
                "SELECT l.raw_dep_key,
                        p.id, p.creator, p.package_name, p.version,
                        p.package_type, p.has_preview, p.is_hidden
                 FROM package_dep_links l
                 LEFT JOIN packages p ON p.id = l.dst_package_id
                 WHERE l.src_package_id = ?1
                 ORDER BY (p.id IS NULL) ASC,
                          p.creator COLLATE NOCASE ASC,
                          p.package_name COLLATE NOCASE ASC,
                          l.raw_dep_key ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok(RelatedPackage {
                    raw_dep_key: r.get(0)?,
                    id: r.get(1)?,
                    creator: r.get(2)?,
                    package_name: r.get(3)?,
                    version: r.get(4)?,
                    package_type: r.get(5)?,
                    has_preview: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                    is_hidden: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                })
            })
            .map_err(map_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_err)?;
        rows
    };

    // Incoming: every package whose dep_links point to this id. Only resolved
    // edges count (a "used by" must have a real src package).
    let used_by: Vec<RelatedPackage> = {
        let mut stmt = conn
            .prepare(
                "SELECT l.raw_dep_key,
                        p.id, p.creator, p.package_name, p.version,
                        p.package_type, p.has_preview, p.is_hidden
                 FROM package_dep_links l
                 JOIN packages p ON p.id = l.src_package_id
                 WHERE l.dst_package_id = ?1
                 ORDER BY p.creator COLLATE NOCASE ASC,
                          p.package_name COLLATE NOCASE ASC,
                          p.version COLLATE NOCASE ASC",
            )
            .map_err(map_err)?;
        let rows = stmt
            .query_map(params![id], |r| {
                Ok(RelatedPackage {
                    raw_dep_key: r.get(0)?,
                    id: r.get(1)?,
                    creator: r.get(2)?,
                    package_name: r.get(3)?,
                    version: r.get(4)?,
                    package_type: r.get(5)?,
                    has_preview: r.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                    is_hidden: r.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                })
            })
            .map_err(map_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_err)?;
        rows
    };

    Ok(PackageRelationships { depends_on, used_by })
}

/// One-shot CLI/dev affordance: rebuild `package_dep_links` from the current
/// `package_dependencies` table without rescanning files. Useful when the
/// resolver logic changes or after manually editing the DB during development.
#[tauri::command]
pub fn resolve_dependencies(state: State<'_, AppState>) -> Result<(), String> {
    let mut conn = state.db.lock();
    let tx = conn.transaction().map_err(map_err)?;
    crate::deps::resolve_all(&tx).map_err(|e| format!("resolve deps: {e:#}"))?;
    tx.commit().map_err(map_err)?;
    Ok(())
}

/// Open Explorer focused on the .var file (selects it in the parent folder).
/// Windows-only for v1 — falls through silently on other platforms.
#[tauri::command]
pub fn reveal_in_folder(path: String) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // Per Microsoft docs the syntax is `explorer.exe /select,"<path>"`.
        // The comma is part of the flag, not a separator, so we need `raw_arg`
        // to avoid Rust's default quoting that would mangle it.
        // Spawn-without-wait because explorer.exe may not return promptly.
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{path}\""))
            .spawn()
            .map_err(|e| format!("spawn explorer: {e}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct TypeCount {
    pub package_type: String,
    pub count: i64,
}

/// Aggregate package counts by type for the chip filter row.
/// Always excludes hidden packages so the chip counts match the user's default view.
#[tauri::command]
pub fn list_type_counts(state: State<'_, AppState>) -> Result<Vec<TypeCount>, String> {
    let conn = state.db.lock();
    let mut stmt = conn
        .prepare(
            "SELECT package_type, COUNT(*)
             FROM packages
             WHERE is_hidden = 0
             GROUP BY package_type
             ORDER BY 2 DESC",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(TypeCount {
                package_type: r.get(0)?,
                count: r.get(1)?,
            })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

#[derive(Debug, Serialize)]
pub struct HubCategoryCount {
    pub hub_category: String,
    pub count: i64,
}

/// Hub category chip-row data for Fetched mode. Aggregates non-hidden
/// packages with a non-null `hub_category` (deduped — categories like
/// "Looks" share one chip across Free/Paid/Paid Early-Access tiers since
/// the per-tier prefix is stripped at scrape time).
#[tauri::command]
pub fn list_hub_categories(
    state: State<'_, AppState>,
) -> Result<Vec<HubCategoryCount>, String> {
    let conn = state.db.lock();
    let mut stmt = conn
        .prepare(
            "SELECT hub_category, COUNT(*)
             FROM packages
             WHERE is_hidden = 0 AND hub_category IS NOT NULL
             GROUP BY hub_category
             ORDER BY 2 DESC",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(HubCategoryCount {
                hub_category: r.get(0)?,
                count: r.get(1)?,
            })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

/// Count of non-hidden packages with no hub category — the "(unidentified)"
/// virtual chip alongside the per-category counts. Inverse of
/// `list_hub_categories`' selection clause: `hub_category IS NULL` covers
/// never-synced, not_found, failed, and gated rows in one count.
#[tauri::command]
pub fn count_hub_unidentified(state: State<'_, AppState>) -> Result<i64, String> {
    let conn = state.db.lock();
    conn.query_row(
        "SELECT COUNT(*) FROM packages
         WHERE is_hidden = 0 AND hub_category IS NULL",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map_err(map_err)
}

#[derive(Debug, Serialize)]
pub struct Namespace {
    pub namespace: String,
    /// Raw JSON: either the string "any" or an array of kind: values.
    pub applies_to_json: Option<String>,
    pub cardinality: Option<String>,
    /// Number of distinct active tag values that exist in this namespace.
    pub value_count: i64,
    /// Number of families that have at least one tag in this namespace.
    pub family_count: i64,
}

/// Enumerate the active v4 namespaces (taxonomy entries with is_active=1),
/// with per-namespace value + family counts. Drives the FacetPanel sidebar.
#[tauri::command]
pub fn list_namespaces(state: State<'_, AppState>) -> Result<Vec<Namespace>, String> {
    let conn = state.db.lock();
    let mut stmt = conn
        .prepare(
            "SELECT t.namespace,
                    MIN(t.applies_to_json) AS applies_to_json,
                    MIN(t.cardinality)     AS cardinality,
                    COUNT(*)               AS value_count,
                    COALESCE((
                        SELECT COUNT(DISTINCT ft.family_id)
                        FROM family_tags ft
                        WHERE ft.tag LIKE t.namespace || ':%'
                    ), 0) AS family_count
             FROM taxonomy t
             WHERE t.is_active = 1 AND t.namespace IS NOT NULL
             GROUP BY t.namespace
             ORDER BY family_count DESC, t.namespace ASC",
        )
        .map_err(map_err)?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Namespace {
                namespace: r.get(0)?,
                applies_to_json: r.get(1)?,
                cardinality: r.get(2)?,
                value_count: r.get(3)?,
                family_count: r.get(4)?,
            })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?;
    Ok(rows)
}

#[derive(Debug, Serialize)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

/// Per-tag family counts. Drives the chip values inside each FacetPanel section.
/// Filtering by `namespace` is cheap (indexed on `family_tags.tag` via prefix
/// LIKE) and avoids shipping ~280 rows when the UI only needs one namespace.
#[tauri::command]
pub fn list_tag_counts(
    state: State<'_, AppState>,
    namespace: Option<String>,
) -> Result<Vec<TagCount>, String> {
    let conn = state.db.lock();
    let (sql, prefix) = match namespace {
        Some(ns) if !ns.is_empty() => (
            "SELECT tag, COUNT(*) AS n
             FROM family_tags
             WHERE tag LIKE ?1
             GROUP BY tag
             ORDER BY n DESC, tag ASC",
            Some(format!("{ns}:%")),
        ),
        _ => (
            "SELECT tag, COUNT(*) AS n
             FROM family_tags
             GROUP BY tag
             ORDER BY n DESC, tag ASC",
            None,
        ),
    };
    let mut stmt = conn.prepare(sql).map_err(map_err)?;
    let rows: Vec<TagCount> = if let Some(p) = prefix {
        stmt.query_map(params![p], |r| {
            Ok(TagCount { tag: r.get(0)?, count: r.get(1)? })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?
    } else {
        stmt.query_map([], |r| {
            Ok(TagCount { tag: r.get(0)?, count: r.get(1)? })
        })
        .map_err(map_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_err)?
    };
    Ok(rows)
}

#[derive(Debug, Serialize, Clone)]
pub struct ThumbProgress {
    pub id: i64,
    pub ok: bool,
    pub done: usize,
    pub total: usize,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ThumbGenSummary {
    pub considered: usize,
    pub generated: usize,
    pub already_fresh: usize,
    pub errors: usize,
    pub elapsed_ms: u128,
}

/// Generate WebP thumbnails for every package that has a `preview_path` and either
/// no thumb yet or a stale one (source .var mtime > thumb mtime). Runs in parallel
/// via rayon; emits a `thumb-progress` event per package as it completes.
#[tauri::command]
pub async fn generate_thumbnails(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<ThumbGenSummary, String> {
    let db = state.db.clone();
    let thumbs_dir = state.thumbs_dir();
    std::fs::create_dir_all(&thumbs_dir)
        .map_err(|e| format!("create thumbs dir: {e}"))?;

    tauri::async_runtime::spawn_blocking(move || {
        let start = Instant::now();

        // 1. Pull candidates from DB (single connection, single read).
        let candidates: Vec<(i64, PathBuf, String, i64)> = {
            let conn = db.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT id, var_path, preview_path, file_mtime
                     FROM packages
                     WHERE preview_path IS NOT NULL AND error IS NULL",
                )
                .map_err(|e| format!("prepare: {e}"))?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        PathBuf::from(r.get::<_, String>(1)?),
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                })
                .map_err(|e| format!("query: {e}"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| format!("collect: {e}"))?;
            rows
        };

        let considered = candidates.len();

        // 2. Filter to stale/missing thumbs only.
        let queue: Vec<(i64, PathBuf, String)> = candidates
            .into_iter()
            .filter(|(id, _, _, mtime)| !thumbnails::is_fresh(&thumbs_dir, *id, *mtime))
            .map(|(id, p, e, _)| (id, p, e))
            .collect();

        let total = queue.len();
        let already_fresh = considered - total;
        let done = AtomicUsize::new(0);
        let errors = AtomicUsize::new(0);

        // 3. Process in parallel. Per-thumb errors are non-fatal.
        queue.par_iter().for_each(|(id, var_path, preview_entry)| {
            let result = thumbnails::generate(var_path, preview_entry, &thumbs_dir, *id);
            let (ok, err_msg) = match result {
                Ok(()) => (true, None),
                Err(e) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                    (false, Some(format!("{e:#}")))
                }
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            let _ = app.emit(
                "thumb-progress",
                ThumbProgress { id: *id, ok, done: n, total, error: err_msg },
            );
        });

        Ok(ThumbGenSummary {
            considered,
            generated: total - errors.load(Ordering::Relaxed),
            already_fresh,
            errors: errors.load(Ordering::Relaxed),
            elapsed_ms: start.elapsed().as_millis(),
        })
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

fn build_where(filter: &QueryFilter) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut clauses: Vec<String> = vec![];
    let mut binds: Vec<Value> = vec![];
    let push_text = |binds: &mut Vec<Value>, s: String| {
        binds.push(Value::Text(s));
        binds.len()
    };
    let push_int = |binds: &mut Vec<Value>, n: i64| {
        binds.push(Value::Integer(n));
        binds.len()
    };

    if let Some(s) = filter.search.as_deref().filter(|s| !s.trim().is_empty()) {
        // Token-AND search: each whitespace-separated word is its own
        // substring match against creator or package_name; all tokens must
        // hit. Order-independent and separator-agnostic — "scene 91" hits
        // "Scene_91_Liz" via tokens "scene" and "91". `_` in a token stays
        // as a LIKE single-char wildcard, which is a harmless overmatch.
        // Will be replaced by FTS5 in a later milestone.
        for tok in s.split_whitespace() {
            let i = push_text(&mut binds, format!("%{}%", tok));
            clauses.push(format!(
                "(creator LIKE ?{i} COLLATE NOCASE OR package_name LIKE ?{i} COLLATE NOCASE)"
            ));
        }
    }
    if let Some(c) = filter.creator.as_deref().filter(|s| !s.is_empty()) {
        let i = push_text(&mut binds, c.to_string());
        clauses.push(format!("creator = ?{i} COLLATE NOCASE"));
    }
    if let Some(c) = filter.hub_category.as_deref().filter(|s| !s.is_empty()) {
        let i = push_text(&mut binds, c.to_string());
        clauses.push(format!("hub_category = ?{i}"));
    } else if filter.hub_unmatched {
        // Mirrors the inverse of `list_hub_categories`' selection clause:
        // a row is "unmatched" iff it has no hub_category. Includes both
        // never-synced (hub_sync_state IS NULL) and synced-but-not-matched
        // (not_found / failed / gate).
        clauses.push("hub_category IS NULL".to_string());
    }
    if let Some(t) = filter.package_type.as_deref().filter(|s| !s.is_empty()) {
        let i = push_text(&mut binds, t.to_string());
        clauses.push(format!("package_type = ?{i}"));
    }
    if filter.missing_preview {
        clauses.push("has_preview = 0".to_string());
    }
    if filter.favorites_only {
        clauses.push("is_favorite = 1".to_string());
    }
    if !filter.include_hidden {
        clauses.push("is_hidden = 0".to_string());
    }
    if let Some(min) = filter.min_size {
        let i = push_int(&mut binds, min);
        clauses.push(format!("file_size >= ?{i}"));
    }
    if let Some(max) = filter.max_size {
        let i = push_int(&mut binds, max);
        clauses.push(format!("file_size <= ?{i}"));
    }
    if let Some(min) = filter.min_mtime {
        let i = push_int(&mut binds, min);
        clauses.push(format!("file_mtime >= ?{i}"));
    }
    if let Some(max) = filter.max_mtime {
        let i = push_int(&mut binds, max);
        clauses.push(format!("file_mtime <= ?{i}"));
    }
    if let Some(min) = filter.min_package_mtime {
        let i = push_int(&mut binds, min);
        clauses.push(format!("package_mtime >= ?{i}"));
    }
    if let Some(max) = filter.max_package_mtime {
        let i = push_int(&mut binds, max);
        clauses.push(format!("package_mtime <= ?{i}"));
    }

    // Group selected tags by namespace, emit one EXISTS clause per namespace.
    // Within a namespace: OR (any of the selected values match). Across
    // namespaces: AND (every selected facet must match). Tags without a colon
    // are skipped — they shouldn't exist in v4 but we don't want to panic if
    // a malformed value sneaks in.
    if !filter.tags.is_empty() {
        let mut by_ns: std::collections::BTreeMap<&str, Vec<&str>> =
            std::collections::BTreeMap::new();
        for tag in &filter.tags {
            if tag.contains(':') {
                let ns = tag.split(':').next().unwrap_or("");
                by_ns.entry(ns).or_default().push(tag.as_str());
            }
        }
        for tags_in_ns in by_ns.values() {
            let placeholders: Vec<String> = tags_in_ns
                .iter()
                .map(|t| {
                    let i = push_text(&mut binds, t.to_string());
                    format!("?{i}")
                })
                .collect();
            clauses.push(format!(
                "EXISTS (SELECT 1 FROM family_tags ft \
                 WHERE ft.family_id = packages.family_id \
                 AND ft.tag IN ({}))",
                placeholders.join(",")
            ));
        }
    }

    let where_clause = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    (where_clause, binds)
}

fn map_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

/// Translate the user-facing sort_by + sort_order strings into a SQL ORDER BY
/// clause. Always appends a stable tiebreaker (id) so the order is deterministic
/// when multiple rows share the primary sort key.
fn build_order_clause(filter: &QueryFilter) -> String {
    let sort_by = filter.sort_by.as_deref().unwrap_or("creator");
    // Default direction depends on the sort field: name/creator → asc;
    // size/dates → desc (newest/biggest first).
    let default_desc = matches!(sort_by, "size" | "mtime" | "package_mtime" | "scanned");
    let asc = match filter.sort_order.as_deref() {
        Some("asc") => true,
        Some("desc") => false,
        _ => !default_desc,
    };
    let dir = if asc { "ASC" } else { "DESC" };

    let primary = match sort_by {
        "name" => format!("package_name COLLATE NOCASE {dir}, creator COLLATE NOCASE ASC"),
        "size" => format!("file_size {dir}"),
        "mtime" => format!("file_mtime {dir}"),
        // Rows scanned before v6 have package_mtime=0; push those to the end
        // regardless of direction so the sort is useful before a rescan.
        "package_mtime" => format!("(package_mtime = 0) ASC, package_mtime {dir}"),
        // "Added" = the later of when we first indexed it and when the file
        // itself last changed on disk. This handles the case where a user
        // re-downloads or replaces a .var after the initial scan — it should
        // resurface as "newly added" even though the row was created earlier.
        "scanned" => format!("max(scanned_at, file_mtime) {dir}"),
        // Default & "creator"
        _ => format!("creator COLLATE NOCASE {dir}, package_name COLLATE NOCASE ASC"),
    };
    format!("ORDER BY {primary}, id ASC")
}

#[derive(Debug, Serialize)]
pub struct SearchResult {
    pub family_id: i64,
    /// `latest_package_id` for the family — what the UI navigates to when the
    /// user clicks a hit. `None` if the family has no rows in `packages`,
    /// which shouldn't happen in practice but is handled gracefully.
    pub package_id: Option<i64>,
    pub creator: String,
    pub package_name: String,
    /// The text we embedded. Surfaced so the UI can show a one-line snippet
    /// next to each hit ("why did this match?").
    pub purpose: Option<String>,
    /// Raw cosine score. Range varies by model; the UI should normalize
    /// within the result set if it wants to show a relevance bar.
    pub score: f32,
}

fn attach_package_ids(
    conn: &rusqlite::Connection,
    hits: Vec<embedding::search::SearchHit>,
) -> Result<Vec<SearchResult>, String> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }
    let ids_csv = hits
        .iter()
        .map(|h| h.family_id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, latest_package_id FROM package_family WHERE id IN ({ids_csv})"
    );
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let mut by_id: std::collections::HashMap<i64, Option<i64>> =
        std::collections::HashMap::new();
    let mut rows = stmt.query([]).map_err(map_err)?;
    while let Some(r) = rows.next().map_err(map_err)? {
        let fid: i64 = r.get(0).map_err(map_err)?;
        let pid: Option<i64> = r.get(1).map_err(map_err)?;
        by_id.insert(fid, pid);
    }
    Ok(hits
        .into_iter()
        .map(|h| SearchResult {
            package_id: by_id.get(&h.family_id).copied().unwrap_or(None),
            family_id: h.family_id,
            creator: h.creator,
            package_name: h.package_name,
            purpose: h.purpose,
            score: h.score,
        })
        .collect())
}

/// Semantic search over family embeddings. Returns top-N families ranked by
/// cosine similarity to the encoded query text. The DB lock is held across
/// the model encode (3-5 s on first call after launch, ~15 ms thereafter);
/// the setup-hook warm-up keeps that first call out of the user's hot path.
#[tauri::command]
pub async fn search_families(
    state: State<'_, AppState>,
    query: String,
    top_n: Option<usize>,
) -> Result<Vec<SearchResult>, String> {
    let db = state.db.clone();
    let n = top_n.unwrap_or(40).clamp(1, 500);
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock();
        let hits = embedding::search::search_text(&conn, SEARCH_MODEL, SEARCH_INPUT, &query, n)
            .map_err(|e| format!("{e:#}"))?;
        attach_package_ids(&conn, hits)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// "Find similar to this" — anchor by a package's family. Resolves the
/// package_id → family_id internally so the frontend doesn't need to track
/// family ids. Returns the family itself excluded from the top-N (per the
/// embedding module's behavior).
#[tauri::command]
pub async fn search_similar_families(
    state: State<'_, AppState>,
    package_id: i64,
    top_n: Option<usize>,
) -> Result<Vec<SearchResult>, String> {
    let db = state.db.clone();
    let n = top_n.unwrap_or(20).clamp(1, 500);
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock();
        let family_id: Option<i64> = conn
            .query_row(
                "SELECT family_id FROM packages WHERE id = ?1",
                params![package_id],
                |r| r.get(0),
            )
            .map_err(map_err)?;
        let Some(family_id) = family_id else {
            return Err(format!("package {package_id} has no family_id (untagged)"));
        };
        let hits = embedding::search::search_similar_to_family(
            &conn,
            SEARCH_MODEL,
            SEARCH_INPUT,
            family_id,
            n,
        )
        .map_err(|e| format!("{e:#}"))?;
        attach_package_ids(&conn, hits)
    })
    .await
    .map_err(|e| format!("join error: {e}"))?
}

/// Fetch full PackageRow data for a specific set of ids, preserving the
/// caller's order. Used by the semantic-search UI to materialize hits as
/// proper grid rows (with thumbnails, type, size, tags, etc.).
#[tauri::command]
pub fn get_packages_by_ids(
    state: State<'_, AppState>,
    ids: Vec<i64>,
) -> Result<Vec<PackageRow>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = state.db.lock();
    let ids_csv = ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT id, creator, package_name, version, license, program_version, description,
                package_type, content_count, dep_count, file_size, file_mtime, var_path,
                has_preview, is_favorite, is_hidden,
                hub_resource_id, hub_url, hub_title, hub_author, hub_category,
                hub_preview_url, hub_synced_at, hub_sync_state,
                scene_count, look_count, plugin_count, clothing_count,
                hair_count, pose_count, subscene_count,
                error, package_mtime,
                hub_billing_tier, hub_is_hub_hosted, hub_license,
                hub_lastmod, hub_external_url, hub_match_method,
                {TAGS_SUBQUERY} AS tags
         FROM packages WHERE id IN ({ids_csv})"
    );
    let mut stmt = conn.prepare(&sql).map_err(map_err)?;
    let mut rows: std::collections::HashMap<i64, PackageRow> = stmt
        .query_map([], |row| {
            Ok(PackageRow {
                id: row.get(0)?,
                creator: row.get(1)?,
                package_name: row.get(2)?,
                version: row.get(3)?,
                license: row.get(4)?,
                program_version: row.get(5)?,
                description: row.get(6)?,
                package_type: row.get(7)?,
                content_count: row.get(8)?,
                dep_count: row.get(9)?,
                file_size: row.get(10)?,
                file_mtime: row.get(11)?,
                var_path: row.get(12)?,
                has_preview: row.get::<_, i64>(13)? != 0,
                is_favorite: row.get::<_, i64>(14)? != 0,
                is_hidden: row.get::<_, i64>(15)? != 0,
                hub_resource_id: row.get(16)?,
                hub_url: row.get(17)?,
                hub_title: row.get(18)?,
                hub_author: row.get(19)?,
                hub_category: row.get(20)?,
                hub_preview_url: row.get(21)?,
                hub_synced_at: row.get(22)?,
                hub_sync_state: row.get(23)?,
                scene_count: row.get(24)?,
                look_count: row.get(25)?,
                plugin_count: row.get(26)?,
                clothing_count: row.get(27)?,
                hair_count: row.get(28)?,
                pose_count: row.get(29)?,
                subscene_count: row.get(30)?,
                error: row.get(31)?,
                package_mtime: row.get(32)?,
                hub_billing_tier: row.get(33)?,
                hub_is_hub_hosted: row.get(34)?,
                hub_license: row.get(35)?,
                hub_lastmod: row.get(36)?,
                hub_external_url: row.get(37)?,
                hub_match_method: row.get(38)?,
                tags: split_tags(row.get::<_, Option<String>>(39)?),
            })
        })
        .map_err(map_err)?
        .filter_map(|r| r.ok())
        .map(|p| (p.id, p))
        .collect();
    // Preserve the caller's order; drop any ids the DB didn't return
    // (e.g. deleted between calls). `remove` consumes the row out of the
    // HashMap so PackageRow doesn't need to be Clone.
    Ok(ids.into_iter().filter_map(|id| rows.remove(&id)).collect())
}
