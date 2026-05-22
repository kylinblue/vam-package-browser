import { useEffect, useState } from "react";
import {
  setHubAuthor,
  setHubCategory,
  setHubPin,
  setPackageType,
  type AuthorReport,
  type CategoryReport,
  type PackageType,
  type PackageTypeReport,
  type PinReport,
} from "../lib/api";

/** Local heuristic-type values — mirrors PACKAGE_TYPE_VALUES in
 *  commands.rs and PACKAGE_TYPES in DetailView. Used by the override
 *  dropdown. */
const PACKAGE_TYPES: readonly PackageType[] = [
  "Scene",
  "Look",
  "Morph",
  "Texture",
  "Clothing",
  "Hair",
  "Plugin",
  "Asset",
  "Pose",
  "Sound",
  "SubScene",
  "Mixed",
  "Unknown",
];

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
  /** Drives the classify action — hub_category in Fetched mode, the
   *  local heuristic package_type in Simple/Tagged. Same UI slot, mode
   *  picks the backend command and the option list. */
  viewMode: "simple" | "tagged" | "fetched";
  /** Tell App.tsx to clear the selection (after the user explicitly
   *  clears, or implicitly after a successful bulk action). */
  onClear: () => void;
  /** App-level action result sink. Bar forwards every successful or
   *  failed write here; App shows the toast and (on success) refreshes
   *  the grid + aggregates. */
  onActionResult: (msg: { kind: "ok" | "error"; text: string }) => void;
  /** Optional handler for the visibility action. When undefined the
   *  button renders disabled with a tooltip pointing at the future
   *  visibility-preset feature. Wired in by a separate session — see
   *  TODO-visibility-presets.md (planned). */
  onSetVisibility?: (ids: number[]) => void;
}

export function SelectionActionBar({
  selection,
  viewMode,
  onClear,
  onActionResult,
  onSetVisibility,
}: Props) {
  const isFetched = viewMode === "fetched";
  const classifyLabel = isFetched ? "Override category…" : "Override type…";
  const classifyOptions: readonly string[] = isFetched
    ? HUB_CATEGORIES
    : PACKAGE_TYPES;

  const [mode, setMode] = useState<
    "closed" | "pin" | "classify" | "author"
  >("closed");
  const [pinUrl, setPinUrl] = useState("");
  const [classifyDraft, setClassifyDraft] = useState<string>(
    isFetched ? "Scenes" : "Scene",
  );
  const [authorDraft, setAuthorDraft] = useState("");
  const [busy, setBusy] = useState<"pin" | "classify" | "author" | null>(null);

  // Re-seed the draft + close the form on mode flip so the dropdown
  // doesn't carry a stale value from the previous mode's option list.
  useEffect(() => {
    setClassifyDraft(isFetched ? "Scenes" : "Scene");
    if (mode === "classify") setMode("closed");
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isFetched]);

  function reset() {
    setMode("closed");
    setPinUrl("");
    setAuthorDraft("");
    setBusy(null);
  }

  async function handlePin() {
    if (!pinUrl.trim() || busy) return;
    setBusy("pin");
    try {
      const report: PinReport = await setHubPin(selection, pinUrl);
      const okCount = report.results.filter((r) => r.status === "ok").length;
      const failCount = report.results.length - okCount;
      if (okCount === 0) {
        const first = report.results[0];
        onActionResult({
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
      onActionResult({ kind: "ok", text: msg });
      reset();
      onClear();
    } catch (e) {
      onActionResult({ kind: "error", text: `Pin error: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  async function handleClassify() {
    if (!classifyDraft || busy) return;
    setBusy("classify");
    try {
      let msg: string;
      if (isFetched) {
        const report: CategoryReport = await setHubCategory(
          selection,
          classifyDraft,
        );
        const direct = report.directly_updated;
        const sib = report.siblings_updated;
        msg =
          sib > 0
            ? `Updated category for ${direct} package${
                direct === 1 ? "" : "s"
              } and ${sib} sibling version${sib === 1 ? "" : "s"}. Auto-sync will keep this override.`
            : `Updated category for ${direct} package${
                direct === 1 ? "" : "s"
              }. Auto-sync will keep this override.`;
      } else {
        const report: PackageTypeReport = await setPackageType(
          selection,
          classifyDraft as PackageType,
        );
        const direct = report.directly_updated;
        const sib = report.siblings_updated;
        msg =
          sib > 0
            ? `Set type to ${classifyDraft} for ${direct} package${
                direct === 1 ? "" : "s"
              } and ${sib} sibling version${
                sib === 1 ? "" : "s"
              }. Scanner will preserve this on rescan.`
            : `Set type to ${classifyDraft} for ${direct} package${
                direct === 1 ? "" : "s"
              }. Scanner will preserve this on rescan.`;
      }
      onActionResult({ kind: "ok", text: msg });
      reset();
      onClear();
    } catch (e) {
      onActionResult({
        kind: "error",
        text: `${isFetched ? "Category" : "Type"} override error: ${e}`,
      });
    } finally {
      setBusy(null);
    }
  }

  async function handleAuthor() {
    if (!authorDraft.trim() || busy) return;
    setBusy("author");
    try {
      const report: AuthorReport = await setHubAuthor(selection, authorDraft);
      const direct = report.directly_updated;
      const author = report.authors_updated;
      const msg =
        author > 0
          ? `Updated author for ${direct} package${
              direct === 1 ? "" : "s"
            } and ${author} other row${
              author === 1 ? "" : "s"
            } by the same creator${author === 1 ? "" : "s"}. Auto-sync will keep this override.`
          : `Updated author for ${direct} package${
              direct === 1 ? "" : "s"
            }. Auto-sync will keep this override.`;
      onActionResult({ kind: "ok", text: msg });
      reset();
      onClear();
    } catch (e) {
      onActionResult({ kind: "error", text: `Author error: ${e}` });
    } finally {
      setBusy(null);
    }
  }

  const n = selection.length;
  // Pinning N>1 packages to the same hub URL is semantically wrong — one
  // resource URL maps to one hub resource, and version-siblings already
  // get covered automatically by propagation from the pinned row. So we
  // gate the Pin action to N=1. Users wanting to pin different packages
  // to different URLs can do so one at a time via DetailView.
  const canPin = n === 1;

  // Auto-close the Pin form if the selection grows past 1 while it's open
  // (e.g. the user opened Pin at N=1 then Ctrl-clicked another tile).
  useEffect(() => {
    if (mode === "pin" && !canPin) {
      setMode("closed");
      setPinUrl("");
    }
  }, [mode, canPin]);

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
          }}
          disabled={busy !== null || !canPin}
          title={
            canPin
              ? "Pin this package to a hub resource URL"
              : "Pin URL works on one package at a time — one URL maps to one hub resource. Sibling versions auto-inherit via propagation."
          }
        >
          Pin to hub URL…
        </button>
        <button
          type="button"
          className={`selection-bar-action ${mode === "classify" ? "active" : ""}`}
          onClick={() => {
            setMode(mode === "classify" ? "closed" : "classify");
          }}
          disabled={busy !== null}
          title={
            isFetched
              ? "Override hub_category for selected packages — protected from auto-sync overwrites"
              : "Override the local heuristic package_type — kept across rescans, propagates to sibling versions"
          }
        >
          {classifyLabel}
        </button>
        <button
          type="button"
          className={`selection-bar-action ${mode === "author" ? "active" : ""}`}
          onClick={() => {
            setMode(mode === "author" ? "closed" : "author");
          }}
          disabled={busy !== null}
          title="Override the hub_author for selected packages. Propagates to every other package by the same creator(s) and protects against auto-sync overwrites."
        >
          Override author…
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

      {mode === "classify" && (
        <div className="selection-bar-form">
          <select
            value={classifyDraft}
            onChange={(e) => setClassifyDraft(e.target.value)}
            disabled={busy === "classify"}
          >
            {classifyOptions.map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
          <button
            type="button"
            className="selection-bar-action selection-bar-primary"
            onClick={handleClassify}
            disabled={busy === "classify"}
          >
            {busy === "classify" ? "Applying…" : `Apply to ${n}`}
          </button>
          <button
            type="button"
            className="selection-bar-action"
            onClick={reset}
            disabled={busy === "classify"}
          >
            Cancel
          </button>
        </div>
      )}

      {mode === "author" && (
        <div className="selection-bar-form">
          <input
            type="text"
            value={authorDraft}
            onChange={(e) => setAuthorDraft(e.target.value)}
            placeholder={`Canonical hub author — also propagates to every other package by the affected creator${n > 1 ? "s" : ""}`}
            disabled={busy === "author"}
            onKeyDown={(e) => {
              if (e.key === "Enter") handleAuthor();
              if (e.key === "Escape") reset();
            }}
            autoFocus
          />
          <button
            type="button"
            className="selection-bar-action selection-bar-primary"
            onClick={handleAuthor}
            disabled={!authorDraft.trim() || busy === "author"}
          >
            {busy === "author" ? "Applying…" : `Apply to ${n}`}
          </button>
          <button
            type="button"
            className="selection-bar-action"
            onClick={reset}
            disabled={busy === "author"}
          >
            Cancel
          </button>
        </div>
      )}

    </div>
  );
}
