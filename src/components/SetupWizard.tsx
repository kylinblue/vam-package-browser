import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  beginMigration,
  getSetupState,
  probeManagedPath,
  revertSetup,
  type MigrationProgress,
  type MigrationResult,
  type ProbeResult,
  type RevertResult,
  type SetupState,
} from "../lib/api";

interface Props {
  /** Called after the wizard finishes (success, resume-success, or
   *  cancel). Parent should refresh its settings copy. */
  onClose: () => void;
}

type Phase =
  | { kind: "loading" }
  | { kind: "configuring"; state: SetupState }
  | { kind: "resume_prompt"; state: SetupState }
  | { kind: "complete_prompt"; state: SetupState }
  | { kind: "migrating"; progress: MigrationProgress | null }
  | { kind: "reverting"; progress: MigrationProgress | null; state: SetupState }
  | { kind: "done"; result: MigrationResult }
  | { kind: "reverted"; result: RevertResult }
  | { kind: "error"; message: string };

/** Default suggested managed-folder name. Sits as a sibling of the user's
 *  AddonPackages so it shares the same NTFS volume by default. */
function suggestManagedPath(addonRoot: string): string {
  // Trim trailing slash/backslash if any, then append "_Managed".
  const trimmed = addonRoot.replace(/[\\\/]+$/, "");
  return `${trimmed}_Managed`;
}

export function SetupWizard({ onClose }: Props) {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [managedPath, setManagedPath] = useState<string>("");
  const [probe, setProbe] = useState<ProbeResult | null>(null);
  const [probing, setProbing] = useState(false);
  const [ackVamClosed, setAckVamClosed] = useState(false);
  const [ackOneWay, setAckOneWay] = useState(false);

  // Debounced probe scheduling — re-running the FS probe on every keystroke
  // would flicker the UI and create filesystem noise.
  const probeTimer = useRef<number | null>(null);

  // Initial load.
  useEffect(() => {
    (async () => {
      try {
        const state = await getSetupState();
        if (state.setup_complete) {
          setPhase({ kind: "complete_prompt", state });
          return;
        }
        if (state.migration_in_progress && state.managed_root) {
          setManagedPath(state.managed_root);
          setPhase({ kind: "resume_prompt", state });
          return;
        }
        setManagedPath(suggestManagedPath(state.addon_root ?? ""));
        setPhase({ kind: "configuring", state });
      } catch (e) {
        setPhase({ kind: "error", message: `Failed to load setup state: ${e}` });
      }
    })();
  }, []);

  // Re-probe on managed path change, debounced.
  useEffect(() => {
    if (phase.kind !== "configuring") return;
    if (!managedPath.trim()) {
      setProbe(null);
      return;
    }
    if (probeTimer.current !== null) {
      window.clearTimeout(probeTimer.current);
    }
    setProbing(true);
    probeTimer.current = window.setTimeout(async () => {
      try {
        const r = await probeManagedPath(managedPath);
        setProbe(r);
      } catch (e) {
        setProbe(null);
        // Probe errors that escape are unusual (rust side already returns
        // structured ProbeResult.diagnostic for the user-fixable cases).
        // Show in console; let the user keep editing.
        console.error("probe failed:", e);
      } finally {
        setProbing(false);
      }
    }, 350);
    return () => {
      if (probeTimer.current !== null) {
        window.clearTimeout(probeTimer.current);
        probeTimer.current = null;
      }
    };
  }, [managedPath, phase.kind]);

  // Subscribe to migration.progress events when migrating OR reverting.
  // Backend emits the same event from both code paths so one subscription
  // covers both phases.
  useEffect(() => {
    if (phase.kind !== "migrating" && phase.kind !== "reverting") return;
    let unlisten: UnlistenFn | undefined;
    (async () => {
      unlisten = await listen<MigrationProgress>("migration.progress", (e) => {
        setPhase((prev) => {
          if (prev.kind === "migrating") {
            return { kind: "migrating", progress: e.payload };
          }
          if (prev.kind === "reverting") {
            return { ...prev, progress: e.payload };
          }
          return prev;
        });
      });
    })();
    return () => {
      if (unlisten) unlisten();
    };
  }, [phase.kind]);

  const startMigration = useCallback(async () => {
    setPhase({ kind: "migrating", progress: null });
    try {
      const result = await beginMigration(managedPath);
      setPhase({ kind: "done", result });
    } catch (e) {
      setPhase({ kind: "error", message: `${e}` });
    }
  }, [managedPath]);

  const startRevert = useCallback(async (state: SetupState) => {
    setPhase({ kind: "reverting", progress: null, state });
    try {
      const result = await revertSetup();
      setPhase({ kind: "reverted", result });
    } catch (e) {
      setPhase({ kind: "error", message: `Revert failed: ${e}` });
    }
  }, []);

  const canStart = useMemo(() => {
    return (
      phase.kind === "configuring" &&
      probe?.ok === true &&
      ackVamClosed &&
      ackOneWay
    );
  }, [phase.kind, probe, ackVamClosed, ackOneWay]);

  return (
    <div style={overlayStyle} role="dialog" aria-modal="true">
      <div style={cardStyle}>
        <h2 style={{ marginTop: 0, marginBottom: 8 }}>
          Set up package library management
        </h2>

        {phase.kind === "loading" && <p style={{ color: "var(--fg-dim)" }}>Loading…</p>}

        {phase.kind === "error" && (
          <>
            <p style={errorTextStyle}>{phase.message}</p>
            <div style={buttonRowStyle}>
              <button style={primaryButtonStyle} onClick={onClose}>
                Close
              </button>
            </div>
          </>
        )}

        {phase.kind === "resume_prompt" && (
          <>
            <p>
              A previous migration was interrupted. Some files may have
              been moved to the managed folder; some may remain in
              AddonPackages.
            </p>
            <p style={{ color: "var(--fg-dim)", margin: "8px 0" }}>
              <strong>Managed folder:</strong>{" "}
              <code>{phase.state.managed_root}</code>
            </p>
            <p>
              Choose how to proceed:
            </p>
            <ul style={{ color: "var(--fg-dim)", paddingLeft: 18, fontSize: 12, lineHeight: 1.5 }}>
              <li>
                <strong>Resume migration</strong> — finish moving any
                remaining files. Best if the previous run just got
                interrupted (closed window, crash).
              </li>
              <li>
                <strong>Revert migration</strong> — move every file
                back from the managed folder to AddonPackages, clear
                the setup state. Best if the previous run hit an error
                you want to back out of, or you want to start over
                cleanly.
              </li>
            </ul>
            <div style={buttonRowStyle}>
              <button style={secondaryButtonStyle} onClick={onClose}>
                Cancel
              </button>
              <button
                style={dangerButtonStyle}
                onClick={() => startRevert(phase.state)}
              >
                ◀ Revert migration
              </button>
              <button style={primaryButtonStyle} onClick={startMigration}>
                Resume migration ▶
              </button>
            </div>
          </>
        )}

        {phase.kind === "complete_prompt" && (
          <>
            <p>Setup is already complete.</p>
            <p style={{ color: "var(--fg-dim)", margin: "8px 0" }}>
              <strong>Managed library:</strong>{" "}
              <code>{phase.state.managed_root}</code>
            </p>
            <p style={warnBoxStyle}>
              ⚠ Reverting will move every <code>.var</code> file (and
              directory) from the managed library back into your
              AddonPackages folder, remove any currently-loaded
              hardlinks/junctions, and reset the setup state. VaM will
              see the full library again on its next launch. Subfolder
              structure from the managed library is preserved.
            </p>
            <p style={{ color: "var(--fg-dim)", fontSize: 12 }}>
              Same-volume rename is fast — this finishes in seconds
              even for thousands of files.
            </p>
            <div style={buttonRowStyle}>
              <button style={secondaryButtonStyle} onClick={onClose}>
                Close
              </button>
              <button
                style={dangerButtonStyle}
                onClick={() => startRevert(phase.state)}
              >
                ◀ Revert setup
              </button>
            </div>
          </>
        )}

        {phase.kind === "configuring" && (
          <ConfigureView
            state={phase.state}
            managedPath={managedPath}
            onManagedPathChange={setManagedPath}
            probe={probe}
            probing={probing}
            ackVamClosed={ackVamClosed}
            ackOneWay={ackOneWay}
            onAckVamClosedChange={setAckVamClosed}
            onAckOneWayChange={setAckOneWay}
            canStart={canStart}
            onCancel={onClose}
            onStart={startMigration}
          />
        )}

        {phase.kind === "migrating" && (
          <MigratingView progress={phase.progress} verb="Hardlinking" />
        )}

        {phase.kind === "reverting" && (
          <MigratingView progress={phase.progress} verb="Reverting" />
        )}

        {phase.kind === "done" && (
          <DoneView result={phase.result} onClose={onClose} />
        )}

        {phase.kind === "reverted" && (
          <RevertedView result={phase.result} onClose={onClose} />
        )}
      </div>
    </div>
  );
}

// --- subviews ---------------------------------------------------------------

interface ConfigureProps {
  state: SetupState;
  managedPath: string;
  onManagedPathChange: (s: string) => void;
  probe: ProbeResult | null;
  probing: boolean;
  ackVamClosed: boolean;
  ackOneWay: boolean;
  onAckVamClosedChange: (b: boolean) => void;
  onAckOneWayChange: (b: boolean) => void;
  canStart: boolean;
  onCancel: () => void;
  onStart: () => void;
}

function ConfigureView(p: ConfigureProps) {
  return (
    <>
      <div style={infoBannerStyle}>
        This is a one-time setup. It moves your existing <code>.var</code>{" "}
        files into a managed library folder. VaM will keep reading from the
        same AddonPackages path it always has — no reconfiguration needed.
        The managed folder must be on the <strong>same drive</strong> as
        VaM (hardlinks can't cross drives).
      </div>

      <label style={labelStyle}>
        AddonPackages (read-only here — VaM reads from this)
      </label>
      <input
        type="text"
        value={p.state.addon_root ?? ""}
        readOnly
        style={inputReadOnlyStyle}
      />

      <label style={labelStyle}>Managed library folder</label>
      <input
        type="text"
        value={p.managedPath}
        onChange={(e) => p.onManagedPathChange(e.target.value)}
        placeholder="D:\\Games\\VAM\\AddonPackages_Managed"
        style={inputStyle}
        spellCheck={false}
      />
      <div style={{ minHeight: 24, marginBottom: 8 }}>
        {p.probing && (
          <span style={{ color: "var(--fg-dim)" }}>Checking path…</span>
        )}
        {!p.probing && p.probe && (
          <ProbeSummary probe={p.probe} />
        )}
      </div>

      {p.probe && <ProbeChecksList probe={p.probe} />}

      <hr style={hrStyle} />

      <label style={checkboxRowStyle}>
        <input
          type="checkbox"
          checked={p.ackVamClosed}
          onChange={(e) => p.onAckVamClosedChange(e.target.checked)}
        />
        <span>
          I have <strong>closed VaM</strong>. It must not be running while
          files are moved.
        </span>
      </label>
      <label style={checkboxRowStyle}>
        <input
          type="checkbox"
          checked={p.ackOneWay}
          onChange={(e) => p.onAckOneWayChange(e.target.checked)}
        />
        <span>
          I understand this is a <strong>one-way</strong> migration. Reversal
          is not provided in-app; a manual recipe is documented.
        </span>
      </label>

      <div style={buttonRowStyle}>
        <button style={secondaryButtonStyle} onClick={p.onCancel}>
          Cancel
        </button>
        <button
          style={p.canStart ? primaryButtonStyle : disabledButtonStyle}
          disabled={!p.canStart}
          onClick={p.onStart}
        >
          Start migration ▶
        </button>
      </div>
    </>
  );
}

function ProbeSummary({ probe }: { probe: ProbeResult }) {
  if (probe.ok) {
    return (
      <span style={{ color: "#3fb950" }}>
        ● Ready — all checks passed
      </span>
    );
  }
  return (
    <span style={{ color: "#f85149" }}>
      ● {probe.diagnostic ?? "Cannot migrate to this path."}
    </span>
  );
}

function ProbeChecksList({ probe }: { probe: ProbeResult }) {
  return (
    <ul style={{ listStyle: "none", padding: 0, margin: "4px 0 12px" }}>
      {probe.checks.map((c) => (
        <li
          key={c.name}
          style={{
            display: "flex",
            gap: 8,
            padding: "2px 0",
            color: c.ok ? "var(--fg-dim)" : "#f85149",
          }}
        >
          <span style={{ width: 18, textAlign: "center" }}>
            {c.ok ? "✓" : "✗"}
          </span>
          <span style={{ width: 200, color: "var(--fg-dim)" }}>
            {labelForCheck(c.name)}
          </span>
          <span style={{ flex: 1 }}>{c.detail}</span>
        </li>
      ))}
    </ul>
  );
}

function labelForCheck(name: string): string {
  switch (name) {
    case "addon_root_exists":
      return "AddonPackages exists";
    case "managed_not_under_addon":
      return "Managed outside AddonPackages";
    case "managed_empty":
      return "Managed folder empty";
    case "same_volume":
      return "Same NTFS volume";
    case "ntfs":
      return "NTFS filesystem";
    case "hardlink_probe":
      return "Hardlink probe";
    default:
      return name;
  }
}

function MigratingView({
  progress,
  verb,
}: {
  progress: MigrationProgress | null;
  verb: "Hardlinking" | "Reverting";
}) {
  const moved = progress?.moved ?? 0;
  const total = progress?.total ?? 0;
  const pct = total > 0 ? Math.min(100, Math.round((moved * 100) / total)) : 0;
  const heading =
    verb === "Reverting" ? "Reverting setup…" : "Migrating package library…";
  return (
    <>
      <p>{heading}</p>
      <div style={progressOuterStyle}>
        <div
          style={{
            ...progressInnerStyle,
            width: `${pct}%`,
          }}
        />
      </div>
      <p style={{ color: "var(--fg-dim)", margin: "8px 0" }}>
        {total > 0
          ? `${moved.toLocaleString()} / ${total.toLocaleString()} files`
          : "Preparing…"}
        {progress?.current && (
          <>
            {" — "}
            <code>{progress.current}</code>
          </>
        )}
      </p>
      <p style={{ color: "var(--fg-dim)", fontSize: 12 }}>
        Same-volume renames are O(1) per file; this usually finishes within
        seconds even on large libraries.
      </p>
    </>
  );
}

function RevertedView({
  result,
  onClose,
}: {
  result: RevertResult;
  onClose: () => void;
}) {
  const sec = Math.max(1, Math.round(result.elapsed_ms / 1000));
  return (
    <>
      <p style={{ color: "#3fb950", fontWeight: 600 }}>✓ Revert complete</p>
      <ul style={{ color: "var(--fg-dim)", paddingLeft: 18 }}>
        <li>
          {result.moved.toLocaleString()} package
          {result.moved === 1 ? "" : "s"} moved back to AddonPackages
        </li>
        {result.active_cleared > 0 && (
          <li>
            {result.active_cleared.toLocaleString()} active-folder
            entr{result.active_cleared === 1 ? "y" : "ies"} unlinked
          </li>
        )}
        {result.orphans_pruned > 0 && (
          <li>
            {result.orphans_pruned.toLocaleString()} stale package row
            {result.orphans_pruned === 1 ? "" : "s"} pruned (file no
            longer exists)
          </li>
        )}
        <li>elapsed {sec.toLocaleString()} s</li>
        {result.errors.length > 0 && (
          <li style={{ color: "#f85149" }}>
            {result.errors.length} per-file error
            {result.errors.length === 1 ? "" : "s"} (see details)
          </li>
        )}
      </ul>
      {result.errors.length > 0 && (
        <details style={{ margin: "8px 0", color: "var(--fg-dim)" }}>
          <summary>Error details</summary>
          <ul style={{ paddingLeft: 18, marginTop: 4 }}>
            {result.errors.slice(0, 20).map((err, i) => (
              <li key={i}>
                <code>{err.path}</code> — {err.reason}
              </li>
            ))}
            {result.errors.length > 20 && (
              <li>… and {result.errors.length - 20} more</li>
            )}
          </ul>
        </details>
      )}
      <p>
        Setup state cleared. Run <em>Scan all</em> to re-index, then
        the wizard if you want to set up again.
      </p>
      <div style={buttonRowStyle}>
        <button style={primaryButtonStyle} onClick={onClose}>
          Done
        </button>
      </div>
    </>
  );
}

function DoneView({
  result,
  onClose,
}: {
  result: MigrationResult;
  onClose: () => void;
}) {
  const sec = Math.max(1, Math.round(result.elapsed_ms / 1000));
  return (
    <>
      <p style={{ color: "#3fb950", fontWeight: 600 }}>✓ Migration complete</p>
      <ul style={{ color: "var(--fg-dim)", paddingLeft: 18 }}>
        <li>{result.moved.toLocaleString()} indexed packages moved</li>
        {result.leftover_moved > 0 && (
          <li>
            {result.leftover_moved.toLocaleString()} additional .var files
            (not yet indexed) moved
          </li>
        )}
        <li>elapsed {sec.toLocaleString()} s</li>
        {result.errors.length > 0 && (
          <li style={{ color: "#f85149" }}>
            {result.errors.length} per-file errors (see logs)
          </li>
        )}
      </ul>
      {result.errors.length > 0 && (
        <details style={{ margin: "8px 0", color: "var(--fg-dim)" }}>
          <summary>Error details</summary>
          <ul style={{ paddingLeft: 18, marginTop: 4 }}>
            {result.errors.slice(0, 20).map((err, i) => (
              <li key={i}>
                <code>{err.path}</code> — {err.reason}
              </li>
            ))}
            {result.errors.length > 20 && (
              <li>… and {result.errors.length - 20} more</li>
            )}
          </ul>
        </details>
      )}
      <p>Next: pick what to load. Your library is now managed.</p>
      <div style={buttonRowStyle}>
        <button style={primaryButtonStyle} onClick={onClose}>
          Done
        </button>
      </div>
    </>
  );
}

// --- styles -----------------------------------------------------------------

const overlayStyle: React.CSSProperties = {
  position: "fixed",
  inset: 0,
  background: "rgba(0, 0, 0, 0.65)",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
  zIndex: 1000,
};

const cardStyle: React.CSSProperties = {
  background: "var(--bg-elev)",
  border: "1px solid var(--border)",
  borderRadius: 8,
  padding: 24,
  width: 640,
  maxWidth: "92vw",
  maxHeight: "90vh",
  overflowY: "auto",
  color: "var(--fg)",
};

const infoBannerStyle: React.CSSProperties = {
  background: "rgba(122, 162, 255, 0.08)",
  border: "1px solid rgba(122, 162, 255, 0.3)",
  borderRadius: 4,
  padding: "10px 12px",
  marginBottom: 16,
  color: "var(--fg)",
  lineHeight: 1.5,
};

const labelStyle: React.CSSProperties = {
  display: "block",
  color: "var(--fg-dim)",
  marginTop: 12,
  marginBottom: 4,
  fontSize: 12,
};

const inputStyle: React.CSSProperties = {
  width: "100%",
  background: "var(--bg)",
  color: "var(--fg)",
  border: "1px solid var(--border)",
  borderRadius: 4,
  padding: "6px 8px",
  fontFamily: "monospace",
  fontSize: 13,
};

const inputReadOnlyStyle: React.CSSProperties = {
  ...inputStyle,
  color: "var(--fg-dim)",
  cursor: "default",
};

const hrStyle: React.CSSProperties = {
  border: "none",
  borderTop: "1px solid var(--border)",
  margin: "16px 0",
};

const checkboxRowStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "center",
  gap: 8,
  padding: "4px 0",
  cursor: "pointer",
};

const buttonRowStyle: React.CSSProperties = {
  display: "flex",
  justifyContent: "flex-end",
  gap: 8,
  marginTop: 16,
};

const primaryButtonStyle: React.CSSProperties = {
  background: "var(--accent)",
  color: "#0f1115",
  border: "none",
  padding: "8px 16px",
  borderRadius: 4,
  fontWeight: 600,
  cursor: "pointer",
};

const disabledButtonStyle: React.CSSProperties = {
  ...primaryButtonStyle,
  background: "#444",
  color: "#888",
  cursor: "not-allowed",
};

const secondaryButtonStyle: React.CSSProperties = {
  background: "transparent",
  color: "var(--fg)",
  border: "1px solid var(--border)",
  padding: "8px 16px",
  borderRadius: 4,
  cursor: "pointer",
};

const dangerButtonStyle: React.CSSProperties = {
  background: "rgba(248, 81, 73, 0.15)",
  color: "#f85149",
  border: "1px solid rgba(248, 81, 73, 0.5)",
  padding: "8px 16px",
  borderRadius: 4,
  fontWeight: 600,
  cursor: "pointer",
};

const warnBoxStyle: React.CSSProperties = {
  background: "rgba(248, 81, 73, 0.08)",
  border: "1px solid rgba(248, 81, 73, 0.3)",
  borderRadius: 4,
  padding: "10px 12px",
  margin: "12px 0",
  fontSize: 13,
  lineHeight: 1.5,
};

const errorTextStyle: React.CSSProperties = {
  color: "#f85149",
};

const progressOuterStyle: React.CSSProperties = {
  width: "100%",
  height: 12,
  background: "var(--bg)",
  border: "1px solid var(--border)",
  borderRadius: 4,
  overflow: "hidden",
};

const progressInnerStyle: React.CSSProperties = {
  height: "100%",
  background: "var(--accent)",
  transition: "width 100ms linear",
};
