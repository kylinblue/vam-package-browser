//! Tauri commands for the in-app classifier pipeline (tagger + embedder).
//! Mirrors the hub-sync command pattern in `commands.rs`:
//!   - `*_status`              read-only stats for the UI badge / banner
//!   - `*_active`              boolean, survives HMR so reloaded UI knows a
//!                             background run is still going
//!   - `start_*_run`           spawn_blocking, emits progress events
//!   - `stop_*_run`            sets cancel flag, runner notices at the next
//!                             checkpoint
//!
//! API key plumbing piggybacks on `app_settings.xai_api_key` — the same row
//! `tag_library --set-api-key` writes to, so a key set from either path is
//! visible to both.

use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

use crate::embedding::{runner as embed_runner, InputKind, ModelChoice};
use crate::tagging::{family, grok::GrokClient, runner as tag_runner};
use crate::{index, AppState};

const SETTING_API_KEY: &str = "xai_api_key";
const DEFAULT_TAXONOMY_VERSION: &str = "v4";
const DEFAULT_MODEL: &str = "grok-4.3";
const DEFAULT_BATCH_SIZE: usize = 100;
const DEFAULT_RATE_LIMIT_MS: u64 = 1000;

// ===== Status =====================================================

#[derive(Debug, Serialize)]
pub struct TaggingStatus {
    pub has_api_key: bool,
    pub api_key_length: usize,
    pub taxonomy_seeded: bool,
    pub taxonomy_active: i64,
    pub families_total: i64,
    pub families_pending: i64,
    pub families_done: i64,
    pub families_failed: i64,
    pub taxonomy_version: String,
}

#[tauri::command]
pub fn tagging_status(state: State<'_, AppState>) -> Result<TaggingStatus, String> {
    let conn = state.db.lock();
    let api_key = index::get_setting(&conn, SETTING_API_KEY).map_err(map_err)?;
    let api_key_length = api_key.as_deref().map(|s| s.len()).unwrap_or(0);
    let taxonomy_active: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM taxonomy WHERE is_active = 1",
            [],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    let families_total: i64 = conn
        .query_row("SELECT COUNT(*) FROM package_family", [], |r| r.get(0))
        .map_err(map_err)?;
    // "Pending" matches what `tag_library` and `runner::select_next_batch`
    // actually pick up — NULL state OR a taxonomy_version stale vs current.
    let families_pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM package_family
              WHERE tagging_state IS NULL
                 OR tagging_state = 'pending'
                 OR taxonomy_version IS NULL
                 OR taxonomy_version <> ?1",
            [DEFAULT_TAXONOMY_VERSION],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    let families_done: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM package_family WHERE tagging_state = 'done'",
            [],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    let families_failed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM package_family WHERE tagging_state = 'failed'",
            [],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    Ok(TaggingStatus {
        has_api_key: !api_key.as_deref().unwrap_or("").is_empty(),
        api_key_length,
        taxonomy_seeded: taxonomy_active > 0,
        taxonomy_active,
        families_total,
        families_pending,
        families_done,
        families_failed,
        taxonomy_version: DEFAULT_TAXONOMY_VERSION.to_string(),
    })
}

#[derive(Debug, Serialize)]
pub struct EmbeddingStatus {
    pub families_with_purpose: i64,
    /// Count of families with a purpose but no embedding for the search
    /// variant (nomic + purpose). This is the "needs embedding" number
    /// surfaced as the badge on the Embed-now button.
    pub families_missing_embedding: i64,
    pub families_embedded: i64,
    pub model: String,
    pub input_kind: String,
}

#[tauri::command]
pub fn embedding_status(state: State<'_, AppState>) -> Result<EmbeddingStatus, String> {
    let conn = state.db.lock();
    let model = ModelChoice::NomicEmbedTextV15;
    let input = InputKind::Purpose;
    let families_with_purpose: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM package_family
              WHERE purpose IS NOT NULL AND TRIM(purpose) <> ''",
            [],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    let families_embedded: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM family_embeddings WHERE model = ?1 AND input_kind = ?2",
            [model.name(), input.name()],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    let families_missing_embedding: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM package_family pf
               LEFT JOIN family_embeddings fe
                 ON fe.family_id = pf.id AND fe.model = ?1 AND fe.input_kind = ?2
              WHERE pf.purpose IS NOT NULL
                AND TRIM(pf.purpose) <> ''
                AND fe.family_id IS NULL",
            [model.name(), input.name()],
            |r| r.get(0),
        )
        .map_err(map_err)?;
    Ok(EmbeddingStatus {
        families_with_purpose,
        families_missing_embedding,
        families_embedded,
        model: model.name().to_string(),
        input_kind: input.name().to_string(),
    })
}

// ===== API key ====================================================

#[tauri::command]
pub fn set_xai_api_key(state: State<'_, AppState>, key: String) -> Result<(), String> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return Err("api key is empty".into());
    }
    let conn = state.db.lock();
    index::set_setting(&conn, SETTING_API_KEY, trimmed).map_err(map_err)?;
    Ok(())
}

#[tauri::command]
pub fn clear_xai_api_key(state: State<'_, AppState>) -> Result<(), String> {
    let conn = state.db.lock();
    conn.execute("DELETE FROM app_settings WHERE key = ?1", [SETTING_API_KEY])
        .map_err(map_err)?;
    Ok(())
}

// ===== Run lifecycle ==============================================

#[tauri::command]
pub fn tagging_active(state: State<'_, AppState>) -> bool {
    state.tagging_running.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn embedding_active(state: State<'_, AppState>) -> bool {
    state.embedding_running.load(Ordering::Relaxed)
}

#[tauri::command]
pub fn stop_tagging_run(state: State<'_, AppState>) {
    state.tagging_cancel.store(true, Ordering::Relaxed);
}

#[tauri::command]
pub fn stop_embedding_run(state: State<'_, AppState>) {
    state.embedding_cancel.store(true, Ordering::Relaxed);
}

#[derive(Debug, Default, Deserialize)]
pub struct TaggingRunOptions {
    pub taxonomy_version: Option<String>,
    pub model: Option<String>,
    pub batch_size: Option<usize>,
    pub rate_limit_ms: Option<u64>,
    pub limit: Option<usize>,
    /// If set, restrict the run to these family ids (passed straight through
    /// to `RunnerConfig.only_ids`). Untouched families are not selected.
    pub only_ids: Option<Vec<i64>>,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct TaggingProgress {
    pub batches: usize,
    pub records_sent: usize,
    pub records_done: usize,
    pub records_failed: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// "running" while the loop is alive; one final "completed", "cancelled",
    /// or "failed" event is emitted after the loop exits.
    pub state: String,
    /// Populated on terminal events ("failed") so the UI can show the cause.
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TaggingRunSummary {
    pub batches: usize,
    pub records_sent: usize,
    pub records_done: usize,
    pub records_failed: usize,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cancelled: bool,
}

#[tauri::command]
pub async fn start_tagging_run(
    state: State<'_, AppState>,
    app: AppHandle,
    options: TaggingRunOptions,
) -> Result<TaggingRunSummary, String> {
    // Reject overlapping runs cleanly — the UI button should already be
    // disabled, but a duplicate request shouldn't double-spawn the worker.
    if state
        .tagging_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("a tagging run is already in progress".into());
    }
    state.tagging_cancel.store(false, Ordering::Relaxed);

    let db = state.db.clone();
    let cancel = state.tagging_cancel.clone();
    let running = state.tagging_running.clone();
    let app_for_emit = app.clone();

    let result = tauri::async_runtime::spawn_blocking(move || {
        let cfg = tag_runner::RunnerConfig {
            taxonomy_version: options
                .taxonomy_version
                .unwrap_or_else(|| DEFAULT_TAXONOMY_VERSION.to_string()),
            model: options
                .model
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            batch_size: options.batch_size.unwrap_or(DEFAULT_BATCH_SIZE),
            rate_limit_ms: options.rate_limit_ms.unwrap_or(DEFAULT_RATE_LIMIT_MS),
            limit: options.limit,
            only_ids: options.only_ids,
            dry_run: options.dry_run,
        };

        // Pre-flight: taxonomy must be seeded. (Same check `tag_library` does.)
        {
            let conn = db.lock();
            let taxonomy_active: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM taxonomy WHERE is_active = 1",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(map_err)?;
            if taxonomy_active == 0 {
                return Err(
                    "taxonomy is empty — seed it with `tag_library --seed-taxonomy <path>` \
                     before starting a run from the GUI"
                        .to_string(),
                );
            }
        }

        // Client: skip for dry-runs, otherwise pull the key from settings.
        let client = if cfg.dry_run {
            None
        } else {
            let key = {
                let conn = db.lock();
                index::get_setting(&conn, SETTING_API_KEY)
                    .map_err(map_err)?
                    .unwrap_or_default()
            };
            if key.trim().is_empty() {
                return Err(
                    "no xAI API key configured — set it via the API key form in the Tagged view"
                        .to_string(),
                );
            }
            Some(GrokClient::new(key, cfg.model.clone()))
        };

        let app_for_progress = app_for_emit.clone();
        let on_progress = |stats: &tag_runner::RunStats| {
            let _ = app_for_progress.emit(
                "tag-run-progress",
                &TaggingProgress {
                    batches: stats.batches,
                    records_sent: stats.records_sent,
                    records_done: stats.records_done,
                    records_failed: stats.records_failed,
                    prompt_tokens: stats.prompt_tokens,
                    completion_tokens: stats.completion_tokens,
                    state: "running".to_string(),
                    error: None,
                },
            );
        };

        let res = tag_runner::run_with_progress(&db, client.as_ref(), &cfg, &cancel, on_progress);
        let cancelled = cancel.load(Ordering::Relaxed);

        match res {
            Ok(stats) => {
                let summary = TaggingRunSummary {
                    batches: stats.batches,
                    records_sent: stats.records_sent,
                    records_done: stats.records_done,
                    records_failed: stats.records_failed,
                    prompt_tokens: stats.prompt_tokens,
                    completion_tokens: stats.completion_tokens,
                    cancelled,
                };
                let _ = app_for_emit.emit(
                    "tag-run-progress",
                    &TaggingProgress {
                        batches: stats.batches,
                        records_sent: stats.records_sent,
                        records_done: stats.records_done,
                        records_failed: stats.records_failed,
                        prompt_tokens: stats.prompt_tokens,
                        completion_tokens: stats.completion_tokens,
                        state: if cancelled { "cancelled" } else { "completed" }.to_string(),
                        error: None,
                    },
                );
                Ok(summary)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let _ = app_for_emit.emit(
                    "tag-run-progress",
                    &TaggingProgress {
                        batches: 0,
                        records_sent: 0,
                        records_done: 0,
                        records_failed: 0,
                        prompt_tokens: 0,
                        completion_tokens: 0,
                        state: "failed".to_string(),
                        error: Some(msg.clone()),
                    },
                );
                Err(msg)
            }
        }
    })
    .await;

    running.store(false, Ordering::Relaxed);

    match result {
        Ok(inner) => inner,
        Err(join_err) => Err(format!("worker join error: {join_err}")),
    }
}

// ===== Embedding run ==============================================

#[derive(Debug, Default, Deserialize)]
pub struct EmbeddingRunOptions {
    pub limit: Option<usize>,
    pub batch_size: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
pub struct EmbeddingProgress {
    pub candidates: usize,
    pub embedded: usize,
    pub skipped_empty: usize,
    pub state: String,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct EmbeddingRunSummary {
    pub model: String,
    pub input_kind: String,
    pub candidates: usize,
    pub embedded: usize,
    pub skipped_empty: usize,
    pub elapsed_secs: f64,
    pub cancelled: bool,
}

const EMBED_DEFAULT_BATCH_SIZE: usize = 32;

#[tauri::command]
pub async fn start_embedding_run(
    state: State<'_, AppState>,
    app: AppHandle,
    options: EmbeddingRunOptions,
) -> Result<EmbeddingRunSummary, String> {
    if state
        .embedding_running
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return Err("an embedding run is already in progress".into());
    }
    state.embedding_cancel.store(false, Ordering::Relaxed);

    let db = state.db.clone();
    let cancel = state.embedding_cancel.clone();
    let running = state.embedding_running.clone();
    let app_for_emit = app.clone();

    let result = tauri::async_runtime::spawn_blocking(move || {
        let model = ModelChoice::NomicEmbedTextV15;
        let input = InputKind::Purpose;
        let batch_size = options.batch_size.unwrap_or(EMBED_DEFAULT_BATCH_SIZE);

        let app_for_progress = app_for_emit.clone();
        let on_progress = |stats: &embed_runner::EmbedRunStats| {
            let _ = app_for_progress.emit(
                "embed-run-progress",
                &EmbeddingProgress {
                    candidates: stats.candidates,
                    embedded: stats.embedded,
                    skipped_empty: stats.skipped_empty,
                    state: "running".to_string(),
                    error: None,
                },
            );
        };

        let res = embed_runner::embed_missing_with_progress(
            &db,
            model,
            input,
            options.limit,
            batch_size,
            &cancel,
            on_progress,
        );
        let cancelled = cancel.load(Ordering::Relaxed);

        match res {
            Ok(stats) => {
                let summary = EmbeddingRunSummary {
                    model: stats.model.clone(),
                    input_kind: stats.input_kind.clone(),
                    candidates: stats.candidates,
                    embedded: stats.embedded,
                    skipped_empty: stats.skipped_empty,
                    elapsed_secs: stats.elapsed_secs,
                    cancelled,
                };
                let _ = app_for_emit.emit(
                    "embed-run-progress",
                    &EmbeddingProgress {
                        candidates: stats.candidates,
                        embedded: stats.embedded,
                        skipped_empty: stats.skipped_empty,
                        state: if cancelled { "cancelled" } else { "completed" }.to_string(),
                        error: None,
                    },
                );
                Ok(summary)
            }
            Err(e) => {
                let msg = format!("{e:#}");
                let _ = app_for_emit.emit(
                    "embed-run-progress",
                    &EmbeddingProgress {
                        candidates: 0,
                        embedded: 0,
                        skipped_empty: 0,
                        state: "failed".to_string(),
                        error: Some(msg.clone()),
                    },
                );
                Err(msg)
            }
        }
    })
    .await;

    running.store(false, Ordering::Relaxed);

    match result {
        Ok(inner) => inner,
        Err(join_err) => Err(format!("worker join error: {join_err}")),
    }
}

// ===== Manual family recompute (rarely needed; scanner auto-runs it) ====

#[derive(Debug, Serialize)]
pub struct RecomputeFamiliesSummary {
    pub families_before: i64,
    pub families_after: i64,
    pub families_added: i64,
    pub packages_linked_this_run: usize,
    pub families_with_latest: i64,
    pub families_inheriting_tags: usize,
    pub family_tag_rows_added: usize,
}

#[tauri::command]
pub fn recompute_families(
    state: State<'_, AppState>,
) -> Result<RecomputeFamiliesSummary, String> {
    let conn = state.db.lock();
    let stats = family::recompute(&conn).map_err(map_err)?;
    Ok(RecomputeFamiliesSummary {
        families_before: stats.families_before,
        families_after: stats.families_after,
        families_added: stats.families_added,
        packages_linked_this_run: stats.packages_linked_this_run,
        families_with_latest: stats.families_with_latest,
        families_inheriting_tags: stats.families_inheriting_tags,
        family_tag_rows_added: stats.family_tag_rows_added,
    })
}

fn map_err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}
