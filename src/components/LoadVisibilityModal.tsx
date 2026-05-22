import { useCallback, useEffect, useState } from "react";
import {
  computeLoadPlan,
  loadVisibility,
  unloadAll,
  type LoadPlan,
  type LoadResult,
} from "../lib/api";
import type { ToastMessage } from "./Toast";

interface Props {
  /** Package ids the user selected before clicking "Set visibility…".
   *  Pass `null` to mean "no selection — the modal is being opened for
   *  Unload All". */
  selection: number[] | null;
  /** True when the visibility-presets setup has completed. When false,
   *  the modal shows a "setup required" message instead of the closure
   *  preview. */
  setupComplete: boolean;
  /** App-level callback for the result toast + grid refresh. */
  onActionResult: (msg: ToastMessage) => void;
  /** Dismiss the modal. Caller clears the selection. */
  onClose: () => void;
}

type Phase =
  | { kind: "loading" }
  | { kind: "ready"; plan: LoadPlan }
  | { kind: "committing" }
  | { kind: "error"; message: string };

export function LoadVisibilityModal({
  selection,
  setupComplete,
  onActionResult,
  onClose,
}: Props) {
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });

  // Build the seed spec we'll send to backend. Null selection = empty
  // SeedSpec, which means "compute the plan for unloading everything."
  const seeds = {
    creators: [],
    package_ids: selection ?? [],
  };

  // Load the plan on mount / whenever selection changes.
  useEffect(() => {
    if (!setupComplete) return;
    setPhase({ kind: "loading" });
    (async () => {
      try {
        const plan = await computeLoadPlan(seeds);
        setPhase({ kind: "ready", plan });
      } catch (e) {
        setPhase({ kind: "error", message: `${e}` });
      }
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection?.join(","), setupComplete]);

  const commit = useCallback(async () => {
    setPhase({ kind: "committing" });
    try {
      const result: LoadResult = selection
        ? await loadVisibility(seeds)
        : await unloadAll();
      const msg = formatResult(result);
      onActionResult({ kind: "ok", text: msg });
      onClose();
    } catch (e) {
      onActionResult({ kind: "error", text: `Load failed: ${e}` });
      setPhase({ kind: "error", message: `${e}` });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selection, onActionResult, onClose]);

  if (!setupComplete) {
    return (
      <div style={overlayStyle} role="dialog" aria-modal="true">
        <div style={cardStyle}>
          <h2 style={headingStyle}>Setup required</h2>
          <p>
            Loading packages into the active folder needs the one-time
            library setup to be done first. Open the setup wizard from
            the main toolbar ("Set up library…") and run the migration.
          </p>
          <div style={buttonRowStyle}>
            <button style={primaryButtonStyle} onClick={onClose}>
              Close
            </button>
          </div>
        </div>
      </div>
    );
  }

  const isUnloadAll = selection === null;

  return (
    <div style={overlayStyle} role="dialog" aria-modal="true">
      <div style={cardStyle}>
        <h2 style={headingStyle}>
          {isUnloadAll ? "Unload all packages" : "Load to active folder"}
        </h2>

        {phase.kind === "loading" && (
          <p style={dimText}>Computing closure…</p>
        )}

        {phase.kind === "error" && (
          <>
            <p style={errorText}>{phase.message}</p>
            <div style={buttonRowStyle}>
              <button style={secondaryButtonStyle} onClick={onClose}>
                Close
              </button>
            </div>
          </>
        )}

        {phase.kind === "ready" && (
          <ReadyView
            plan={phase.plan}
            isUnloadAll={isUnloadAll}
            onCancel={onClose}
            onCommit={commit}
          />
        )}

        {phase.kind === "committing" && (
          <p style={dimText}>
            {isUnloadAll ? "Unlinking…" : "Hardlinking into active folder…"}
          </p>
        )}
      </div>
    </div>
  );
}

// --- subviews ---------------------------------------------------------------

interface ReadyProps {
  plan: LoadPlan;
  isUnloadAll: boolean;
  onCancel: () => void;
  onCommit: () => void;
}

function ReadyView({ plan, isUnloadAll, onCancel, onCommit }: ReadyProps) {
  const { preview, currently_loaded, will_add, will_remove, will_keep } = plan;
  const targetTotal = preview.total;
  const remainsRemove = currently_loaded > 0 && will_remove > 0;

  return (
    <>
      {!isUnloadAll && (
        <div style={infoBannerStyle}>
          {preview.from_packages > 0 && (
            <>
              <strong>{preview.from_packages}</strong> selected
              {preview.from_deps > 0 && (
                <>
                  {" + "}
                  <strong>{preview.from_deps}</strong> pulled in by
                  dependencies
                </>
              )}
              {" = "}
              <strong>{targetTotal}</strong> total
            </>
          )}
          {preview.from_packages === 0 && preview.from_deps === 0 && (
            <>No packages selected.</>
          )}
        </div>
      )}

      <table style={tableStyle}>
        <tbody>
          <tr>
            <td style={labelCellStyle}>Currently loaded</td>
            <td style={valueCellStyle}>{currently_loaded.toLocaleString()}</td>
          </tr>
          <tr>
            <td style={labelCellStyle}>Target</td>
            <td style={valueCellStyle}>{targetTotal.toLocaleString()}</td>
          </tr>
          <tr>
            <td style={labelCellStyle}>+ Add</td>
            <td style={{ ...valueCellStyle, color: "#3fb950" }}>
              {will_add.toLocaleString()}
            </td>
          </tr>
          <tr>
            <td style={labelCellStyle}>− Remove</td>
            <td
              style={{
                ...valueCellStyle,
                color: remainsRemove ? "#f85149" : "var(--fg-dim)",
              }}
            >
              {will_remove.toLocaleString()}
            </td>
          </tr>
          <tr>
            <td style={labelCellStyle}>= Keep</td>
            <td style={valueCellStyle}>{will_keep.toLocaleString()}</td>
          </tr>
        </tbody>
      </table>

      {preview.unresolved.length > 0 && (
        <details style={{ margin: "8px 0", color: "var(--fg-dim)" }}>
          <summary style={{ cursor: "pointer" }}>
            {preview.unresolved.length} unresolved dep
            {preview.unresolved.length === 1 ? "" : "s"} — packages your
            scenes reference but you don't own
          </summary>
          <ul style={{ paddingLeft: 18, marginTop: 4 }}>
            {preview.unresolved.slice(0, 20).map((u, i) => (
              <li key={i}>
                <code>{u.raw_dep_key}</code>
              </li>
            ))}
            {preview.unresolved.length > 20 && (
              <li>… and {preview.unresolved.length - 20} more</li>
            )}
          </ul>
        </details>
      )}

      {remainsRemove && (
        <p style={warnNoteStyle}>
          ⚠ Close VaM before committing — the {will_remove.toLocaleString()}{" "}
          package{will_remove === 1 ? "" : "s"} being removed may be open
          in VaM, and Windows will refuse to unlink a file that's held.
        </p>
      )}

      <div style={buttonRowStyle}>
        <button style={secondaryButtonStyle} onClick={onCancel}>
          Cancel
        </button>
        <button
          style={
            will_add + will_remove === 0
              ? disabledButtonStyle
              : primaryButtonStyle
          }
          disabled={will_add + will_remove === 0}
          onClick={onCommit}
          title={
            will_add + will_remove === 0
              ? "Already in target state — nothing to do."
              : ""
          }
        >
          {isUnloadAll ? "Unload all" : "Load"}
        </button>
      </div>
    </>
  );
}

function formatResult(r: LoadResult): string {
  const parts: string[] = [];
  if (r.added > 0) parts.push(`+${r.added} added`);
  if (r.removed > 0) parts.push(`−${r.removed} removed`);
  if (r.kept > 0) parts.push(`${r.kept} kept`);
  let msg =
    parts.length > 0
      ? `Active folder updated: ${parts.join(", ")}.`
      : "Active folder already in target state.";
  if (r.errors.length > 0) {
    msg += ` (${r.errors.length} per-file error${r.errors.length === 1 ? "" : "s"})`;
  }
  return msg;
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
  width: 480,
  maxWidth: "92vw",
  maxHeight: "90vh",
  overflowY: "auto",
  color: "var(--fg)",
};

const headingStyle: React.CSSProperties = {
  marginTop: 0,
  marginBottom: 12,
};

const dimText: React.CSSProperties = {
  color: "var(--fg-dim)",
};

const errorText: React.CSSProperties = {
  color: "#f85149",
};

const infoBannerStyle: React.CSSProperties = {
  background: "rgba(122, 162, 255, 0.08)",
  border: "1px solid rgba(122, 162, 255, 0.3)",
  borderRadius: 4,
  padding: "10px 12px",
  marginBottom: 12,
};

const tableStyle: React.CSSProperties = {
  width: "100%",
  borderCollapse: "collapse",
  marginBottom: 8,
};

const labelCellStyle: React.CSSProperties = {
  padding: "4px 8px",
  color: "var(--fg-dim)",
  fontSize: 13,
};

const valueCellStyle: React.CSSProperties = {
  padding: "4px 8px",
  textAlign: "right",
  fontVariantNumeric: "tabular-nums",
};

const warnNoteStyle: React.CSSProperties = {
  background: "rgba(248, 81, 73, 0.08)",
  border: "1px solid rgba(248, 81, 73, 0.3)",
  borderRadius: 4,
  padding: "8px 12px",
  margin: "8px 0",
  fontSize: 12,
  lineHeight: 1.4,
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
