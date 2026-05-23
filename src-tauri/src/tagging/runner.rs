//! Batched tagging runner. Pulls packages needing tagging from the index in
//! batches of N, builds JSONL, calls Grok, parses + validates the JSONL
//! response, writes results back in one transaction per batch.
//!
//! Idempotency / resumability: row state lives in
//! `packages.tagging_state` (`pending` after we claim it, `done`/`failed`
//! after we hear back). Re-running picks up any row where `tagging_state`
//! is NULL or `taxonomy_version` doesn't match the current run.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Deserialize;

use crate::tagging::{grok::GrokClient, prompt, record};

#[derive(Debug, Clone)]
pub struct RunnerConfig {
    pub taxonomy_version: String,
    pub model: String,
    pub batch_size: usize,
    pub rate_limit_ms: u64,
    /// Optional cap on rows processed (for pilots / smoke tests).
    pub limit: Option<usize>,
    /// Optional explicit id list; overrides the default queue.
    pub only_ids: Option<Vec<i64>>,
    /// If true, log what would be processed but make no API calls.
    pub dry_run: bool,
}

#[derive(Debug, Default)]
pub struct RunStats {
    pub batches: usize,
    pub records_sent: usize,
    pub records_done: usize,
    pub records_failed: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct GrokRecord {
    id: i64,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    purpose: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    notes: String,
}

#[derive(Debug, Deserialize)]
struct GrokResponse {
    records: Vec<GrokRecord>,
}

/// Run the tagging loop. Caller owns the connection (single-writer model).
/// `client` is None for dry-runs. Equivalent to `run_with_progress` with a
/// never-cancelled flag and a no-op progress sink — used by the CLI binary,
/// which surfaces progress via eprintln logs only.
pub fn run(
    conn: &Mutex<Connection>,
    client: Option<&GrokClient>,
    cfg: &RunnerConfig,
) -> Result<RunStats> {
    let cancel = AtomicBool::new(false);
    run_with_progress(conn, client, cfg, &cancel, |_| {})
}

/// Cancellable + progress-reporting variant. The GUI's `start_tagging_run`
/// command uses this to plumb a shared `AtomicBool` for stop signalling and
/// a closure that emits `tag-run-progress` events to the frontend. Progress
/// is emitted at the end of every batch (after the DB write commits), so the
/// `RunStats` snapshot passed to `on_progress` is the canonical running total.
///
/// Cancellation checkpoints: start of each loop iteration AND after each
/// rate-limit sleep. A cancel that arrives mid-API-call still completes that
/// batch (its DB write is committed) before the loop exits — clean stop, no
/// half-written batch.
pub fn run_with_progress<F: FnMut(&RunStats)>(
    conn: &Mutex<Connection>,
    client: Option<&GrokClient>,
    cfg: &RunnerConfig,
    cancel: &AtomicBool,
    mut on_progress: F,
) -> Result<RunStats> {
    let mut stats = RunStats::default();
    let valid_tags = load_valid_tags(&conn.lock())?;
    let taxonomy_json = prompt::fetch_taxonomy_json(&conn.lock())?;

    // Open a run audit row at start.
    let run_id = if !cfg.dry_run {
        Some(open_run(&conn.lock(), &cfg.taxonomy_version, &cfg.model)?)
    } else {
        None
    };

    let started = Instant::now();
    let mut processed_total = 0usize;

    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if let Some(cap) = cfg.limit {
            if processed_total >= cap {
                break;
            }
        }
        let remaining = cfg.limit.map(|c| c - processed_total);
        let want = remaining.map(|r| r.min(cfg.batch_size)).unwrap_or(cfg.batch_size);

        let batch = select_next_batch(&conn.lock(), &cfg.only_ids, &cfg.taxonomy_version, want)?;
        if batch.is_empty() {
            break;
        }

        // Build per-record JSONL. Skip any that fail to read (e.g. missing
        // .var on disk) — mark them as failed without an API call.
        let mut jsonl = String::new();
        let mut record_ids: Vec<i64> = Vec::with_capacity(batch.len());
        let mut build_failed: Vec<(i64, String)> = Vec::new();
        for (id, var_path) in &batch {
            match record::build_record(*id, var_path) {
                Ok(rec) => {
                    let line = serde_json::to_string(&rec)?;
                    jsonl.push_str(&line);
                    jsonl.push('\n');
                    record_ids.push(*id);
                }
                Err(e) => {
                    build_failed.push((*id, format!("build_record: {e:#}")));
                }
            }
        }

        eprintln!(
            "batch {:>3}: {} records (record_ids[0]={}, last={})",
            stats.batches + 1,
            record_ids.len(),
            record_ids.first().copied().unwrap_or(-1),
            record_ids.last().copied().unwrap_or(-1),
        );

        if cfg.dry_run {
            for (id, err) in &build_failed {
                eprintln!("  skip build {id}: {err}");
            }
            eprintln!("  [dry-run] would call Grok with {} records", record_ids.len());
            stats.batches += 1;
            stats.records_sent += record_ids.len();
            processed_total += record_ids.len() + build_failed.len();
            if cfg.rate_limit_ms > 0 {
                thread::sleep(Duration::from_millis(cfg.rate_limit_ms));
            }
            continue;
        }

        // Persist build failures first so the same ids aren't picked up
        // again on resume.
        if !build_failed.is_empty() {
            mark_failures(&conn.lock(), &build_failed, &cfg.taxonomy_version, &cfg.model)?;
            stats.records_failed += build_failed.len();
        }

        if record_ids.is_empty() {
            processed_total += build_failed.len();
            continue;
        }

        let client = client.ok_or_else(|| anyhow!("client required when not dry-run"))?;
        let user_msg = prompt::build_user_message(&taxonomy_json, &jsonl);
        let schema = prompt::response_format_schema();
        let result = client.complete(prompt::SYSTEM_PROMPT, &user_msg, Some(0.0), Some(&schema));

        match result {
            Ok(chat) => {
                if let Some(u) = chat.usage {
                    stats.prompt_tokens += u.prompt_tokens as u64;
                    stats.completion_tokens += u.completion_tokens as u64;
                }
                let parsed = parse_response(&chat.content, &record_ids, &valid_tags);
                write_batch(
                    &conn.lock(),
                    &parsed,
                    &record_ids,
                    &cfg.taxonomy_version,
                    &cfg.model,
                )?;
                stats.records_done += parsed.successes.len();
                stats.records_failed += parsed.failures.len();
                let elapsed = started.elapsed().as_secs();
                eprintln!(
                    "  done: {}/{} ok, {} failed; tokens prompt={} completion={}; elapsed {}s",
                    parsed.successes.len(),
                    record_ids.len(),
                    parsed.failures.len(),
                    stats.prompt_tokens,
                    stats.completion_tokens,
                    elapsed,
                );
            }
            Err(e) => {
                eprintln!("  batch failed: {e:#}");
                let failures: Vec<(i64, String)> = record_ids
                    .iter()
                    .map(|id| (*id, format!("api: {e:#}")))
                    .collect();
                mark_failures(&conn.lock(), &failures, &cfg.taxonomy_version, &cfg.model)?;
                stats.records_failed += failures.len();
            }
        }

        stats.batches += 1;
        stats.records_sent += record_ids.len();
        processed_total += record_ids.len() + build_failed.len();

        on_progress(&stats);

        if cfg.rate_limit_ms > 0 {
            sleep_with_cancel(cfg.rate_limit_ms, cancel);
        }
    }

    if let Some(rid) = run_id {
        close_run(&conn.lock(), rid, &stats)?;
    }

    Ok(stats)
}

/// Interruptible sleep — wakes within ~100ms of a cancel signal. Used between
/// batches so a Stop click during the rate-limit pause doesn't have to wait
/// out the full second.
fn sleep_with_cancel(ms: u64, cancel: &AtomicBool) {
    let mut remaining = ms;
    while remaining > 0 && !cancel.load(Ordering::Relaxed) {
        let chunk = remaining.min(100);
        thread::sleep(Duration::from_millis(chunk));
        remaining = remaining.saturating_sub(chunk);
    }
}

fn load_valid_tags(conn: &Connection) -> Result<HashSet<String>> {
    let mut stmt = conn.prepare("SELECT tag FROM taxonomy")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut set = HashSet::new();
    for r in rows {
        set.insert(r?);
    }
    Ok(set)
}

/// Pick the next `n` families to tag. Returns (family_id, var_path) where
/// var_path is the latest package's location — used by build_record to read
/// the .var content. With `only_ids` set, restrict to those family ids;
/// otherwise pull rows with NULL tagging_state or whose taxonomy_version
/// doesn't match the current target.
///
/// Hidden filtering happens on the latest package: if the user has hidden
/// the latest version, the family is skipped (matches the user's grid-view
/// expectation that hidden packages are out-of-scope for everything).
fn select_next_batch(
    conn: &Connection,
    only_ids: &Option<Vec<i64>>,
    taxonomy_version: &str,
    n: usize,
) -> Result<Vec<(i64, String)>> {
    if let Some(ids) = only_ids {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders: Vec<&str> = (0..ids.len()).map(|_| "?").collect();
        let sql = format!(
            "SELECT pf.id, p.var_path
               FROM package_family pf
               JOIN packages p ON p.id = pf.latest_package_id
              WHERE pf.id IN ({})
                AND p.error IS NULL
                AND (pf.tagging_state IS NULL
                     OR pf.tagging_state = 'pending'
                     OR pf.taxonomy_version IS NULL
                     OR pf.taxonomy_version <> ?)
              ORDER BY pf.id
              LIMIT ?",
            placeholders.join(",")
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut binds: Vec<rusqlite::types::Value> = Vec::with_capacity(ids.len() + 2);
        for id in ids {
            binds.push(rusqlite::types::Value::Integer(*id));
        }
        binds.push(rusqlite::types::Value::Text(taxonomy_version.to_string()));
        binds.push(rusqlite::types::Value::Integer(n as i64));
        let params_ref: Vec<&dyn rusqlite::ToSql> =
            binds.iter().map(|v| v as &dyn rusqlite::ToSql).collect();
        let rows = stmt
            .query_map(params_ref.as_slice(), |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(rows);
    }

    let mut stmt = conn.prepare(
        "SELECT pf.id, p.var_path
           FROM package_family pf
           JOIN packages p ON p.id = pf.latest_package_id
          WHERE p.error IS NULL
            AND p.is_hidden = 0
            AND (pf.tagging_state IS NULL
                 OR pf.taxonomy_version IS NULL
                 OR pf.taxonomy_version <> ?1)
          ORDER BY pf.id
          LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![taxonomy_version, n as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[derive(Debug)]
struct ParsedBatch {
    successes: HashMap<i64, GrokRecord>,
    failures: Vec<(i64, String)>,
}

/// Parse Grok's response. With xAI's strict JSON-schema response_format,
/// the content is guaranteed to be a single JSON object matching the
/// schema — no markdown wrapping, no malformed lines, no missing fields.
/// We still validate id membership (expected vs returned) and filter tags
/// against the live taxonomy.
fn parse_response(
    content: &str,
    expected_ids: &[i64],
    valid_tags: &HashSet<String>,
) -> ParsedBatch {
    let expected: HashSet<i64> = expected_ids.iter().copied().collect();
    let mut successes: HashMap<i64, GrokRecord> = HashMap::new();
    let mut failures: Vec<(i64, String)> = Vec::new();

    let parsed: GrokResponse = match serde_json::from_str(content) {
        Ok(p) => p,
        Err(e) => {
            // Strict mode failure is rare but possible if schema rejected,
            // server hiccup, etc. Mark the whole batch failed.
            eprintln!(
                "  response JSON parse failed: {e}; head: {}",
                content.chars().take(200).collect::<String>()
            );
            for id in expected_ids {
                failures.push((*id, format!("response parse failed: {e}")));
            }
            return ParsedBatch { successes, failures };
        }
    };

    for rec in parsed.records {
        if !expected.contains(&rec.id) {
            eprintln!("  unexpected id {} in response, dropping", rec.id);
            continue;
        }
        // v4: KEEP unknown tags. Grok is allowed to propose new namespace
        // values inline (e.g. type:nostril-darkener) and even new
        // namespaces (era:victorian) — those proposals only inform future
        // taxonomy iterations if we keep them. Log them so we can grep the
        // run output for review.
        let unknowns: Vec<&String> = rec
            .tags
            .iter()
            .filter(|t| !valid_tags.contains(*t))
            .collect();
        if !unknowns.is_empty() {
            eprintln!(
                "  id {}: {} new tag(s) proposed: {}",
                rec.id,
                unknowns.len(),
                unknowns.iter().take(5).map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            );
        }
        successes.insert(rec.id, rec);
    }

    for id in expected_ids {
        if !successes.contains_key(id) {
            failures.push((*id, "no record in response".to_string()));
        }
    }

    ParsedBatch { successes, failures }
}

fn write_batch(
    conn: &Connection,
    parsed: &ParsedBatch,
    expected_ids: &[i64],
    taxonomy_version: &str,
    model: &str,
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // One transaction per batch so a mid-batch crash leaves no half-state.
    // Writes target package_family + family_tags (v11+). The legacy
    // packages.tagging_* columns are not touched — they retain their v3 era
    // values as historical record.
    let tx = conn.unchecked_transaction()?;
    {
        let mut up_done = tx.prepare_cached(
            "UPDATE package_family
                SET purpose = ?1,
                    out_of_scope = ?2,
                    tagging_state = 'done',
                    tagging_model = ?3,
                    taxonomy_version = ?4,
                    tagged_at = ?5,
                    tagging_error = NULL,
                    tagging_suggested_new_tag = ?6,
                    tagging_notes = ?7
              WHERE id = ?8",
        )?;
        let mut up_fail = tx.prepare_cached(
            "UPDATE package_family
                SET tagging_state = 'failed',
                    tagging_model = ?1,
                    taxonomy_version = ?2,
                    tagged_at = ?3,
                    tagging_error = ?4
              WHERE id = ?5",
        )?;
        let mut del_tags = tx.prepare_cached(
            "DELETE FROM family_tags WHERE family_id = ?1",
        )?;
        let mut ins_tag = tx.prepare_cached(
            "INSERT OR IGNORE INTO family_tags(family_id, tag) VALUES (?1, ?2)",
        )?;

        for id in expected_ids {
            if let Some(rec) = parsed.successes.get(id) {
                // xAI strict schema doesn't allow nullable strings under
                // additionalProperties=false, so empty strings carry the
                // "no value" sentinel — collapse them to SQL NULL on write
                // so downstream filters can use `IS NULL` cleanly.
                let nullify = |s: &str| -> Option<String> {
                    let t = s.trim();
                    if t.is_empty() {
                        None
                    } else {
                        Some(s.to_string())
                    }
                };
                let purpose = nullify(&rec.purpose);
                let notes = nullify(&rec.notes);

                // v4 collapses the out_of_scope binary — every family has a
                // kind: tag instead. Set the column to 0 for everything;
                // legacy code that filters on out_of_scope still works but
                // returns no OOS rows, which is the correct semantics now.
                up_done.execute(params![
                    purpose,
                    0_i64,
                    model,
                    taxonomy_version,
                    now,
                    Option::<String>::None,   // tagging_suggested_new_tag — v4 has no such field
                    notes,
                    id,
                ])?;
                del_tags.execute(params![id])?;

                // Insert the kind: tag first, then all other tags. The kind
                // value from Grok is the full namespaced string per the
                // strict-enum schema (e.g. "kind:character-look"), so we
                // store it directly. Tolerate missing prefix as a belt-and-
                // suspenders against future schema drift.
                let kind_tag = if rec.kind.starts_with("kind:") {
                    rec.kind.clone()
                } else if !rec.kind.trim().is_empty() {
                    format!("kind:{}", rec.kind.trim())
                } else {
                    String::new()
                };
                if !kind_tag.is_empty() {
                    ins_tag.execute(params![id, kind_tag])?;
                }
                for tag in &rec.tags {
                    let t = tag.trim();
                    if t.is_empty() || t == kind_tag {
                        continue;
                    }
                    ins_tag.execute(params![id, t])?;
                }
            } else {
                let err = parsed
                    .failures
                    .iter()
                    .find(|(fid, _)| fid == id)
                    .map(|(_, e)| e.as_str())
                    .unwrap_or("unknown");
                up_fail.execute(params![model, taxonomy_version, now, err, id])?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}

fn mark_failures(
    conn: &Connection,
    failures: &[(i64, String)],
    taxonomy_version: &str,
    model: &str,
) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "UPDATE package_family
                SET tagging_state = 'failed',
                    tagging_model = ?1,
                    taxonomy_version = ?2,
                    tagged_at = ?3,
                    tagging_error = ?4
              WHERE id = ?5",
        )?;
        for (id, err) in failures {
            stmt.execute(params![model, taxonomy_version, now, err, id])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn open_run(conn: &Connection, taxonomy_version: &str, model: &str) -> Result<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO tagging_runs(started_at, taxonomy_version, model) VALUES (?1, ?2, ?3)",
        params![now, taxonomy_version, model],
    )?;
    Ok(conn.last_insert_rowid())
}

fn close_run(conn: &Connection, run_id: i64, stats: &RunStats) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    conn.execute(
        "UPDATE tagging_runs
            SET completed_at = ?1,
                total = ?2,
                succeeded = ?3,
                failed = ?4
          WHERE id = ?5",
        params![
            now,
            stats.records_sent as i64,
            stats.records_done as i64,
            stats.records_failed as i64,
            run_id,
        ],
    )?;
    Ok(())
}
