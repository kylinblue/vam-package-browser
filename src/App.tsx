import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AuthorPicker } from "./components/AuthorPicker";
import { DetailView } from "./components/DetailView";
import { FacetPanel } from "./components/FacetPanel";
import { HubSyncView } from "./components/HubSyncView";
import { PackageGrid } from "./components/PackageGrid";
import { SelectionActionBar } from "./components/SelectionActionBar";
import { StatsPanel } from "./components/StatsPanel";
import { TypeChips } from "./components/TypeChips";
import {
  countPackages,
  generateThumbnails,
  listCreatorsWithCounts,
  listHubCategories,
  listTypeCounts,
  queryPackages,
  scanLibrary,
  setAddonRoot,
  setFavorite,
  setHidden,
  getSettings,
  type CreatorCount,
  type HubCategoryCount,
  type PackageRow,
  type PackageType,
  type SortField,
  type SortOrder,
  type ThumbProgress,
  type TypeCount,
} from "./lib/api";
import { HubCategoryChips } from "./components/HubCategoryChips";

const DEFAULT_ADDON_ROOT = "D:\\Games\\VAM\\AddonPackages";
const SAMPLE_LIMIT = 200;
const MIN_TILE_SIZE = 120;
// Cap matches `MAX_DIM` in src-tauri/src/thumbnails.rs — beyond that, tiles
// would upscale a 512px source and blur. Keep in sync if MAX_DIM ever changes.
const MAX_TILE_SIZE = 512;

function loadTileDim(key: string, fallback: number): number {
  const stored = Number(localStorage.getItem(key));
  if (Number.isFinite(stored) && stored >= MIN_TILE_SIZE && stored <= MAX_TILE_SIZE) {
    return stored;
  }
  return fallback;
}

export default function App() {
  const [addonRoot, setAddonRootState] = useState<string>(DEFAULT_ADDON_ROOT);
  const [search, setSearch] = useState("");
  const [selectedType, setSelectedType] = useState<PackageType | null>(null);
  const [selectedCreator, setSelectedCreator] = useState<string>("");
  const [missingPreview, setMissingPreview] = useState(false);
  const [favoritesOnly, setFavoritesOnly] = useState(false);
  const [includeHidden, setIncludeHidden] = useState(false);
  const [errorsOnly, setErrorsOnly] = useState(false);
  const [tileSize, setTileSize] = useState<number>(() =>
    loadTileDim("tileSize", 200),
  );
  const [selectedTags, setSelectedTags] = useState<string[]>([]);
  // Three-mode classification source selector:
  //   - "simple"  : heuristic package_type only, no extra metadata UI
  //   - "tagged"  : v4 LLM tag layer (TagChips + FacetPanel). Tag quality is
  //                 mixed, so this is currently an opt-in inspection mode.
  //   - "fetched" : hub-scraped metadata (category, billing tier, etc.).
  //                 Wires up in milestone 3 of the hub pivot.
  // Persists in localStorage. Old `facetPanelOpen` key is read once for a
  // soft migration: anyone who had the panel open lands in "tagged" mode.
  const [viewMode, setViewMode] = useState<"simple" | "tagged" | "fetched">(() => {
    const stored = localStorage.getItem("viewMode");
    if (stored === "simple" || stored === "tagged" || stored === "fetched") {
      return stored;
    }
    return localStorage.getItem("facetPanelOpen") === "1" ? "tagged" : "simple";
  });
  const [statsPanelVisible, setStatsPanelVisible] = useState<boolean>(
    () => localStorage.getItem("statsPanelVisible") === "1",
  );

  // ── Group select ──────────────────────────────────────────────────────────
  // `selectionMode` flips primary-click behavior to "toggle select" instead
  // of "open detail". Modifier-driven select (Ctrl / Shift) works in either
  // mode. Selection survives mode toggling so the user can dip out, refine
  // filters, then continue selecting.
  const [selectionMode, setSelectionMode] = useState(false);
  const [selectedIds, setSelectedIds] = useState<Set<number>>(new Set());
  // Last id that received a non-shift click. Shift-range fills from anchor
  // to the next target. Null when no anchor has been set yet (or it's been
  // filtered out of the visible packages — see toggleSelect's degradation).
  const [selectionAnchor, setSelectionAnchor] = useState<number | null>(null);

  // Size + date range filter inputs are kept as raw strings; empty = no bound.
  const [minSizeMb, setMinSizeMb] = useState("");
  const [maxSizeMb, setMaxSizeMb] = useState("");
  const [minDate, setMinDate] = useState("");
  const [maxDate, setMaxDate] = useState("");

  const [packages, setPackages] = useState<PackageRow[]>([]);
  const [totalMatched, setTotalMatched] = useState(0);
  // Hub-category filter state for Fetched mode. Mirrors selectedType in
  // spirit but for hub_category instead of heuristic package_type. The
  // "unidentified" virtual chip filters to packages that didn't match any
  // hub resource (hub_resource_id IS NULL).
  const [selectedHubCategory, setSelectedHubCategory] = useState<string | null>(null);
  const [hubCategoryCounts, setHubCategoryCounts] = useState<HubCategoryCount[]>([]);

  // Semantic-search state shelved alongside the Ask UI — see the commented
  // toolbar row below and TODO-semantic-search-ui.md. Reactivation: revert
  // this block and the related visiblePackages / headerCount branches via
  // git history (commit that introduced the shelving).
  const [detailPackageId, setDetailPackageId] = useState<number | null>(null);
  const [typeCounts, setTypeCounts] = useState<TypeCount[]>([]);
  const [creators, setCreators] = useState<CreatorCount[]>([]);
  const [sortBy, setSortBy] = useState<SortField>("creator");
  const [sortOrder, setSortOrder] = useState<SortOrder>("asc");
  // Viewport width is owned here (lifted from PackageGrid) so we can snap the
  // tile-size slider to viewport-aware "neat" positions in real time.
  const [viewportWidth, setViewportWidth] = useState(0);

  const [scanning, setScanning] = useState(false);
  const [generatingThumbs, setGeneratingThumbs] = useState(false);
  const [thumbProgress, setThumbProgress] = useState<{ done: number; total: number } | null>(null);
  const [thumbVersions, setThumbVersions] = useState<Record<number, number>>({});
  const [loading, setLoading] = useState(false);
  const [statusMsg, setStatusMsg] = useState("Ready.");

  // Batch thumb-progress events so we don't re-render per-image.
  const pendingVersionsRef = useRef<Record<number, number>>({});
  const flushTimerRef = useRef<number | null>(null);

  useEffect(() => {
    localStorage.setItem("tileSize", String(tileSize));
  }, [tileSize]);

  useEffect(() => {
    localStorage.setItem("viewMode", viewMode);
  }, [viewMode]);

  useEffect(() => {
    localStorage.setItem("statsPanelVisible", statsPanelVisible ? "1" : "0");
  }, [statsPanelVisible]);

  // Re-snap tileSize when the viewport changes (e.g. window resize). Keeps
  // the column count stable in spirit — picks the nearest perfect-fit size
  // for the new width.
  useEffect(() => {
    if (viewportWidth <= 0) return;
    const snapped = snapTileSize(tileSize, viewportWidth);
    if (snapped !== tileSize) setTileSize(snapped);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [viewportWidth]);

  const debouncedSearch = useDebounced(search, 80);

  // Signature of every filter that the StatsPanel does NOT manage. Any
  // change here resets the panel's navigation history — its breadcrumb of
  // clicked filters only makes sense within a stable surrounding context.
  // (Panel-managed axes: selectedType, selectedCreator, selectedHubCategory.)
  const externalFilterSignature = useMemo(
    () =>
      JSON.stringify({
        search: debouncedSearch,
        tags: selectedTags,
        min: minSizeMb,
        max: maxSizeMb,
        minD: minDate,
        maxD: maxDate,
        fav: favoritesOnly,
        hidden: includeHidden,
        noThumb: missingPreview,
        err: errorsOnly,
        view: viewMode,
      }),
    [
      debouncedSearch,
      selectedTags,
      minSizeMb,
      maxSizeMb,
      minDate,
      maxDate,
      favoritesOnly,
      includeHidden,
      missingPreview,
      errorsOnly,
      viewMode,
    ],
  );

  // Client-side filter for the locally-mutated flags (errorsOnly is purely
  // client-side; the others mirror the backend filter so optimistic toggles
  // on individual tiles update the view instantly).
  const visiblePackages = useMemo(() => {
    if (errorsOnly) return packages.filter((p) => p.error);
    return packages.filter((p) => {
      if (!includeHidden && p.is_hidden) return false;
      if (favoritesOnly && !p.is_favorite) return false;
      return true;
    });
  }, [packages, errorsOnly, includeHidden, favoritesOnly]);

  // Count of distinct filter axes currently set away from default. Drives
  // the toolbar's Clear-filters button (label + disabled state). Excludes
  // sort fields and `includeHidden` — sort is a view choice, not a filter,
  // and `includeHidden=true` is permissive (a reset to `false` would be a
  // surprising tightening). All the rest are "narrows the visible set"
  // toggles or selectors.
  const activeFilterCount = useMemo(() => {
    let n = 0;
    if (debouncedSearch) n++;
    if (selectedType !== null) n++;
    if (selectedCreator !== "") n++;
    if (selectedHubCategory !== null) n++;
    if (selectedTags.length > 0) n++;
    if (favoritesOnly) n++;
    if (missingPreview) n++;
    if (errorsOnly) n++;
    if (minSizeMb !== "" || maxSizeMb !== "") n++;
    if (minDate !== "" || maxDate !== "") n++;
    return n;
  }, [
    debouncedSearch,
    selectedType,
    selectedCreator,
    selectedHubCategory,
    selectedTags,
    favoritesOnly,
    missingPreview,
    errorsOnly,
    minSizeMb,
    maxSizeMb,
    minDate,
    maxDate,
  ]);
  const hasActiveFilters = activeFilterCount > 0;

  const clearAllFilters = useCallback(() => {
    setSearch("");
    setSelectedType(null);
    setSelectedCreator("");
    setSelectedHubCategory(null);
    setSelectedTags([]);
    setFavoritesOnly(false);
    setMissingPreview(false);
    setErrorsOnly(false);
    setMinSizeMb("");
    setMaxSizeMb("");
    setMinDate("");
    setMaxDate("");
  }, []);

  // Tile click → selection mutator. Behavior:
  //   range=true (Shift)        — fill from selectionAnchor to id in the
  //                                current visiblePackages order. If the
  //                                anchor isn't in the view (e.g. filter
  //                                changed since), degrade to a plain
  //                                toggle on `id`.
  //   additive=false            — replace prior selection with just `id`.
  //   default                   — toggle `id` on/off; keep other selections.
  // Only non-range clicks update the anchor.
  const toggleSelect = useCallback(
    (id: number, additive: boolean, range: boolean) => {
      setSelectedIds((prev) => {
        if (range && selectionAnchor !== null) {
          const ids = visiblePackages.map((p) => p.id);
          const a = ids.indexOf(selectionAnchor);
          const b = ids.indexOf(id);
          if (a >= 0 && b >= 0) {
            const [lo, hi] = a < b ? [a, b] : [b, a];
            const next = new Set(additive ? prev : []);
            for (let i = lo; i <= hi; i++) next.add(ids[i]);
            return next;
          }
          // Anchor scrolled out of view — fall through to plain toggle.
        }
        const next = new Set(additive ? prev : []);
        if (next.has(id)) next.delete(id);
        else next.add(id);
        return next;
      });
      if (!range) setSelectionAnchor(id);
    },
    [visiblePackages, selectionAnchor],
  );

  const loadResults = useCallback(async () => {
    setLoading(true);
    try {
      const creatorFilter = creators.some((c) => c.creator === selectedCreator)
        ? selectedCreator
        : undefined;
      const minBytes = parseFiniteNumber(minSizeMb);
      const maxBytes = parseFiniteNumber(maxSizeMb);
      const filter = {
        search: debouncedSearch || undefined,
        creator: creatorFilter,
        package_type: selectedType ?? undefined,
        missing_preview: missingPreview || undefined,
        favorites_only: favoritesOnly || undefined,
        include_hidden: includeHidden || undefined,
        min_size: minBytes !== undefined ? Math.round(minBytes * 1024 * 1024) : undefined,
        max_size: maxBytes !== undefined ? Math.round(maxBytes * 1024 * 1024) : undefined,
        min_mtime: dateStringToUnixStart(minDate),
        // Inclusive end-of-day: add (86400 - 1) so a maxDate of "2026-05-15"
        // covers everything modified on that calendar day.
        max_mtime: dateStringToUnixEnd(maxDate),
        sort_by: sortBy,
        sort_order: sortOrder,
        limit: 10000,
        // Only push tag filter when in tagged mode — otherwise switching
        // away from Tagged would leave an invisible filter applied. The
        // selection is preserved in state, so switching back restores it.
        tags: viewMode === "tagged" && selectedTags.length > 0 ? selectedTags : undefined,
        // Hub category filter is only relevant in fetched mode.
        hub_category:
          viewMode === "fetched" && selectedHubCategory !== null
            ? selectedHubCategory
            : undefined,
      };
      const [rows, total] = await Promise.all([
        queryPackages(filter),
        countPackages(filter),
      ]);
      setPackages(rows);
      setTotalMatched(total);
    } catch (e) {
      setStatusMsg(`query error: ${e}`);
    } finally {
      setLoading(false);
    }
  }, [
    debouncedSearch,
    selectedType,
    selectedCreator,
    missingPreview,
    favoritesOnly,
    includeHidden,
    minSizeMb,
    maxSizeMb,
    minDate,
    maxDate,
    sortBy,
    sortOrder,
    creators,
    selectedTags,
    selectedHubCategory,
    viewMode,
  ]);

  const refreshTypeCountsAndCreators = useCallback(async () => {
    try {
      const [tc, cs, hc] = await Promise.all([
        listTypeCounts(),
        listCreatorsWithCounts(),
        listHubCategories(),
      ]);
      setTypeCounts(tc);
      setCreators(cs);
      setHubCategoryCounts(hc);
    } catch (e) {
      console.error("refresh aggregates:", e);
    }
  }, []);

  // Bootstrap.
  useEffect(() => {
    (async () => {
      try {
        const s = await getSettings();
        if (s.addon_root) setAddonRootState(s.addon_root);
      } catch {
        /* pre-scan: settings may not be set */
      }
      await refreshTypeCountsAndCreators();
    })().catch((e) => setStatusMsg(`init error: ${e}`));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Re-query whenever any filter changes.
  useEffect(() => {
    loadResults().catch(() => {});
  }, [loadResults]);

  // Listen for thumb-progress events from the Rust backend during generation.
  useEffect(() => {
    let unlisten: UnlistenFn | undefined;
    (async () => {
      unlisten = await listen<ThumbProgress>("thumb-progress", (event) => {
        const p = event.payload;
        setThumbProgress({ done: p.done, total: p.total });
        if (p.ok) {
          pendingVersionsRef.current[p.id] = p.done;
          if (flushTimerRef.current === null) {
            flushTimerRef.current = window.setTimeout(() => {
              setThumbVersions((prev) => ({ ...prev, ...pendingVersionsRef.current }));
              pendingVersionsRef.current = {};
              flushTimerRef.current = null;
            }, 80);
          }
        }
      });
    })();
    return () => {
      if (unlisten) unlisten();
      if (flushTimerRef.current !== null) {
        window.clearTimeout(flushTimerRef.current);
        flushTimerRef.current = null;
      }
    };
  }, []);

  const runScan = useCallback(
    async (limit: number | null) => {
      setScanning(true);
      setStatusMsg(
        limit
          ? `Scanning first ${limit} packages from ${addonRoot}…`
          : `Scanning full library at ${addonRoot}…`,
      );
      try {
        await setAddonRoot(addonRoot);
        const result = await scanLibrary(addonRoot, limit);
        setStatusMsg(
          `Scan done — ${result.scanned} packages, ${result.errors} errors, ${result.elapsed_ms} ms.`,
        );
        await refreshTypeCountsAndCreators();
        await loadResults();
      } catch (e) {
        setStatusMsg(`scan error: ${e}`);
      } finally {
        setScanning(false);
      }
    },
    [addonRoot, loadResults, refreshTypeCountsAndCreators],
  );

  // (Hub sync UI is shelved — backend kept compiled for later. See commands::start_hub_sync.)

  const runGenerateThumbnails = useCallback(async () => {
    setGeneratingThumbs(true);
    setThumbProgress({ done: 0, total: 0 });
    setStatusMsg("Generating thumbnails…");
    try {
      const result = await generateThumbnails();
      setStatusMsg(
        `Thumbnails — generated ${result.generated.toLocaleString()}, ` +
          `already fresh ${result.already_fresh.toLocaleString()}, ` +
          `errors ${result.errors.toLocaleString()}, ${result.elapsed_ms} ms.`,
      );
      setThumbVersions((prev) => ({ ...prev, ...pendingVersionsRef.current }));
      pendingVersionsRef.current = {};
    } catch (e) {
      setStatusMsg(`thumbnail error: ${e}`);
    } finally {
      setGeneratingThumbs(false);
      setThumbProgress(null);
    }
  }, []);

  const onToggleFavorite = useCallback(async (id: number, current: boolean) => {
    try {
      await setFavorite(id, !current);
      setPackages((prev) =>
        prev.map((p) => (p.id === id ? { ...p, is_favorite: !current } : p)),
      );
    } catch (e) {
      setStatusMsg(`favorite error: ${e}`);
    }
  }, []);

  const onToggleHidden = useCallback(async (id: number, current: boolean) => {
    try {
      await setHidden(id, !current);
      setPackages((prev) =>
        prev.map((p) => (p.id === id ? { ...p, is_hidden: !current } : p)),
      );
      // Refresh type chip counts since hidden packages are excluded from them.
      refreshTypeCountsAndCreators().catch(() => {});
    } catch (e) {
      setStatusMsg(`hide error: ${e}`);
    }
  }, [refreshTypeCountsAndCreators]);

  const headerCount = useMemo(() => {
    if (errorsOnly) {
      return `${visiblePackages.length.toLocaleString()} error${visiblePackages.length === 1 ? "" : "s"} (of ${packages.length.toLocaleString()} loaded)`;
    }
    if (packages.length === 0 && totalMatched === 0) return "0 packages";
    if (packages.length < totalMatched) {
      return `Showing ${packages.length.toLocaleString()} of ${totalMatched.toLocaleString()} matches`;
    }
    return `${visiblePackages.length.toLocaleString()} package${visiblePackages.length === 1 ? "" : "s"}`;
  }, [packages.length, totalMatched, errorsOnly, visiblePackages.length]);

  return (
    <div className="app">
      <div className="toolbar">
        <div className="toolbar-row">
          <input
            type="text"
            value={addonRoot}
            onChange={(e) => setAddonRootState(e.target.value)}
            style={{ flex: "0 1 360px" }}
            placeholder="AddonPackages path"
          />
          <button onClick={() => runScan(SAMPLE_LIMIT)} disabled={scanning}>
            {scanning ? "Scanning…" : `Scan sample (${SAMPLE_LIMIT})`}
          </button>
          <button onClick={() => runScan(null)} disabled={scanning}>
            Scan all
          </button>
          <button onClick={runGenerateThumbnails} disabled={scanning || generatingThumbs}>
            {generatingThumbs
              ? thumbProgress
                ? `Thumbs ${thumbProgress.done}/${thumbProgress.total}`
                : "Thumbs…"
              : "Generate thumbnails"}
          </button>
          <div className="seg-control" role="radiogroup" aria-label="Classification source" style={{ marginLeft: "auto" }}>
            <button
              type="button"
              role="radio"
              aria-checked={viewMode === "simple"}
              className={`seg-btn ${viewMode === "simple" ? "active" : ""}`}
              onClick={() => setViewMode("simple")}
              title="Heuristic package_type only — no tag UI, no hub data."
            >
              Simple
            </button>
            <button
              type="button"
              role="radio"
              aria-checked={viewMode === "tagged"}
              className={`seg-btn ${viewMode === "tagged" ? "active" : ""}`}
              onClick={() => setViewMode("tagged")}
              title="LLM-derived v4 tags. Mixed quality — inspection mode."
            >
              Tagged{selectedTags.length > 0 && ` (${selectedTags.length})`}
            </button>
            <button
              type="button"
              role="radio"
              aria-checked={viewMode === "fetched"}
              className={`seg-btn ${viewMode === "fetched" ? "active" : ""}`}
              onClick={() => setViewMode("fetched")}
              title="Hub-scraped metadata. Wires up in the hub-pivot milestone."
            >
              Fetched
            </button>
          </div>
        </div>

        {viewMode === "fetched" ? (
          <HubCategoryChips
            counts={hubCategoryCounts}
            selected={selectedHubCategory}
            onSelect={setSelectedHubCategory}
          />
        ) : (
          <TypeChips counts={typeCounts} selected={selectedType} onSelect={setSelectedType} />
        )}

        <div className="toolbar-row">
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Filter by creator or package name…"
            style={{ flex: "0 1 280px" }}
          />
          <label className="toolbar-toggle" style={{ marginLeft: "auto" }}>
            <input
              type="checkbox"
              checked={favoritesOnly}
              onChange={(e) => setFavoritesOnly(e.target.checked)}
            />
            <span>★ Favorites only</span>
          </label>
          <label className="toolbar-toggle">
            <input
              type="checkbox"
              checked={includeHidden}
              onChange={(e) => setIncludeHidden(e.target.checked)}
            />
            <span>👁 Show hidden</span>
          </label>
          <label className="toolbar-toggle">
            <input
              type="checkbox"
              checked={missingPreview}
              onChange={(e) => setMissingPreview(e.target.checked)}
            />
            <span>📷 No thumbnail only</span>
          </label>
          <label className="toolbar-toggle">
            <input
              type="checkbox"
              checked={errorsOnly}
              onChange={(e) => setErrorsOnly(e.target.checked)}
            />
            <span>errors only</span>
          </label>
          <label className="toolbar-toggle">
            <input
              type="checkbox"
              checked={statsPanelVisible}
              onChange={(e) => setStatsPanelVisible(e.target.checked)}
            />
            <span>📊 Stats</span>
          </label>
          <label className="toolbar-toggle">
            <input
              type="checkbox"
              checked={selectionMode}
              onChange={(e) => setSelectionMode(e.target.checked)}
            />
            <span>📋 Select{selectedIds.size > 0 ? ` (${selectedIds.size})` : ""}</span>
          </label>
          <button
            type="button"
            className="toolbar-clear-filters"
            onClick={clearAllFilters}
            disabled={!hasActiveFilters}
            title={
              hasActiveFilters
                ? `Clear ${activeFilterCount} active filter${activeFilterCount === 1 ? "" : "s"}`
                : "No active filters"
            }
          >
            Clear filters{activeFilterCount > 0 ? ` (${activeFilterCount})` : ""}
          </button>

          <label className="toolbar-sort">
            <span>Sort</span>
            <select
              value={`${sortBy}:${sortOrder}`}
              onChange={(e) => {
                const [f, o] = e.target.value.split(":") as [SortField, SortOrder];
                setSortBy(f);
                setSortOrder(o);
              }}
            >
              <option value="creator:asc">Creator A→Z</option>
              <option value="creator:desc">Creator Z→A</option>
              <option value="name:asc">Name A→Z</option>
              <option value="name:desc">Name Z→A</option>
              <option value="size:desc">Size largest</option>
              <option value="size:asc">Size smallest</option>
              <option value="package_mtime:desc">Packaged newest</option>
              <option value="package_mtime:asc">Packaged oldest</option>
              <option value="scanned:desc">Added newest</option>
              <option value="scanned:asc">Added oldest</option>
            </select>
          </label>

          <div className="toolbar-zoom">
            <label
              className="toolbar-zoom-group"
              title="Tile size. Snaps to viewport-aware values that perfectly fill the grid row."
            >
              <span>□</span>
              <input
                type="range"
                min={MIN_TILE_SIZE}
                max={MAX_TILE_SIZE}
                step={4}
                value={tileSize}
                onChange={(e) =>
                  setTileSize(snapTileSize(Number(e.target.value), viewportWidth))
                }
              />
              <span className="toolbar-zoom-n">{tileSize}</span>
            </label>
          </div>
        </div>

        {/* Semantic-search ("Ask…") row shelved pending a v4 tag retag pass.
            Reactivation path:
              - revert this comment block and the related state, visiblePackages
                and headerCount branches in this file (see git history for the
                shelving commit);
              - restore the SimilarSection block in DetailView.tsx;
              - restore the model warm-up std::thread::spawn in lib.rs setup().
            The Tauri commands (search_families, search_similar_families,
            get_packages_by_ids) and the embedding/ Rust module remain wired,
            so the backend is ready to use. See TODO-semantic-search-ui.md. */}

        <div className="toolbar-row">
          <AuthorPicker
            creators={creators}
            value={selectedCreator}
            onChange={setSelectedCreator}
            placeholder="Author"
          />
        </div>

        <div className="toolbar-row">
          <span className="toolbar-range-group" title="File size range in megabytes">
            <span className="toolbar-range-label">Size</span>
            <input
              type="number"
              min={0}
              step={1}
              value={minSizeMb}
              onChange={(e) => setMinSizeMb(e.target.value)}
              placeholder="min"
              className="toolbar-range-input"
            />
            <span className="toolbar-range-dash">—</span>
            <input
              type="number"
              min={0}
              step={1}
              value={maxSizeMb}
              onChange={(e) => setMaxSizeMb(e.target.value)}
              placeholder="max"
              className="toolbar-range-input"
            />
            <span className="toolbar-range-unit">MB</span>
            {(minSizeMb || maxSizeMb) && (
              <button
                type="button"
                onClick={() => {
                  setMinSizeMb("");
                  setMaxSizeMb("");
                }}
                title="Clear size filter"
              >
                ×
              </button>
            )}
          </span>

          <span className="toolbar-range-group" title="Last modified date range">
            <span className="toolbar-range-label">Modified</span>
            <input
              type="date"
              value={minDate}
              onChange={(e) => setMinDate(e.target.value)}
              className="toolbar-range-input toolbar-range-date"
            />
            <span className="toolbar-range-dash">—</span>
            <input
              type="date"
              value={maxDate}
              onChange={(e) => setMaxDate(e.target.value)}
              className="toolbar-range-input toolbar-range-date"
            />
            {(minDate || maxDate) && (
              <button
                type="button"
                onClick={() => {
                  setMinDate("");
                  setMaxDate("");
                }}
                title="Clear date filter"
              >
                ×
              </button>
            )}
          </span>
        </div>

        {/* Hub sync controls live inside the toolbar in Fetched mode so the
            collapsed handle sits flush with the rest of the filter UI. */}
        {viewMode === "fetched" && <HubSyncView />}
      </div>

      <div className="content-area">
        {viewMode === "tagged" && (
          <FacetPanel
            selectedTags={selectedTags}
            onChange={setSelectedTags}
          />
        )}
        <PackageGrid
          packages={visiblePackages}
          thumbVersions={thumbVersions}
          tileSize={tileSize}
          displayMode={viewMode === "fetched" ? "hub" : "heuristic"}
          onToggleFavorite={onToggleFavorite}
          onToggleHidden={onToggleHidden}
          onOpenDetail={setDetailPackageId}
          onFilterByAuthor={setSelectedCreator}
          onFilterByType={(t) => setSelectedType(t as PackageType)}
          onViewportWidth={setViewportWidth}
          selectionMode={selectionMode}
          selectedIds={selectedIds}
          onToggleSelect={toggleSelect}
        />
        {statsPanelVisible && (
          <StatsPanel
            packages={visiblePackages}
            totalMatched={totalMatched}
            truncated={totalMatched > visiblePackages.length}
            viewMode={viewMode}
            selectedType={selectedType}
            selectedCreator={selectedCreator}
            selectedHubCategory={selectedHubCategory}
            setSelectedType={setSelectedType}
            setSelectedCreator={setSelectedCreator}
            setSelectedHubCategory={setSelectedHubCategory}
            externalFilterSignature={externalFilterSignature}
          />
        )}
      </div>

      {detailPackageId !== null && (
        <DetailView
          packageId={detailPackageId}
          thumbVersion={thumbVersions[detailPackageId] ?? 0}
          viewMode={viewMode}
          onClose={() => setDetailPackageId(null)}
          onFilterByAuthor={(a) => {
            setSelectedCreator(a);
            setDetailPackageId(null);
          }}
          onFilterByType={(t) => {
            setSelectedType(t as PackageType);
            setDetailPackageId(null);
          }}
          onOpenPackage={setDetailPackageId}
        />
      )}

      {selectedIds.size > 0 && (
        <SelectionActionBar
          selection={[...selectedIds]}
          viewMode={viewMode}
          onClear={() => {
            setSelectedIds(new Set());
            setSelectionAnchor(null);
          }}
          onActionApplied={() => {
            // Pull a fresh result set + aggregates after the backend
            // applied a bulk action. Selection is left intact so the user
            // can stack further actions on the same set if they want.
            loadResults();
            refreshTypeCountsAndCreators();
          }}
        />
      )}

      <div className="statusbar">
        <span>{headerCount}</span>
        {loading && <span>(loading…)</span>}
        <span style={{ marginLeft: "auto" }}>{statusMsg}</span>
      </div>
    </div>
  );
}

function parseFiniteNumber(s: string): number | undefined {
  if (!s.trim()) return undefined;
  const n = Number(s);
  return Number.isFinite(n) && n >= 0 ? n : undefined;
}

const GRID_GAP = 8; // must match GAP in PackageGrid.tsx
const GRID_PADDING_X = 8; // matches .grid-row horizontal padding

/// Given a viewport width, return tile sizes that exactly fill the grid for
/// 1..N columns (no leftover horizontal space). Always at least one entry.
function neatTileSizes(viewportWidth: number): number[] {
  if (viewportWidth <= 0) return [];
  const usable = Math.max(0, viewportWidth - GRID_PADDING_X * 2);
  const sizes = new Set<number>();
  for (let n = 1; n <= 40; n++) {
    const totalGap = (n - 1) * GRID_GAP;
    const t = Math.floor((usable - totalGap) / n);
    if (t < MIN_TILE_SIZE) break;
    if (t > MAX_TILE_SIZE) continue;
    sizes.add(t);
  }
  return [...sizes].sort((a, b) => a - b);
}

/// Snap a target tile size to the nearest "neat" (perfect-fit) size for the
/// current viewport. If no neat sizes exist (viewport not measured yet),
/// returns the target unchanged.
function snapTileSize(target: number, viewportWidth: number): number {
  const neat = neatTileSizes(viewportWidth);
  if (neat.length === 0) return target;
  return neat.reduce((best, n) =>
    Math.abs(n - target) < Math.abs(best - target) ? n : best,
  );
}

function dateStringToUnixStart(s: string): number | undefined {
  if (!s) return undefined;
  const d = new Date(`${s}T00:00:00`);
  const t = d.getTime();
  return Number.isFinite(t) ? Math.floor(t / 1000) : undefined;
}

function dateStringToUnixEnd(s: string): number | undefined {
  const start = dateStringToUnixStart(s);
  return start === undefined ? undefined : start + 86_400 - 1;
}

function useDebounced<T>(value: T, ms: number): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}
