import { useCallback, useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  hubCatalogRefresh,
  hubStatus,
  hubSyncActive,
  startHubSync,
  stopHubSync,
  type HubStatus,
  type HubSyncLog,
  type HubSyncOptions,
  type HubSyncProgress,
  type HubSyncSummary,
} from "../lib/api";

const LOG_RING_SIZE = 300;
const RATE_WINDOW_EVENTS = 30;

/// "Fetched" mode dashboard: monitor the hub-side catalog + sync state and
/// kick off the long-running sync operations. Subscribes to the
/// `hub-sync-progress` Tauri event for live counters during a running sync;
/// otherwise shows the static `hubStatus()` snapshot.
export function HubSyncView() {
  const [status, setStatus] = useState<HubStatus | null>(null);
  const [statusErr, setStatusErr] = useState<string | null>(null);

  // Live progress when a sync is running. `null` when idle.
  const [progress, setProgress] = useState<HubSyncProgress | null>(null);
  const [running, setRunning] = useState(false);
  const [lastSummary, setLastSummary] = useState<HubSyncSummary | null>(null);
  const [opErr, setOpErr] = useState<string | null>(null);

  // Sync options the user can tweak before kicking off a run.
  const [creatorFilter, setCreatorFilter] = useState("");
  const [onlyMissing, setOnlyMissing] = useState(true);
  const [pullPreview, setPullPreview] = useState(false);
  const [rateLimitMs, setRateLimitMs] = useState(700);
  const [workers, setWorkers] = useState(3);
  const [catalogBusy, setCatalogBusy] = useState(false);

  // Log ring buffer. Keeps the last LOG_RING_SIZE entries from
  // `hub-sync-log` events for the user to scroll/copy and share.
  const [logs, setLogs] = useState<HubSyncLog[]>([]);

  // Rate / ETA derived from progress event arrival times. Each entry is
  // `[done, monotonic_ms]`. Sampled at every progress event; we drop the
  // head when the buffer exceeds RATE_WINDOW_EVENTS so the rate is over
  // a recent rolling window rather than the full sync.
  const rateSamplesRef = useRef<Array<[number, number]>>([]);
  const [rateInfo, setRateInfo] = useState<{
    pkgsPerMin: number;
    etaSec: number | null;
  } | null>(null);

  const loadStatus = useCallback(async () => {
    try {
      const s = await hubStatus();
      setStatus(s);
      setStatusErr(null);
    } catch (e) {
      setStatusErr(String(e));
    }
  }, []);

  useEffect(() => {
    loadStatus();
  }, [loadStatus]);

  // HMR recovery: check if a backend sync is still running across page
  // reloads. If yes, flip `running` so the UI reflects the in-flight work
  // even though the JS-side handlers from the initiating click are gone.
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const active = await hubSyncActive();
        if (!cancelled && active) setRunning(true);
      } catch {
        /* ignore */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Refresh status periodically while sync is running so the post-event
  // DB state is reflected (matched count, by-category breakdown). The
  // hub-sync-progress event covers the live counters; this picks up the
  // DB-level aggregates that aren't in the event payload.
  useEffect(() => {
    if (!running) return;
    const handle = window.setInterval(loadStatus, 5000);
    return () => window.clearInterval(handle);
  }, [running, loadStatus]);

  // Subscribe to hub-sync-progress for live counters + rate sampling.
  // The `cancelled` flag handles React 18 StrictMode dev double-mount: if
  // the effect cleanup fires before the async listen() resolves, we'd
  // otherwise end up subscribed twice and see every event duplicated.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    (async () => {
      const u = await listen<HubSyncProgress>("hub-sync-progress", (event) => {
        const p = event.payload;
        setProgress(p);

        // Append to rate-sampling ring buffer.
        const now = performance.now();
        rateSamplesRef.current.push([p.done, now]);
        while (rateSamplesRef.current.length > RATE_WINDOW_EVENTS) {
          rateSamplesRef.current.shift();
        }
        if (rateSamplesRef.current.length >= 2) {
          const [doneOldest, tOldest] = rateSamplesRef.current[0];
          const [doneNewest, tNewest] = rateSamplesRef.current[rateSamplesRef.current.length - 1];
          const deltaDone = doneNewest - doneOldest;
          const deltaMs = tNewest - tOldest;
          if (deltaMs > 0 && deltaDone > 0) {
            const pkgsPerMin = (deltaDone / deltaMs) * 60_000;
            const remaining = Math.max(0, p.total - p.done);
            const etaSec = pkgsPerMin > 0 ? (remaining / pkgsPerMin) * 60 : null;
            setRateInfo({ pkgsPerMin, etaSec });
          }
        }
      });
      if (cancelled) u();
      else unlisten = u;
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  // Subscribe to hub-sync-log for the scrollable log panel.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    let cancelled = false;
    (async () => {
      const u = await listen<HubSyncLog>("hub-sync-log", (event) => {
        setLogs((prev) => {
          const next = [...prev, event.payload];
          return next.length > LOG_RING_SIZE
            ? next.slice(next.length - LOG_RING_SIZE)
            : next;
        });
      });
      if (cancelled) u();
      else unlisten = u;
    })();
    return () => {
      cancelled = true;
      if (unlisten) unlisten();
    };
  }, []);

  const copyLog = useCallback(async () => {
    const text = logs
      .map((l) => `[${new Date(l.timestamp * 1000).toLocaleTimeString()}] ${l.level.toUpperCase()}: ${l.message}`)
      .join("\n");
    try {
      await navigator.clipboard.writeText(text);
    } catch (e) {
      console.error("copy log failed", e);
    }
  }, [logs]);

  const clearLog = useCallback(() => {
    setLogs([]);
  }, []);

  const onRefreshCatalog = useCallback(async () => {
    setCatalogBusy(true);
    setOpErr(null);
    try {
      const r = await hubCatalogRefresh();
      console.log("catalog refresh:", r);
      await loadStatus();
    } catch (e) {
      setOpErr(`catalog refresh: ${e}`);
    } finally {
      setCatalogBusy(false);
    }
  }, [loadStatus]);

  const onStartSync = useCallback(async () => {
    setRunning(true);
    setOpErr(null);
    setLastSummary(null);
    setProgress(null);
    const options: HubSyncOptions = {
      only_missing: onlyMissing,
      pull_preview_for_no_thumb: pullPreview,
      rate_limit_ms: rateLimitMs,
      workers,
    };
    const c = creatorFilter.trim();
    if (c) options.creator = c;
    try {
      const s = await startHubSync(options);
      setLastSummary(s);
      await loadStatus();
    } catch (e) {
      setOpErr(`sync: ${e}`);
    } finally {
      setRunning(false);
    }
  }, [creatorFilter, onlyMissing, pullPreview, rateLimitMs, workers, loadStatus]);

  const onStopSync = useCallback(async () => {
    try {
      await stopHubSync();
    } catch (e) {
      setOpErr(`stop: ${e}`);
    }
  }, []);

  // Persist the collapsed state across HMR / app restarts. Default to
  // open on first launch (so the controls are discoverable), then the
  // user's last preference sticks.
  const collapsed = (typeof window !== "undefined" &&
    localStorage.getItem("hubSyncCollapsed") === "1");
  const [isCollapsed, setIsCollapsed] = useState(collapsed);
  useEffect(() => {
    localStorage.setItem("hubSyncCollapsed", isCollapsed ? "1" : "0");
  }, [isCollapsed]);

  const matchedPct = status && status.total_packages > 0
    ? ((status.matched / status.total_packages) * 100).toFixed(1)
    : "0";

  return (
    <div className={`hub-sync-view ${isCollapsed ? "is-collapsed" : ""}`}>
      <button
        type="button"
        className="hub-collapse-handle"
        onClick={() => setIsCollapsed((v) => !v)}
        title={isCollapsed ? "Expand sync controls" : "Collapse sync controls"}
      >
        <span className="hub-collapse-caret">{isCollapsed ? "▸" : "▾"}</span>
        <span>Hub sync</span>
        {status && (
          <span className="hub-collapse-summary">
            {status.matched.toLocaleString()} / {status.total_packages.toLocaleString()} matched
            ({matchedPct}%)
          </span>
        )}
        {running && (
          <span className="hub-collapse-running">● running</span>
        )}
      </button>

      {isCollapsed ? null : (<>
      <div className="hub-section">
        <h3>Catalog</h3>
        {statusErr && <div className="detail-error-inline">status: {statusErr}</div>}
        {status && (
          <div className="hub-stat-row">
            <Stat label="Sitemap rows" value={status.catalog_rows.toLocaleString()} />
            <Stat
              label="Last fetched"
              value={formatRelTime(status.catalog_latest_fetched_at)}
              title={absDate(status.catalog_latest_fetched_at)}
            />
            <Stat
              label="Newest resource"
              value={formatRelTime(status.catalog_latest_lastmod)}
              title={absDate(status.catalog_latest_lastmod)}
            />
            <button
              type="button"
              onClick={onRefreshCatalog}
              disabled={catalogBusy || running}
              className="hub-action"
            >
              {catalogBusy ? "Refreshing…" : "Refresh catalog"}
            </button>
          </div>
        )}
      </div>

      <div className="hub-section">
        <h3>Coverage</h3>
        {status && (
          <>
            <div className="hub-stat-row">
              <Stat
                label="Matched"
                value={`${status.matched.toLocaleString()} / ${status.total_packages.toLocaleString()}`}
                hint={`${pct(status.matched, status.total_packages)}%`}
              />
              <Stat label="By filename" value={status.matched_by_filename.toLocaleString()} />
              <Stat label="By fuzzy" value={status.matched_by_fuzzy_title.toLocaleString()} />
              <Stat label="Not found" value={status.not_found.toLocaleString()} />
              <Stat label="Never synced" value={status.never_synced.toLocaleString()} />
              <Stat label="Failed" value={status.failed.toLocaleString()} />
            </div>

            {status.by_billing_tier.length > 0 && (
              <div className="hub-stat-row">
                <span className="hub-stat-cluster-label">Billing:</span>
                {status.by_billing_tier.map(([tier, n]) => (
                  <span key={tier} className="hub-chip">
                    {tier} <span className="hub-chip-n">{n.toLocaleString()}</span>
                  </span>
                ))}
              </div>
            )}

            {status.top_categories.length > 0 && (
              <div className="hub-categories">
                <div className="hub-categories-label">Top hub categories (matched):</div>
                <div className="hub-stat-row hub-categories-row">
                  {status.top_categories.slice(0, 15).map(([cat, n]) => (
                    <span key={cat} className="hub-chip">
                      {cat} <span className="hub-chip-n">{n.toLocaleString()}</span>
                    </span>
                  ))}
                </div>
              </div>
            )}
          </>
        )}
      </div>

      <div className="hub-section">
        <h3>Run sync</h3>
        <div className="hub-stat-row hub-controls">
          <label className="hub-field">
            <span>Creator filter</span>
            <input
              type="text"
              value={creatorFilter}
              onChange={(e) => setCreatorFilter(e.target.value)}
              placeholder="(blank = all)"
              disabled={running}
            />
          </label>
          <label className="hub-field">
            <span>Workers</span>
            <input
              type="number"
              min={1}
              max={8}
              value={workers}
              onChange={(e) => setWorkers(Math.max(1, Math.min(8, Number(e.target.value) || 1)))}
              disabled={running}
              style={{ width: 60 }}
            />
          </label>
          <label className="hub-field">
            <span>Rate limit (ms)</span>
            <input
              type="number"
              min={100}
              step={50}
              value={rateLimitMs}
              onChange={(e) => setRateLimitMs(Math.max(100, Number(e.target.value) || 700))}
              disabled={running}
              style={{ width: 90 }}
            />
          </label>
          <label className="toolbar-toggle">
            <input
              type="checkbox"
              checked={onlyMissing}
              onChange={(e) => setOnlyMissing(e.target.checked)}
              disabled={running}
            />
            <span>only_missing</span>
          </label>
          <label className="toolbar-toggle">
            <input
              type="checkbox"
              checked={pullPreview}
              onChange={(e) => setPullPreview(e.target.checked)}
              disabled={running}
            />
            <span>pull preview thumbnails</span>
          </label>
          {!running ? (
            <button type="button" onClick={onStartSync} className="hub-action hub-action-primary">
              Start sync
            </button>
          ) : (
            <button type="button" onClick={onStopSync} className="hub-action hub-action-stop">
              Stop sync
            </button>
          )}
        </div>

        {progress && (
          <ProgressBlock progress={progress} running={running} rateInfo={rateInfo} />
        )}

        {lastSummary && !running && (
          <div className="hub-summary">
            Last run: <strong>{lastSummary.matched}</strong> / {lastSummary.considered} matched,{" "}
            {lastSummary.not_found} not found, {lastSummary.failed} failed
            {lastSummary.previews_pulled > 0 && `, ${lastSummary.previews_pulled} previews pulled`}
            {lastSummary.gated && <span className="hub-gated">  — hit hub gate</span>}
            <span className="hub-elapsed"> · {(lastSummary.elapsed_ms / 1000).toFixed(1)}s</span>
          </div>
        )}

        {opErr && <div className="detail-error-inline">{opErr}</div>}
      </div>

      <LogPanel logs={logs} onCopy={copyLog} onClear={clearLog} />
      </>)}
    </div>
  );
}

function LogPanel({
  logs,
  onCopy,
  onClear,
}: {
  logs: HubSyncLog[];
  onCopy: () => void;
  onClear: () => void;
}) {
  // Auto-scroll to bottom whenever new entries arrive — but ONLY if the
  // user is already near the bottom. Otherwise we'd hijack their scroll
  // while they're reading older entries.
  const ref = useRef<HTMLDivElement>(null);
  const lastLen = useRef(logs.length);
  useEffect(() => {
    if (!ref.current) return;
    const grew = logs.length > lastLen.current;
    lastLen.current = logs.length;
    if (!grew) return;
    const el = ref.current;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 60;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [logs]);

  return (
    <div className="hub-section">
      <div className="hub-log-header">
        <h3 style={{ margin: 0 }}>Log <span className="hub-stat-hint">({logs.length})</span></h3>
        <div style={{ display: "flex", gap: 8 }}>
          <button
            type="button"
            className="hub-action"
            onClick={onCopy}
            disabled={logs.length === 0}
          >
            Copy
          </button>
          <button
            type="button"
            className="hub-action"
            onClick={onClear}
            disabled={logs.length === 0}
          >
            Clear
          </button>
        </div>
      </div>
      <div className="hub-log-box" ref={ref}>
        {logs.length === 0 && (
          <div className="hub-log-empty">No log entries yet. Start a sync to see events here.</div>
        )}
        {logs.map((l, i) => (
          <div key={i} className={`hub-log-line hub-log-${l.level}`}>
            <span className="hub-log-time">{formatLogTime(l.timestamp)}</span>
            <span className="hub-log-level">{l.level}</span>
            <span className="hub-log-msg">{l.message}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

function formatLogTime(unix: number): string {
  const d = new Date(unix * 1000);
  return d.toLocaleTimeString([], { hour12: false });
}

function ProgressBlock({
  progress,
  running,
  rateInfo,
}: {
  progress: HubSyncProgress;
  running: boolean;
  rateInfo: { pkgsPerMin: number; etaSec: number | null } | null;
}) {
  const pctDone = progress.total > 0
    ? Math.min(100, (progress.done / progress.total) * 100)
    : 0;
  return (
    <div className="hub-progress">
      <div className="hub-progress-row">
        <span className="hub-progress-phase">
          {running ? `phase: ${progress.current_status}` : "(completed)"}
        </span>
        <span className="hub-progress-count">
          {progress.done} / {progress.total}
        </span>
        {running && rateInfo && (
          <>
            <span className="hub-progress-rate">
              {rateInfo.pkgsPerMin.toFixed(1)} pkg/min
            </span>
            <span className="hub-progress-eta">
              ETA: {rateInfo.etaSec !== null ? formatDuration(rateInfo.etaSec) : "—"}
            </span>
          </>
        )}
        <span className="hub-progress-pct">{pctDone.toFixed(1)}%</span>
      </div>
      <div className="hub-progress-bar">
        <div className="hub-progress-fill" style={{ width: `${pctDone}%` }} />
      </div>
      <div className="hub-progress-row hub-progress-tally">
        <span>✓ matched: <strong>{progress.matched}</strong></span>
        <span>· not found: {progress.not_found}</span>
        <span>· failed: {progress.failed}</span>
        {progress.previews_pulled > 0 && <span>· previews: {progress.previews_pulled}</span>}
      </div>
      <div className="hub-progress-current" title={progress.current}>
        {progress.current}
      </div>
    </div>
  );
}

function formatDuration(seconds: number): string {
  if (!isFinite(seconds) || seconds < 0) return "—";
  if (seconds < 90) return `${Math.round(seconds)}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 90) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rem = minutes % 60;
  return rem > 0 ? `${hours}h ${rem}m` : `${hours}h`;
}

function Stat({
  label,
  value,
  hint,
  title,
}: {
  label: string;
  value: string;
  hint?: string;
  title?: string;
}) {
  return (
    <div className="hub-stat" title={title}>
      <div className="hub-stat-label">{label}</div>
      <div className="hub-stat-value">
        {value}
        {hint && <span className="hub-stat-hint"> {hint}</span>}
      </div>
    </div>
  );
}

function formatRelTime(unix: number | null): string {
  if (unix === null) return "—";
  const seconds = Math.floor(Date.now() / 1000) - unix;
  if (seconds < 60) return "just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}

function absDate(unix: number | null): string | undefined {
  if (unix === null) return undefined;
  return new Date(unix * 1000).toLocaleString();
}

function pct(num: number, den: number): string {
  if (den === 0) return "0";
  return ((num / den) * 100).toFixed(1);
}
