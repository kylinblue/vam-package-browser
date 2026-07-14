import { useEffect, useMemo, useRef, useState } from "react";
import type { PackageRow, PackageType } from "../lib/api";

interface Props {
  packages: PackageRow[];
  /** True when totalMatched > packages.length (backend limit clip). */
  truncated: boolean;
  totalMatched: number;
  viewMode: "simple" | "advanced";
  selectedType: PackageType | null;
  selectedCreator: string;
  selectedHubCategory: string | null;
  setSelectedType: (t: PackageType | null) => void;
  setSelectedCreator: (c: string) => void;
  setSelectedHubCategory: (c: string | null) => void;
  /** JSON-serialized snapshot of every filter the panel does NOT manage.
   *  Any change here resets the panel's internal navigation history — its
   *  trail of clicked filters only makes sense as long as the surrounding
   *  filter context is unchanged. */
  externalFilterSignature: string;
}

type Snapshot = {
  type: PackageType | null;
  creator: string;
  hubCategory: string | null;
};

function snapshotEqual(a: Snapshot, b: Snapshot): boolean {
  return (
    a.type === b.type &&
    a.creator === b.creator &&
    a.hubCategory === b.hubCategory
  );
}

const AUTHOR_HEAD = 5;
const AUTHOR_STEP = 20;

// ── Hue palette ────────────────────────────────────────────────────────────
// All values are CSS-var-friendly RGB triples (no `rgb()` wrapper, no alpha).
// Consumed via `rgba(var(--row-accent), <alpha>)` in styles.css so the row's
// bar fill, selected-row stripe, and selected-row fill all draw from one
// variable. Alpha policy stays low (0.14–0.32) to keep the panel restrained.

/** Semantic family for package_type / hub_category. */
const FAMILY_HUE = {
  content: "122, 162, 255",   // blue   (scenes, subscenes, environments)
  body: "140, 210, 160",      // green  (looks, morphs, clothing, hair, poses)
  resources: "220, 180, 110", // amber  (textures, audio, lighting, assets)
  code: "180, 140, 230",      // purple (plugins, toolkits, scripts)
  other: "160, 165, 178",     // neutral (mixed, unknown, demos, guides)
} as const;

const PACKAGE_TYPE_HUE: Record<PackageType, string> = {
  Scene: FAMILY_HUE.content,
  SubScene: FAMILY_HUE.content,
  Look: FAMILY_HUE.body,
  Morph: FAMILY_HUE.body,
  Clothing: FAMILY_HUE.body,
  Hair: FAMILY_HUE.body,
  Pose: FAMILY_HUE.body,
  Texture: FAMILY_HUE.resources,
  Asset: FAMILY_HUE.resources,
  Sound: FAMILY_HUE.resources,
  Plugin: FAMILY_HUE.code,
  Mixed: FAMILY_HUE.other,
  Unknown: FAMILY_HUE.other,
};

const HUB_CATEGORY_HUE: Record<string, string> = {
  Scenes: FAMILY_HUE.content,
  Environments: FAMILY_HUE.content,
  "Comics + Storytelling": FAMILY_HUE.content,
  "Voxta Content": FAMILY_HUE.content,
  Looks: FAMILY_HUE.body,
  Clothing: FAMILY_HUE.body,
  Hairstyles: FAMILY_HUE.body,
  Morphs: FAMILY_HUE.body,
  Poses: FAMILY_HUE.body,
  "Mocap + Animation": FAMILY_HUE.body,
  Textures: FAMILY_HUE.resources,
  Audio: FAMILY_HUE.resources,
  "Lighting + HDRI": FAMILY_HUE.resources,
  "Assets + Accessories": FAMILY_HUE.resources,
  "Plugins + Scripts": FAMILY_HUE.code,
  "Toolkits + Templates": FAMILY_HUE.code,
  "Demo + Lite": FAMILY_HUE.other,
  Guides: FAMILY_HUE.other,
  Other: FAMILY_HUE.other,
};

interface SizeBucket {
  label: string;
  lo: number;
  hi: number;
  hue: string;
}

const SIZE_BUCKETS: SizeBucket[] = [
  // Ordinal cool → warm gradient so the histogram reads as "small to large"
  // at a glance, independent of label parsing.
  { label: "< 5 MB", lo: 0, hi: 5 * 1024 * 1024, hue: "122, 180, 255" },        // cool blue
  { label: "5–50 MB", lo: 5 * 1024 * 1024, hi: 50 * 1024 * 1024, hue: "130, 210, 200" }, // teal
  { label: "50–500 MB", lo: 50 * 1024 * 1024, hi: 500 * 1024 * 1024, hue: "220, 180, 110" }, // amber
  { label: "> 500 MB", lo: 500 * 1024 * 1024, hi: Number.POSITIVE_INFINITY, hue: "235, 140, 120" }, // coral
];

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

interface Bucket {
  key: string;
  count: number;
  /** False for placeholder buckets like "(unknown)" / "(unidentified)" and
   *  for size buckets — they can't be applied as a single filter today. */
  clickable: boolean;
  /** CSS-var-friendly RGB triple ("R, G, B"). Set on the row inline so the
   *  bar fill / selection stripe read from the same custom property. If
   *  undefined, the row inherits the section's default hue. */
  hue?: string;
}

export function StatsPanel({
  packages,
  truncated,
  totalMatched,
  viewMode,
  selectedType,
  selectedCreator,
  selectedHubCategory,
  setSelectedType,
  setSelectedCreator,
  setSelectedHubCategory,
  externalFilterSignature,
}: Props) {
  // ── Navigation history ──────────────────────────────────────────────────
  // Trail of (type, creator, hubCategory) snapshots produced by clicks
  // *inside* this panel. Index points at the snapshot currently applied.
  // history.length === 0 means "no panel actions yet, history is empty".
  const [history, setHistory] = useState<Snapshot[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);
  // The last snapshot we applied via the panel. An external filter mutation
  // is detected when the current panel-axis state drifts from this ref.
  const lastAppliedRef = useRef<Snapshot | null>(null);

  // ── Author expander ─────────────────────────────────────────────────────
  const [authorLimit, setAuthorLimit] = useState(AUTHOR_HEAD);

  // Reset history when any non-panel-managed filter (or viewMode) changes.
  // This includes search, tags, size/date range, favorites/hidden toggles —
  // anything in externalFilterSignature.
  useEffect(() => {
    setHistory([]);
    setHistoryIndex(-1);
    lastAppliedRef.current = null;
    setAuthorLimit(AUTHOR_HEAD);
  }, [externalFilterSignature]);

  // Reset history when one of OUR axes (type/creator/hubCategory) is mutated
  // from somewhere other than the panel (TypeChips chip click, AuthorPicker,
  // DetailView's "filter by author" link, etc.).
  useEffect(() => {
    if (lastAppliedRef.current === null) return;
    const current: Snapshot = {
      type: selectedType,
      creator: selectedCreator,
      hubCategory: selectedHubCategory,
    };
    if (snapshotEqual(current, lastAppliedRef.current)) return;
    setHistory([]);
    setHistoryIndex(-1);
    lastAppliedRef.current = null;
  }, [selectedType, selectedCreator, selectedHubCategory]);

  // ── Aggregation ─────────────────────────────────────────────────────────
  const stats = useMemo(() => {
    const useHub = viewMode === "advanced";
    const typeCounts = new Map<string, number>();
    let typeNullCount = 0;
    const authorCounts = new Map<string, number>();
    let authorEmpty = 0;
    const sizeBucketCounts = SIZE_BUCKETS.map(() => 0);
    let totalSize = 0;
    let maxSize = 0;
    for (const p of packages) {
      if (useHub) {
        if (p.hub_category) {
          typeCounts.set(p.hub_category, (typeCounts.get(p.hub_category) ?? 0) + 1);
        } else {
          typeNullCount++;
        }
      } else {
        typeCounts.set(p.package_type, (typeCounts.get(p.package_type) ?? 0) + 1);
      }
      if (p.creator) {
        authorCounts.set(p.creator, (authorCounts.get(p.creator) ?? 0) + 1);
      } else {
        authorEmpty++;
      }
      totalSize += p.file_size;
      if (p.file_size > maxSize) maxSize = p.file_size;
      for (let i = 0; i < SIZE_BUCKETS.length; i++) {
        const b = SIZE_BUCKETS[i];
        if (p.file_size >= b.lo && p.file_size < b.hi) {
          sizeBucketCounts[i]++;
          break;
        }
      }
    }
    const types: Bucket[] = [...typeCounts.entries()]
      .map(([key, count]) => ({
        key,
        count,
        clickable: true,
        hue: useHub
          ? HUB_CATEGORY_HUE[key] ?? FAMILY_HUE.other
          : PACKAGE_TYPE_HUE[key as PackageType] ?? FAMILY_HUE.other,
      }))
      .sort((a, b) => b.count - a.count);
    if (useHub && typeNullCount > 0) {
      // "(unidentified)" — hub_category IS NULL is a real filter axis but
      // App.tsx doesn't wire its setter today, so this stays display-only.
      types.push({
        key: "(unidentified)",
        count: typeNullCount,
        clickable: false,
        hue: FAMILY_HUE.other,
      });
    }
    const authors: Bucket[] = [...authorCounts.entries()]
      .map(([key, count]) => ({ key, count, clickable: true }))
      .sort((a, b) => b.count - a.count || a.key.localeCompare(b.key));
    if (authorEmpty > 0) {
      authors.push({ key: "(unknown)", count: authorEmpty, clickable: false });
    }
    const sizes: Bucket[] = SIZE_BUCKETS.map((b, i) => ({
      key: b.label,
      count: sizeBucketCounts[i],
      clickable: false,
      hue: b.hue,
    }));
    return {
      n: packages.length,
      totalSize,
      avgSize: packages.length > 0 ? totalSize / packages.length : 0,
      maxSize,
      types,
      authors,
      sizes,
    };
  }, [packages, viewMode]);

  // ── History operations ──────────────────────────────────────────────────
  function applySnapshot(s: Snapshot) {
    if (s.type !== selectedType) setSelectedType(s.type);
    if (s.creator !== selectedCreator) setSelectedCreator(s.creator);
    if (s.hubCategory !== selectedHubCategory) setSelectedHubCategory(s.hubCategory);
  }

  function applyAndPush(next: Snapshot) {
    const current: Snapshot = {
      type: selectedType,
      creator: selectedCreator,
      hubCategory: selectedHubCategory,
    };
    if (snapshotEqual(next, current)) return;
    let newHist: Snapshot[];
    let newIdx: number;
    if (history.length === 0) {
      // Seed with pre-panel state so Back returns the user to where they were.
      newHist = [current, next];
      newIdx = 1;
    } else {
      // Truncate any forward branch before pushing.
      newHist = [...history.slice(0, historyIndex + 1), next];
      newIdx = newHist.length - 1;
    }
    setHistory(newHist);
    setHistoryIndex(newIdx);
    lastAppliedRef.current = next;
    applySnapshot(next);
  }

  function goBack() {
    if (historyIndex <= 0) return;
    const i = historyIndex - 1;
    setHistoryIndex(i);
    lastAppliedRef.current = history[i];
    applySnapshot(history[i]);
  }
  function goForward() {
    if (historyIndex < 0 || historyIndex >= history.length - 1) return;
    const i = historyIndex + 1;
    setHistoryIndex(i);
    lastAppliedRef.current = history[i];
    applySnapshot(history[i]);
  }
  function clearPanelFilter() {
    applyAndPush({ type: null, creator: "", hubCategory: null });
  }

  // ── Click handlers ──────────────────────────────────────────────────────
  function handleTypeClick(key: string) {
    if (viewMode === "advanced") {
      const next = selectedHubCategory === key ? null : key;
      applyAndPush({
        type: selectedType,
        creator: selectedCreator,
        hubCategory: next,
      });
    } else {
      const pt = key as PackageType;
      const next = selectedType === pt ? null : pt;
      applyAndPush({
        type: next,
        creator: selectedCreator,
        hubCategory: selectedHubCategory,
      });
    }
  }
  function handleAuthorClick(key: string) {
    const next = selectedCreator === key ? "" : key;
    applyAndPush({
      type: selectedType,
      creator: next,
      hubCategory: selectedHubCategory,
    });
  }

  const canBack = historyIndex > 0;
  const canForward = historyIndex >= 0 && historyIndex < history.length - 1;
  const canClear =
    selectedType !== null || selectedCreator !== "" || selectedHubCategory !== null;

  const typeSelectedKey =
    viewMode === "advanced" ? selectedHubCategory : selectedType;
  const authorsToShow = stats.authors.slice(0, authorLimit);
  const remainingAuthors = Math.max(0, stats.authors.length - authorLimit);
  const showStep20 = remainingAuthors > 0;
  const showShowAll = remainingAuthors > AUTHOR_STEP;
  const showReset = authorLimit > AUTHOR_HEAD;

  return (
    <aside className="stats-panel">
      <div className="stats-nav">
        <button
          type="button"
          className="stats-nav-btn"
          disabled={!canBack}
          onClick={goBack}
          title="Back through panel filter history"
          aria-label="Back"
        >
          ←
        </button>
        <button
          type="button"
          className="stats-nav-btn"
          disabled={!canForward}
          onClick={goForward}
          title="Forward through panel filter history"
          aria-label="Forward"
        >
          →
        </button>
        <button
          type="button"
          className="stats-nav-btn stats-nav-clear"
          disabled={!canClear}
          onClick={clearPanelFilter}
          title="Clear type / author / hub-category filter"
        >
          Clear
        </button>
      </div>

      <div className="stats-summary">
        <div className="stats-summary-row">
          <span className="stats-summary-label">Packages</span>
          <span className="stats-summary-value">{stats.n.toLocaleString()}</span>
        </div>
        <div className="stats-summary-row">
          <span className="stats-summary-label">Total size</span>
          <span className="stats-summary-value">{formatSize(stats.totalSize)}</span>
        </div>
        <div className="stats-summary-row">
          <span className="stats-summary-label">Average</span>
          <span className="stats-summary-value">{formatSize(stats.avgSize)}</span>
        </div>
        <div className="stats-summary-row">
          <span className="stats-summary-label">Largest</span>
          <span className="stats-summary-value">{formatSize(stats.maxSize)}</span>
        </div>
        {truncated && (
          <div className="stats-truncated">
            stats for first {stats.n.toLocaleString()} of{" "}
            {totalMatched.toLocaleString()} matches
          </div>
        )}
      </div>

      <StatsSection
        title={viewMode === "advanced" ? "By hub category" : "By type"}
        rows={stats.types}
        total={stats.n}
        selectedKey={typeSelectedKey}
        onClick={handleTypeClick}
        sectionClass="stats-section-type"
      />

      <StatsSection
        title="By author"
        rows={authorsToShow}
        total={stats.n}
        selectedKey={selectedCreator || null}
        onClick={handleAuthorClick}
        sectionClass="stats-section-author"
        footer={
          (showStep20 || showReset) && (
            <div className="stats-section-footer">
              {showStep20 && (
                <button
                  type="button"
                  className="stats-show-all"
                  onClick={() =>
                    setAuthorLimit((l) =>
                      Math.min(l + AUTHOR_STEP, stats.authors.length),
                    )
                  }
                >
                  Show +{Math.min(AUTHOR_STEP, remainingAuthors)}
                </button>
              )}
              {showShowAll && (
                <button
                  type="button"
                  className="stats-show-all"
                  onClick={() => setAuthorLimit(stats.authors.length)}
                >
                  Show all (+{remainingAuthors} more)
                </button>
              )}
              {showReset && (
                <button
                  type="button"
                  className="stats-show-all stats-show-reset"
                  onClick={() => setAuthorLimit(AUTHOR_HEAD)}
                >
                  Top {AUTHOR_HEAD}
                </button>
              )}
            </div>
          )
        }
      />

      <StatsSection
        title="By size"
        rows={stats.sizes}
        total={stats.n}
        selectedKey={null}
        onClick={null}
        sectionClass="stats-section-size"
      />
    </aside>
  );
}

interface SectionProps {
  title: string;
  rows: Bucket[];
  total: number;
  selectedKey: string | null;
  onClick: ((key: string) => void) | null;
  sectionClass?: string;
  footer?: React.ReactNode;
}

function StatsSection({
  title,
  rows,
  total,
  selectedKey,
  onClick,
  sectionClass,
  footer,
}: SectionProps) {
  const max = rows.reduce((m, r) => (r.count > m ? r.count : m), 0);
  return (
    <div className={`stats-section ${sectionClass ?? ""}`}>
      <div className="stats-section-title">{title}</div>
      {rows.length === 0 ? (
        <div className="stats-empty">—</div>
      ) : (
        rows.map((r) => {
          const pct = total > 0 ? (r.count / total) * 100 : 0;
          const barPct = max > 0 ? (r.count / max) * 100 : 0;
          const clickable = onClick !== null && r.clickable && r.count > 0;
          const isSelected = selectedKey !== null && selectedKey === r.key;
          const cls = [
            "stats-row",
            clickable ? "stats-row-clickable" : "",
            isSelected ? "stats-row-selected" : "",
          ]
            .filter(Boolean)
            .join(" ");
          // Per-row accent override — cascades into .stats-bar-fill and the
          // selected-row stripe via the --row-accent custom property.
          const rowStyle = r.hue
            ? ({ ["--row-accent" as string]: r.hue } as React.CSSProperties)
            : undefined;
          return (
            <div
              key={r.key}
              className={cls}
              style={rowStyle}
              onClick={clickable ? () => onClick!(r.key) : undefined}
              role={clickable ? "button" : undefined}
              tabIndex={clickable ? 0 : undefined}
              onKeyDown={
                clickable
                  ? (e) => {
                      if (e.key === "Enter" || e.key === " ") {
                        e.preventDefault();
                        onClick!(r.key);
                      }
                    }
                  : undefined
              }
              title={
                clickable
                  ? isSelected
                    ? `Click to clear ${r.key}`
                    : `Filter by ${r.key}`
                  : undefined
              }
            >
              <div className="stats-bar-bg">
                <div className="stats-bar-fill" style={{ width: `${barPct}%` }} />
              </div>
              <div className="stats-row-content">
                <span className="stats-row-label">{r.key}</span>
                <span className="stats-row-count">
                  {r.count.toLocaleString()}
                  <span className="stats-row-pct">{pct.toFixed(0)}%</span>
                </span>
              </div>
            </div>
          );
        })
      )}
      {footer}
    </div>
  );
}
