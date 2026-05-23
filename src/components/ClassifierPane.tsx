import { useCallback, useEffect, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  clearXaiApiKey,
  embeddingActive,
  embeddingStatus,
  setXaiApiKey,
  startEmbeddingRun,
  startTaggingRun,
  stopEmbeddingRun,
  stopTaggingRun,
  taggingActive,
  taggingStatus,
  type EmbeddingProgress,
  type EmbeddingStatus,
  type TaggingProgress,
  type TaggingStatus,
} from "../lib/api";

interface Props {
  /// Called after a tag/embed run reaches a terminal state ("completed",
  /// "cancelled", or "failed") so the parent can refresh facet counts +
  /// re-query the grid to reflect newly-applied tags.
  onRunComplete?: () => void;
  /// Bumped by the parent after an event the pane can't observe directly
  /// — most importantly a `scan_library` finish, which can add new
  /// `package_family` rows with `tagging_state IS NULL`. The pane refetches
  /// `tagging_status` + `embedding_status` whenever this value changes.
  refreshNonce?: number;
}

/// "Tagged" mode dashboard: pending-work counters, run launchers, and
/// xAI API key entry. Sits in the toolbar (mirrors HubSyncView for the
/// Fetched mode) so it's visible whenever the user is browsing tagged
/// content.
///
/// Backend status refresh: pulled on mount, on visibility re-show, and
/// after every terminal run event. We don't poll on a timer — the only
/// other writers (CLI tag_library, the scanner's family-recompute) are
/// not running concurrently with the GUI under the project's honor-system
/// DB-lock protocol (CLAUDE.md: Database access protocol).
export function ClassifierPane({ onRunComplete, refreshNonce }: Props) {
  const [tagStatus, setTagStatus] = useState<TaggingStatus | null>(null);
  const [embedStatus, setEmbedStatus] = useState<EmbeddingStatus | null>(null);
  const [statusErr, setStatusErr] = useState<string | null>(null);

  const [tagRunning, setTagRunning] = useState(false);
  const [embedRunning, setEmbedRunning] = useState(false);
  const [tagProgress, setTagProgress] = useState<TaggingProgress | null>(null);
  const [embedProgress, setEmbedProgress] = useState<EmbeddingProgress | null>(null);
  const [opErr, setOpErr] = useState<string | null>(null);

  const [showKeyForm, setShowKeyForm] = useState(false);
  const [keyInput, setKeyInput] = useState("");
  const [keyBusy, setKeyBusy] = useState(false);

  // Tag-run advanced overrides. Defaults match the backend RunnerConfig
  // defaults so an empty form sends the same thing as `tag_library` would.
  const [tagLimitInput, setTagLimitInput] = useState("");
  const [showAdvanced, setShowAdvanced] = useState(false);

  const refreshStatus = useCallback(async () => {
    try {
      const [t, e] = await Promise.all([taggingStatus(), embeddingStatus()]);
      setTagStatus(t);
      setEmbedStatus(e);
      setStatusErr(null);
    } catch (err) {
      setStatusErr(String(err));
    }
  }, []);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus, refreshNonce]);

  // HMR recovery: if a run is still going in the backend after a frontend
  // reload, flip the running flags so the UI reflects it. The next progress
  // event will populate the bars.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [t, e] = await Promise.all([taggingActive(), embeddingActive()]);
        if (cancelled) return;
        if (t) setTagRunning(true);
        if (e) setEmbedRunning(true);
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  useEffect(() => {
    let unlistenTag: UnlistenFn | undefined;
    let unlistenEmbed: UnlistenFn | undefined;
    let cancelled = false;
    (async () => {
      const ut = await listen<TaggingProgress>("tag-run-progress", (event) => {
        const p = event.payload;
        setTagProgress(p);
        if (p.state === "completed" || p.state === "cancelled" || p.state === "failed") {
          setTagRunning(false);
          refreshStatus().catch(() => {});
          if (p.state === "failed" && p.error) {
            setOpErr(`tag run failed: ${p.error}`);
          }
          if (onRunComplete) onRunComplete();
        }
      });
      const ue = await listen<EmbeddingProgress>("embed-run-progress", (event) => {
        const p = event.payload;
        setEmbedProgress(p);
        if (p.state === "completed" || p.state === "cancelled" || p.state === "failed") {
          setEmbedRunning(false);
          refreshStatus().catch(() => {});
          if (p.state === "failed" && p.error) {
            setOpErr(`embed run failed: ${p.error}`);
          }
          if (onRunComplete) onRunComplete();
        }
      });
      if (cancelled) {
        ut();
        ue();
      } else {
        unlistenTag = ut;
        unlistenEmbed = ue;
      }
    })();
    return () => {
      cancelled = true;
      if (unlistenTag) unlistenTag();
      if (unlistenEmbed) unlistenEmbed();
    };
  }, [refreshStatus, onRunComplete]);

  const onStartTagging = useCallback(async () => {
    setOpErr(null);
    setTagProgress(null);
    setTagRunning(true);
    try {
      const limit = parseFinitePositive(tagLimitInput);
      await startTaggingRun(limit !== undefined ? { limit } : {});
      // Resolution arrives via the terminal "completed"/"cancelled" event.
    } catch (e) {
      setOpErr(String(e));
      setTagRunning(false);
    }
  }, [tagLimitInput]);

  const onStopTagging = useCallback(async () => {
    try {
      await stopTaggingRun();
    } catch (e) {
      setOpErr(String(e));
    }
  }, []);

  const onStartEmbedding = useCallback(async () => {
    setOpErr(null);
    setEmbedProgress(null);
    setEmbedRunning(true);
    try {
      await startEmbeddingRun({});
    } catch (e) {
      setOpErr(String(e));
      setEmbedRunning(false);
    }
  }, []);

  const onStopEmbedding = useCallback(async () => {
    try {
      await stopEmbeddingRun();
    } catch (e) {
      setOpErr(String(e));
    }
  }, []);

  const onSaveKey = useCallback(async () => {
    setKeyBusy(true);
    setOpErr(null);
    try {
      await setXaiApiKey(keyInput);
      setKeyInput("");
      setShowKeyForm(false);
      await refreshStatus();
    } catch (e) {
      setOpErr(String(e));
    } finally {
      setKeyBusy(false);
    }
  }, [keyInput, refreshStatus]);

  const onClearKey = useCallback(async () => {
    setKeyBusy(true);
    setOpErr(null);
    try {
      await clearXaiApiKey();
      await refreshStatus();
    } catch (e) {
      setOpErr(String(e));
    } finally {
      setKeyBusy(false);
    }
  }, [refreshStatus]);

  const pending = tagStatus?.families_pending ?? 0;
  const missingEmbed = embedStatus?.families_missing_embedding ?? 0;
  const hasKey = tagStatus?.has_api_key ?? false;
  const taxonomySeeded = tagStatus?.taxonomy_seeded ?? false;

  const tagBlocker = !hasKey
    ? "Set the xAI API key first"
    : !taxonomySeeded
      ? "Seed taxonomy via `tag_library --seed-taxonomy`"
      : pending === 0
        ? "Nothing pending"
        : null;

  return (
    <div className="classifier-pane">
      <div className="classifier-row">
        <span className="classifier-title">Tagged</span>
        <span className="classifier-stat" title="Families with NULL/stale tagging state">
          <strong>{pending.toLocaleString()}</strong> need tagging
        </span>
        <span
          className="classifier-stat"
          title={`Families with purpose text but no ${embedStatus?.model ?? "nomic"} embedding for ${embedStatus?.input_kind ?? "purpose"}`}
        >
          <strong>{missingEmbed.toLocaleString()}</strong> need embedding
        </span>
        {tagStatus && (
          <span className="classifier-stat-dim" title="Families fully tagged at the current taxonomy version">
            ({tagStatus.families_done.toLocaleString()} done / {tagStatus.families_total.toLocaleString()} total
            {tagStatus.families_failed > 0 ? `, ${tagStatus.families_failed} failed` : ""})
          </span>
        )}

        <div className="classifier-actions">
          {tagRunning ? (
            <button onClick={onStopTagging} className="classifier-btn classifier-btn-stop">
              Stop tagging
            </button>
          ) : (
            <button
              onClick={onStartTagging}
              disabled={!!tagBlocker}
              title={tagBlocker ?? `Tag ${pending} families via Grok`}
              className="classifier-btn"
            >
              Tag now
            </button>
          )}
          {embedRunning ? (
            <button onClick={onStopEmbedding} className="classifier-btn classifier-btn-stop">
              Stop embedding
            </button>
          ) : (
            <button
              onClick={onStartEmbedding}
              disabled={missingEmbed === 0}
              title={
                missingEmbed === 0
                  ? "No families need embedding"
                  : `Embed ${missingEmbed} families (local fastembed)`
              }
              className="classifier-btn"
            >
              Embed now
            </button>
          )}
          <button
            type="button"
            onClick={() => setShowAdvanced((v) => !v)}
            className="classifier-btn-link"
            title="Show advanced options (limit, batch size)"
          >
            {showAdvanced ? "less" : "more"}
          </button>
          <button
            type="button"
            onClick={() => setShowKeyForm((v) => !v)}
            className="classifier-btn-link"
            title={
              hasKey
                ? `xAI API key configured (length ${tagStatus?.api_key_length ?? 0})`
                : "xAI API key not configured — click to set"
            }
          >
            {hasKey ? "✓ API key" : "⚠ Set API key"}
          </button>
        </div>
      </div>

      {showAdvanced && (
        <div className="classifier-row classifier-row-sub">
          <label className="classifier-field">
            <span>Limit</span>
            <input
              type="number"
              min={1}
              step={1}
              placeholder="(all)"
              value={tagLimitInput}
              onChange={(e) => setTagLimitInput(e.target.value)}
              disabled={tagRunning}
              style={{ width: 90 }}
            />
          </label>
          <span className="classifier-stat-dim">
            Taxonomy {tagStatus?.taxonomy_version ?? "?"} · {tagStatus?.taxonomy_active ?? 0} active tags
          </span>
        </div>
      )}

      {showKeyForm && (
        <div className="classifier-row classifier-row-sub">
          <span className="classifier-stat-dim">
            xAI API key{" "}
            {hasKey
              ? `(configured, length ${tagStatus?.api_key_length})`
              : "(not set)"}
          </span>
          <input
            type="password"
            placeholder="xai-..."
            value={keyInput}
            onChange={(e) => setKeyInput(e.target.value)}
            disabled={keyBusy}
            style={{ flex: "0 1 320px" }}
            autoComplete="off"
          />
          <button
            onClick={onSaveKey}
            disabled={keyBusy || keyInput.trim().length === 0}
            className="classifier-btn"
          >
            {hasKey ? "Replace" : "Save"}
          </button>
          {hasKey && (
            <button
              onClick={onClearKey}
              disabled={keyBusy}
              className="classifier-btn classifier-btn-stop"
            >
              Clear
            </button>
          )}
          <button
            type="button"
            onClick={() => {
              setShowKeyForm(false);
              setKeyInput("");
            }}
            className="classifier-btn-link"
          >
            cancel
          </button>
        </div>
      )}

      {tagRunning && tagProgress && (
        <div className="classifier-row classifier-row-sub">
          <span className="classifier-stat-dim">
            Tagging — batch {tagProgress.batches}, {tagProgress.records_done} done
            {tagProgress.records_failed > 0 ? `, ${tagProgress.records_failed} failed` : ""}
            {tagProgress.prompt_tokens > 0
              ? ` · tokens ${tagProgress.prompt_tokens.toLocaleString()}/${tagProgress.completion_tokens.toLocaleString()}`
              : ""}
          </span>
        </div>
      )}

      {embedRunning && embedProgress && (
        <div className="classifier-row classifier-row-sub">
          <span className="classifier-stat-dim">
            Embedding — {embedProgress.embedded}/{embedProgress.candidates}
            {embedProgress.skipped_empty > 0 ? `, ${embedProgress.skipped_empty} skipped` : ""}
          </span>
        </div>
      )}

      {(opErr || statusErr) && (
        <div className="classifier-row classifier-row-sub classifier-err">
          {opErr || statusErr}
        </div>
      )}
    </div>
  );
}

function parseFinitePositive(s: string): number | undefined {
  if (!s.trim()) return undefined;
  const n = Number(s);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : undefined;
}
