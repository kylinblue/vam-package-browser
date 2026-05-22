import { useState } from "react";
import {
  setHubCategory,
  setHubPin,
  type PinReport,
  type CategoryReport,
} from "../lib/api";

/** Mirrors HubCategoryChips' canonical list. Duplicated here rather than
 *  imported to keep the action bar self-contained — both lists are tiny. */
const HUB_CATEGORIES: readonly string[] = [
  "Scenes",
  "Looks",
  "Clothing",
  "Hairstyles",
  "Morphs",
  "Poses",
  "Mocap + Animation",
  "Textures",
  "Environments",
  "Lighting + HDRI",
  "Assets + Accessories",
  "Audio",
  "Plugins + Scripts",
  "Toolkits + Templates",
  "Comics + Storytelling",
  "Voxta Content",
  "Demo + Lite",
  "Guides",
  "Other",
];

interface Props {
  /** Currently selected package ids. Bar renders only when non-empty
   *  (App.tsx gates the mount), so this is always |sel| >= 1. */
  selection: number[];
  /** Tell App.tsx to clear the selection (after the user explicitly
   *  clears, or implicitly after a successful bulk action). */
  onClear: () => void;
  /** Tell App.tsx to re-query the grid so updated hub_* fields show. */
  onActionApplied: () => void;
  /** Optional handler for the visibility action. When undefined the
   *  button renders disabled with a tooltip pointing at the future
   *  visibility-preset feature. Wired in by a separate session — see
   *  TODO-visibility-presets.md (planned). */
  onSetVisibility?: (ids: number[]) => void;
}

type Feedback = { kind: "ok" | "error"; text: string } | null;

export function SelectionActionBar({
  selection,
  onClear,
  onActionApplied,
  onSetVisibility,
}: Props) {
  const [mode, setMode] = useState<"closed" | "pin" | "category">("closed");
  const [pinUrl, setPinUrl] = useState("");
  const [category, setCategory] = useState("Scenes");
  const [busy, setBusy] = useState<"pin" | "category" | null>(null);
  const [feedback, setFeedback] = useState<Feedback>(null);

  function reset() {
    setMode("closed");
    setPinUrl("");
    setBusy(null);
  }

  async function handlePin() {
    if (!pinUrl.trim() || busy) return;
    setBusy("pin");
    setFeedback(null);
    try {
      const report: PinReport = await setHubPin(selection, pinUrl);
      const okCount = report.results.filter((r) => r.status === "ok").length;
      const failCount = report.results.length - okCount;
      if (okCount === 0) {
        const first = report.results[0];
        setFeedback({
          kind: "error",
          text: `Pin failed for all ${report.results.length} package${
            report.results.length === 1 ? "" : "s"
          }: ${first?.status ?? "unknown"}${first?.detail ? ` — ${first.detail}` : ""}`,
        });
        return;
      }
      const propagated = report.siblings_updated + report.authors_updated;
      let msg = `Linked ${okCount} package${okCount === 1 ? "" : "s"}.`;
      if (failCount > 0) msg += ` (${failCount} failed.)`;
      if (propagated > 0) {
        msg += ` The match will be applied to ${propagated} related row${
          propagated === 1 ? "" : "s"
        } over the next few minutes — each one is verified against the hub at the configured sync rate.`;
      } else {
        msg += " Metadata fills in on the next hub sync.";
      }
      setFeedback({ kind: "ok", text: msg });
      reset();
      onActionApplied();
      // Don't auto-clear selection — the user might want to apply another
      // action to the same set. They can click Clear when done.
    } catch (e) {
      setFeedback({ kind: "error", text: `Pin error: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function handleCategory() {
    if (!category || busy) return;
    setBusy("category");
    setFeedback(null);
    try {
      const report: CategoryReport = await setHubCategory(selection, category);
      const direct = report.directly_updated;
      const sib = report.siblings_updated;
      const msg =
        sib > 0
          ? `Updated category for ${direct} package${
              direct === 1 ? "" : "s"
            } and ${sib} sibling version${sib === 1 ? "" : "s"}. Auto-sync will keep this override.`
          : `Updated category for ${direct} package${
              direct === 1 ? "" : "s"
            }. Auto-sync will keep this override.`;
      setFeedback({ kind: "ok", text: msg });
      reset();
      onActionApplied();
    } catch (e) {
      setFeedback({ kind: "error", text: `Category error: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  const n = selection.length;

  return (
    <div className="selection-bar">
      <div className="selection-bar-row">
        <span className="selection-bar-count">
          {n.toLocaleString()} selected
        </span>
        <button
          type="button"
          className={`selection-bar-action ${mode === "pin" ? "active" : ""}`}
          onClick={() => {
            setMode(mode === "pin" ? "closed" : "pin");
            setFeedback(null);
          }}
          disabled={busy !== null}
        >
          Pin to hub URL…
        </button>
        <button
          type="button"
          className={`selection-bar-action ${mode === "category" ? "active" : ""}`}
          onClick={() => {
            setMode(mode === "category" ? "closed" : "category");
            setFeedback(null);
          }}
          disabled={busy !== null}
        >
          Override category…
        </button>
        <button
          type="button"
          className="selection-bar-action"
          onClick={() => onSetVisibility?.(selection)}
          disabled={!onSetVisibility}
          title={
            onSetVisibility
              ? "Set visibility for selected packages"
              : "Visibility presets — see TODO-visibility-presets.md (wired by a separate session)"
          }
        >
          Set visibility…
        </button>
        <button
          type="button"
          className="selection-bar-action selection-bar-clear"
          onClick={() => {
            reset();
            setFeedback(null);
            onClear();
          }}
          disabled={busy !== null}
          title="Clear selection (Esc)"
        >
          Clear
        </button>
      </div>

      {mode === "pin" && (
        <div className="selection-bar-form">
          <input
            type="text"
            value={pinUrl}
            onChange={(e) => setPinUrl(e.target.value)}
            placeholder={`URL or numeric ID — applies to all ${n} selected`}
            disabled={busy === "pin"}
            onKeyDown={(e) => {
              if (e.key === "Enter") handlePin();
              if (e.key === "Escape") reset();
            }}
            autoFocus
          />
          <button
            type="button"
            className="selection-bar-action selection-bar-primary"
            onClick={handlePin}
            disabled={!pinUrl.trim() || busy === "pin"}
          >
            {busy === "pin" ? "Pinning…" : `Pin ${n}`}
          </button>
          <button
            type="button"
            className="selection-bar-action"
            onClick={reset}
            disabled={busy === "pin"}
          >
            Cancel
          </button>
        </div>
      )}

      {mode === "category" && (
        <div className="selection-bar-form">
          <select
            value={category}
            onChange={(e) => setCategory(e.target.value)}
            disabled={busy === "category"}
          >
            {HUB_CATEGORIES.map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="selection-bar-action selection-bar-primary"
            onClick={handleCategory}
            disabled={busy === "category"}
          >
            {busy === "category" ? "Applying…" : `Apply to ${n}`}
          </button>
          <button
            type="button"
            className="selection-bar-action"
            onClick={reset}
            disabled={busy === "category"}
          >
            Cancel
          </button>
        </div>
      )}

      {feedback && (
        <div className={`selection-bar-feedback selection-bar-feedback-${feedback.kind}`}>
          {feedback.text}
        </div>
      )}
    </div>
  );
}
