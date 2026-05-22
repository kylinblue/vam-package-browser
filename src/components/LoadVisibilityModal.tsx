import { useCallback, useEffect, useState } from "react";
import {
  computeLoadPlan,
  createPreset,
  deletePreset,
  getPreset,
  listPresets,
  loadVisibility,
  unloadAll,
  type LoadPlan,
  type LoadResult,
  type PresetSummary,
  type SeedSpec,
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

  // The seeds currently driving the plan. Starts from the caller's
  // `selection` but can be reassigned by clicking a saved preset.
  // `null` selection (= the unload-all entry point) keeps activeSeeds
  // null until the user picks a preset.
  const [activeSeeds, setActiveSeeds] = useState<SeedSpec | null>(
    selection ? { creators: [], package_ids: selection } : null,
  );

  // Saved presets — loaded once on mount, refreshed after create/delete.
  const [presets, setPresets] = useState<PresetSummary[]>([]);
  const [saveFormOpen, setSaveFormOpen] = useState(false);
  const [saveDraftName, setSaveDraftName] = useState("");
  const [busy, setBusy] = useState(false);

  // Effective seeds for the plan + commit. Falls back to the empty
  // SeedSpec (= unload-all) when nothing is selected and no preset is
  // active.
  const seeds: SeedSpec = activeSeeds ?? { creators: [], package_ids: [] };

  // Refresh the saved-preset list. Called once on mount and after any
  // create/delete so the list stays current.
  const refreshPresets = useCallback(async () => {
    if (!setupComplete) return;
    try {
      const rows = await listPresets();
      setPresets(rows);
    } catch (e) {
      console.error("list_presets:", e);
    }
  }, [setupComplete]);

  // Load the plan whenever the effective seeds change.
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
  }, [
    activeSeeds?.creators.join("|"),
    activeSeeds?.package_ids.join(","),
    setupComplete,
  ]);

  useEffect(() => {
    refreshPresets();
  }, [refreshPresets]);

  const commit = useCallback(async () => {
    setPhase({ kind: "committing" });
    try {
      // If the user hasn't picked anything and the caller said "no
      // selection" too, treat it as unload-all. Otherwise load the
      // active seeds (which may be from a clicked preset).
      const result: LoadResult =
        activeSeeds === null
          ? await unloadAll()
          : await loadVisibility(activeSeeds);
      const msg = formatResult(result);
      onActionResult({ kind: "ok", text: msg });
      onClose();
    } catch (e) {
      onActionResult({ kind: "error", text: `Load failed: ${e}` });
      setPhase({ kind: "error", message: `${e}` });
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSeeds, onActionResult, onClose]);

  const loadPresetSeeds = useCallback(
    async (id: number) => {
      setBusy(true);
      try {
        const preset = await getPreset(id);
        setActiveSeeds(preset.seeds);
      } catch (e) {
        onActionResult({ kind: "error", text: `Open preset failed: ${e}` });
      } finally {
        setBusy(false);
      }
    },
    [onActionResult],
  );

  const handleDeletePreset = useCallback(
    async (id: number, name: string) => {
      if (!window.confirm(`Delete preset "${name}"?`)) return;
      setBusy(true);
      try {
        await deletePreset(id);
        await refreshPresets();
        onActionResult({ kind: "ok", text: `Deleted preset "${name}".` });
      } catch (e) {
        onActionResult({ kind: "error", text: `Delete failed: ${e}` });
      } finally {
        setBusy(false);
      }
    },
    [refreshPresets, onActionResult],
  );

  const handleSavePreset = useCallback(async () => {
    const name = saveDraftName.trim();
    if (!name) return;
    setBusy(true);
    try {
      await createPreset(name, seeds);
      setSaveDraftName("");
      setSaveFormOpen(false);
      await refreshPresets();
      onActionResult({ kind: "ok", text: `Saved preset "${name}".` });
    } catch (e) {
      onActionResult({ kind: "error", text: `Save failed: ${e}` });
    } finally {
      setBusy(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [saveDraftName, seeds, refreshPresets, onActionResult]);

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

  const isUnloadAll = activeSeeds === null;
  const seedsAreEmpty =
    seeds.creators.length === 0 && seeds.package_ids.length === 0;

  return (
    <div style={overlayStyle} role="dialog" aria-modal="true">
      <div style={cardStyle}>
        <h2 style={headingStyle}>Visibility — load / unload</h2>

        {presets.length > 0 && (
          <PresetsSection
            presets={presets}
            busy={busy}
            onLoadPreset={loadPresetSeeds}
            onDeletePreset={handleDeletePreset}
          />
        )}

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
          <>
            <ReadyView
              plan={phase.plan}
              isUnloadAll={isUnloadAll}
              onCancel={onClose}
              onCommit={commit}
            />
            {!seedsAreEmpty && (
              <SavePresetSection
                open={saveFormOpen}
                draftName={saveDraftName}
                busy={busy}
                onOpen={() => setSaveFormOpen(true)}
                onCancel={() => {
                  setSaveFormOpen(false);
                  setSaveDraftName("");
                }}
                onDraftChange={setSaveDraftName}
                onSave={handleSavePreset}
              />
            )}
          </>
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

interface PresetsSectionProps {
  presets: PresetSummary[];
  busy: boolean;
  onLoadPreset: (id: number) => void;
  onDeletePreset: (id: number, name: string) => void;
}

function PresetsSection({
  presets,
  busy,
  onLoadPreset,
  onDeletePreset,
}: PresetsSectionProps) {
  return (
    <div style={presetsBlockStyle}>
      <div style={presetsHeaderStyle}>
        Saved presets ({presets.length})
      </div>
      <ul style={presetsListStyle}>
        {presets.map((p) => (
          <li key={p.id} style={presetRowStyle}>
            <button
              type="button"
              style={presetLoadButtonStyle}
              onClick={() => onLoadPreset(p.id)}
              disabled={busy}
              title="Click to load this preset into the plan"
            >
              <span style={{ fontWeight: 600 }}>{p.name}</span>
              <span style={presetCountsStyle}>
                {p.creator_count > 0 && (
                  <>
                    {p.creator_count} author
                    {p.creator_count === 1 ? "" : "s"}
                  </>
                )}
                {p.creator_count > 0 && p.package_count > 0 && " + "}
                {p.package_count > 0 && (
                  <>
                    {p.package_count} pkg{p.package_count === 1 ? "" : "s"}
                  </>
                )}
                {p.creator_count === 0 && p.package_count === 0 && (
                  <em>empty</em>
                )}
              </span>
            </button>
            <button
              type="button"
              style={presetDeleteButtonStyle}
              onClick={() => onDeletePreset(p.id, p.name)}
              disabled={busy}
              aria-label={`Delete preset ${p.name}`}
              title="Delete this preset"
            >
              ×
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

interface SavePresetSectionProps {
  open: boolean;
  draftName: string;
  busy: boolean;
  onOpen: () => void;
  onCancel: () => void;
  onDraftChange: (s: string) => void;
  onSave: () => void;
}

function SavePresetSection({
  open,
  draftName,
  busy,
  onOpen,
  onCancel,
  onDraftChange,
  onSave,
}: SavePresetSectionProps) {
  if (!open) {
    return (
      <button
        type="button"
        style={linkButtonStyle}
        onClick={onOpen}
        disabled={busy}
        title="Save the current seed spec as a named preset for future reuse"
      >
        + Save as preset…
      </button>
    );
  }
  return (
    <div style={saveFormStyle}>
      <input
        type="text"
        value={draftName}
        onChange={(e) => onDraftChange(e.target.value)}
        placeholder="Preset name"
        autoFocus
        disabled={busy}
        onKeyDown={(e) => {
          if (e.key === "Enter") onSave();
          if (e.key === "Escape") onCancel();
        }}
        style={saveInputStyle}
      />
      <button
        type="button"
        style={
          draftName.trim() && !busy
            ? primaryButtonStyle
            : disabledButtonStyle
        }
        onClick={onSave}
        disabled={!draftName.trim() || busy}
      >
        Save
      </button>
      <button
        type="button"
        style={secondaryButtonStyle}
        onClick={onCancel}
        disabled={busy}
      >
        Cancel
      </button>
    </div>
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

const presetsBlockStyle: React.CSSProperties = {
  background: "var(--bg)",
  border: "1px solid var(--border)",
  borderRadius: 4,
  padding: "8px 10px",
  marginBottom: 12,
};

const presetsHeaderStyle: React.CSSProperties = {
  fontSize: 12,
  color: "var(--fg-dim)",
  marginBottom: 6,
};

const presetsListStyle: React.CSSProperties = {
  listStyle: "none",
  padding: 0,
  margin: 0,
  display: "flex",
  flexDirection: "column",
  gap: 4,
};

const presetRowStyle: React.CSSProperties = {
  display: "flex",
  alignItems: "stretch",
  gap: 4,
};

const presetLoadButtonStyle: React.CSSProperties = {
  flex: 1,
  display: "flex",
  justifyContent: "space-between",
  alignItems: "center",
  background: "var(--bg-elev)",
  color: "var(--fg)",
  border: "1px solid var(--border)",
  borderRadius: 4,
  padding: "6px 10px",
  cursor: "pointer",
  textAlign: "left",
};

const presetCountsStyle: React.CSSProperties = {
  fontSize: 12,
  color: "var(--fg-dim)",
};

const presetDeleteButtonStyle: React.CSSProperties = {
  background: "var(--bg-elev)",
  color: "var(--fg-dim)",
  border: "1px solid var(--border)",
  borderRadius: 4,
  padding: "0 10px",
  cursor: "pointer",
  fontSize: 16,
  lineHeight: 1,
};

const linkButtonStyle: React.CSSProperties = {
  background: "transparent",
  color: "var(--accent)",
  border: "none",
  padding: "4px 0",
  cursor: "pointer",
  textAlign: "left",
  fontSize: 13,
};

const saveFormStyle: React.CSSProperties = {
  display: "flex",
  gap: 6,
  marginTop: 6,
};

const saveInputStyle: React.CSSProperties = {
  flex: 1,
  background: "var(--bg)",
  color: "var(--fg)",
  border: "1px solid var(--border)",
  borderRadius: 4,
  padding: "6px 8px",
};
