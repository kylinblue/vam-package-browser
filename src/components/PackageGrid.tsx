import { useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  openExternalUrl,
  revealInFolder,
  thumbUrl,
  vamHubAuthorSearchUrl,
  vamHubPackageSearchUrl,
  type PackageRow,
} from "../lib/api";
import { ContextMenu, type MenuItem } from "./ContextMenu";

const GAP = 8;
/** Fixed height of the meta strip below each tile's thumbnail. Keep in sync
 *  with .tile-meta padding+content in styles.css. */
const META_HEIGHT = 64;

const TYPE_EMOJI: Record<string, string> = {
  Scene:    "🎬",
  Look:     "🧍",
  Morph:    "💪",
  Texture:  "🖼️",
  Clothing: "👕",
  Hair:     "👩",
  Plugin:   "🔌",
  Asset:    "📖",
  Pose:     "🕺",
  Sound:    "🔊",
  SubScene: "🎭",
  Mixed:    "📦",
  Unknown:  "❓",
};

interface Props {
  packages: PackageRow[];
  /** Per-package version counter; bumping triggers an <img> reload for that tile. */
  thumbVersions: Record<number, number>;
  /** Square thumbnail edge in px. Total tile height = tileSize + META_HEIGHT. */
  tileSize: number;
  onToggleFavorite: (id: number, current: boolean) => void;
  onToggleHidden: (id: number, current: boolean) => void;
  onOpenDetail: (id: number) => void;
  onFilterByAuthor: (author: string) => void;
  onFilterByType: (type: string) => void;
  /** Reports the grid viewport width to the parent so the toolbar can do
   *  viewport-aware tile-size snapping. */
  onViewportWidth: (px: number) => void;
  /** "heuristic" (default) shows package_type-based badge + filename-based
   *  title. "hub" overrides those with hub_category + hub_title when the
   *  package has been matched on the hub; falls back to heuristic per-tile
   *  for unmatched packages so the grid stays populated. */
  displayMode?: "heuristic" | "hub";
  /** Group-select state. When `selectionMode` is true, primary clicks toggle
   *  selection instead of opening the detail view, and a checkbox overlay
   *  becomes visible on every tile. Outside select mode, Ctrl/Meta-click on
   *  a tile still toggles selection (power-user path), and Shift-click does
   *  range-select from the last clicked tile.
   *  `onToggleSelect(id, additive, range)` — `additive=false` clears prior
   *  selection; `range=true` requests a range fill from the last anchor. */
  selectionMode: boolean;
  selectedIds: Set<number>;
  onToggleSelect: (id: number, additive: boolean, range: boolean) => void;
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/// Compact YYYY-MM date for tile corners. Drops the day to fit in tight space
/// while still giving the user enough signal to spot "from 2020" vs "last week".
function formatShortDate(unix: number): string {
  const d = new Date(unix * 1000);
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  return `${y}-${m}`;
}

export function PackageGrid({
  packages,
  thumbVersions,
  tileSize,
  onToggleFavorite,
  onToggleHidden,
  onOpenDetail,
  onFilterByAuthor,
  onFilterByType,
  onViewportWidth,
  displayMode = "heuristic",
  selectionMode,
  selectedIds,
  onToggleSelect,
}: Props) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(0);

  useEffect(() => {
    const el = viewportRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const w = entries[0].contentRect.width;
      setWidth(w);
      onViewportWidth(w);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [onViewportWidth]);

  const tileTotalHeight = tileSize + META_HEIGHT;
  // Account for the 8px horizontal padding on each side of .grid-row.
  const usableWidth = Math.max(0, width - GAP * 2);
  const columns = Math.max(
    1,
    Math.floor((usableWidth + GAP) / (tileSize + GAP)),
  );
  const rowCount = Math.ceil(packages.length / columns);

  const rowVirtualizer = useVirtualizer({
    count: rowCount,
    getScrollElement: () => viewportRef.current,
    estimateSize: () => tileTotalHeight + GAP,
    overscan: 6,
  });

  // @tanstack/react-virtual caches per-row sizes; when tile dimensions or
  // column count change, the cached numbers go stale and rows visually overlap
  // until something forces a remeasure. Do it ourselves.
  useEffect(() => {
    rowVirtualizer.measure();
  }, [tileTotalHeight, columns, rowVirtualizer]);

  // Fixed pixel column widths so the thumbnail is *exactly* tileSize, not
  // stretched by `1fr`. Leftover horizontal space sits to the right of the grid.
  const gridTemplateColumns = useMemo(
    () => `repeat(${columns}, ${tileSize}px)`,
    [columns, tileSize],
  );

  if (packages.length === 0) {
    return (
      <div ref={viewportRef} className="grid-viewport">
        <div className="empty-state">
          <div>No packages yet.</div>
          <div style={{ fontSize: 12 }}>
            Set your AddonPackages folder and run a scan.
          </div>
        </div>
      </div>
    );
  }

  return (
    <div ref={viewportRef} className="grid-viewport">
      <div
        style={{
          height: rowVirtualizer.getTotalSize(),
          width: "100%",
          position: "relative",
        }}
      >
        {rowVirtualizer.getVirtualItems().map((virtualRow) => {
          const start = virtualRow.index * columns;
          const rowItems = packages.slice(start, start + columns);
          return (
            <div
              key={virtualRow.key}
              className="grid-row"
              style={{
                position: "absolute",
                top: 0,
                left: 0,
                width: "100%",
                transform: `translateY(${virtualRow.start}px)`,
                height: tileTotalHeight,
                gridTemplateColumns,
              }}
            >
              {rowItems.map((pkg) => (
                <Tile
                  key={pkg.id}
                  pkg={pkg}
                  thumbVersion={thumbVersions[pkg.id] ?? 0}
                  tileSize={tileSize}
                  displayMode={displayMode}
                  onToggleFavorite={onToggleFavorite}
                  onToggleHidden={onToggleHidden}
                  onOpenDetail={onOpenDetail}
                  onFilterByAuthor={onFilterByAuthor}
                  onFilterByType={onFilterByType}
                  selectionMode={selectionMode}
                  isSelected={selectedIds.has(pkg.id)}
                  onToggleSelect={onToggleSelect}
                />
              ))}
            </div>
          );
        })}
      </div>
    </div>
  );
}

const CATEGORY_EMOJI: ReadonlyArray<readonly [keyof PackageRow, string, string]> = [
  ["scene_count",    "🎬", "scenes"],
  ["look_count",     "🧍", "looks"],
  ["plugin_count",   "🔌", "plugins"],
  ["clothing_count", "👕", "clothing"],
  ["hair_count",     "👩", "hair"],
  ["pose_count",     "🕺", "poses"],
  ["subscene_count", "🎭", "subscenes"],
];

function categoryStrip(pkg: PackageRow): { emoji: string; n: number; label: string }[] {
  const out: { emoji: string; n: number; label: string }[] = [];
  for (const [key, emoji, label] of CATEGORY_EMOJI) {
    const n = pkg[key] as number;
    if (n > 0) out.push({ emoji, n, label });
  }
  return out;
}

interface TileProps {
  pkg: PackageRow;
  thumbVersion: number;
  tileSize: number;
  displayMode: "heuristic" | "hub";
  onToggleFavorite: (id: number, current: boolean) => void;
  onToggleHidden: (id: number, current: boolean) => void;
  onOpenDetail: (id: number) => void;
  onFilterByAuthor: (author: string) => void;
  onFilterByType: (type: string) => void;
  selectionMode: boolean;
  isSelected: boolean;
  onToggleSelect: (id: number, additive: boolean, range: boolean) => void;
}

function Tile({
  pkg,
  thumbVersion,
  tileSize,
  displayMode,
  onToggleFavorite,
  onToggleHidden,
  onOpenDetail,
  onFilterByAuthor,
  onFilterByType,
  selectionMode,
  isSelected,
  onToggleSelect,
}: TileProps) {
  const filename = pkg.var_path.split(/[\\/]/).pop() ?? pkg.var_path;
  // Hub-mode overrides: prefer hub_title / hub_category when available;
  // fall back to local fields when the package isn't pinned. The boolean
  // tells the JSX below which badge to render (heuristic emoji+text vs
  // hub category text).
  const useHub = displayMode === "hub" && !!pkg.hub_resource_id;
  const displayTitle = useHub && pkg.hub_title ? pkg.hub_title : (pkg.package_name || filename);
  const displayBadge = useHub && pkg.hub_category ? pkg.hub_category : pkg.package_type;
  const isPaid = useHub && pkg.hub_billing_tier !== null && pkg.hub_billing_tier !== undefined;
  const isOffsite = useHub && pkg.hub_is_hub_hosted === 0;
  // "ghost" = in hub mode but no hub match → tile is dim so the user
  // sees they're outside the hub-data picture for these.
  const isHubGhost = displayMode === "hub" && !pkg.hub_resource_id;
  const items = categoryStrip(pkg);
  const itemsTooltip = items.map((i) => `${i.n} ${i.label}`).join(", ");
  const fullTitle = pkg.error
    ? `${pkg.var_path}\n\nERROR: ${pkg.error}`
    : `${pkg.creator}.${pkg.package_name}.${pkg.version}\n${pkg.var_path}${itemsTooltip ? `\n\nContains: ${itemsTooltip}` : ""}`;

  // Default to visible; the parent thumb area has a dark background so the
  // brief moment before paint isn't jarring. Only hide on actual 404/error.
  // This avoids the race where browser memory-cache serves the image before
  // React's onLoad handler can fire, which left tiles stuck invisible.
  const [thumbFailed, setThumbFailed] = useState(false);
  useEffect(() => {
    setThumbFailed(false);
  }, [thumbVersion, pkg.id]);

  const [menuPos, setMenuPos] = useState<{ x: number; y: number } | null>(null);
  const onContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    setMenuPos({ x: e.clientX, y: e.clientY });
  };

  const menuItems: MenuItem[] = pkg.error
    ? [
        {
          label: "Reveal in folder",
          onClick: () => void revealInFolder(pkg.var_path),
        },
        {
          label: "Copy file path",
          onClick: () => void navigator.clipboard.writeText(pkg.var_path),
        },
        { label: "", onClick: () => {}, divider: true },
        {
          label: "Copy error message",
          onClick: () => void navigator.clipboard.writeText(pkg.error ?? ""),
        },
      ]
    : [
        {
          label: "Open details…",
          onClick: () => onOpenDetail(pkg.id),
        },
        { label: "", onClick: () => {}, divider: true },
        {
          label: `Filter by author: ${pkg.creator || "(none)"}`,
          onClick: () => onFilterByAuthor(pkg.creator),
          disabled: !pkg.creator,
        },
        {
          label: `Filter by type: ${pkg.package_type}`,
          onClick: () => onFilterByType(pkg.package_type),
        },
        { label: "", onClick: () => {}, divider: true },
        {
          label: "Reveal in folder",
          onClick: () => void revealInFolder(pkg.var_path),
        },
        {
          label: "Copy file path",
          onClick: () => void navigator.clipboard.writeText(pkg.var_path),
        },
        {
          label: `Copy ref (${pkg.creator}.${pkg.package_name}.${pkg.version})`,
          onClick: () =>
            void navigator.clipboard.writeText(
              `${pkg.creator}.${pkg.package_name}.${pkg.version}`,
            ),
        },
        {
          label: "Search author on VaM Hub",
          onClick: () =>
            void openExternalUrl(vamHubAuthorSearchUrl(pkg.creator)),
        },
        {
          label: "Search package on VaM Hub",
          onClick: () =>
            void openExternalUrl(vamHubPackageSearchUrl(pkg.package_name)),
        },
        { label: "", onClick: () => {}, divider: true },
        {
          label: pkg.is_favorite ? "Unfavorite" : "Mark as favorite",
          onClick: () => onToggleFavorite(pkg.id, pkg.is_favorite),
        },
        {
          label: pkg.is_hidden ? "Unhide" : "Hide from grid",
          onClick: () => onToggleHidden(pkg.id, pkg.is_hidden),
          destructive: !pkg.is_hidden,
        },
      ];

  const errorStyle = pkg.error
    ? {
        background: "#3a1818",
        color: "#ff8888",
        padding: 8,
        overflow: "hidden",
        fontSize: 10,
        textAlign: "left" as const,
        alignItems: "flex-start" as const,
        justifyContent: "flex-start" as const,
      }
    : undefined;

  const onClick = (e: React.MouseEvent) => {
    if (pkg.error) return;
    // Don't open detail when the click is on an action button (favorite/hide).
    if ((e.target as HTMLElement).closest("button")) return;

    const ctrlOrMeta = e.ctrlKey || e.metaKey;
    const shift = e.shiftKey;

    // Modifier-driven selection works in any mode. Shift requests a range
    // fill from the last clicked anchor (App-level state).
    if (ctrlOrMeta || shift) {
      e.preventDefault();
      onToggleSelect(pkg.id, true, shift);
      return;
    }

    // Plain click in select mode: toggle this one, keep prior selections.
    if (selectionMode) {
      onToggleSelect(pkg.id, true, false);
      return;
    }

    // Default: open detail.
    onOpenDetail(pkg.id);
  };

  const tileClass = [
    "tile",
    isHubGhost ? "tile-hub-ghost" : "",
    isSelected ? "tile-selected" : "",
    selectionMode ? "tile-select-mode" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      className={tileClass}
      title={fullTitle}
      onClick={onClick}
      onContextMenu={onContextMenu}
    >
      {(selectionMode || isSelected) && (
        <div
          className={`tile-select-mark ${isSelected ? "tile-select-mark-on" : ""}`}
          aria-label={isSelected ? "Selected" : "Not selected"}
        >
          {isSelected ? "✓" : ""}
        </div>
      )}
      {menuPos && (
        <ContextMenu
          x={menuPos.x}
          y={menuPos.y}
          items={menuItems}
          onClose={() => setMenuPos(null)}
        />
      )}
      <div
        className="tile-thumb"
        style={{ height: tileSize, width: tileSize, ...errorStyle }}
      >
        {!pkg.error && pkg.has_preview && (
          <img
            className="tile-img"
            src={thumbUrl(pkg.id, thumbVersion)}
            alt=""
            onError={() => setThumbFailed(true)}
            style={{ opacity: thumbFailed ? 0 : 1 }}
          />
        )}
        {!pkg.error && (
          <span className="tile-type-icon" title={`Type: ${pkg.package_type}`}>
            {TYPE_EMOJI[pkg.package_type] ?? "❓"}
          </span>
        )}
        {!pkg.error && (
          <div className="tile-actions">
            <button
              type="button"
              className={`tile-action ${pkg.is_favorite ? "active" : ""}`}
              onClick={(e) => {
                e.stopPropagation();
                onToggleFavorite(pkg.id, pkg.is_favorite);
              }}
              title={pkg.is_favorite ? "Unfavorite" : "Favorite"}
            >
              {pkg.is_favorite ? "★" : "☆"}
            </button>
            <button
              type="button"
              className={`tile-action ${pkg.is_hidden ? "active" : ""}`}
              onClick={(e) => {
                e.stopPropagation();
                onToggleHidden(pkg.id, pkg.is_hidden);
              }}
              title={pkg.is_hidden ? "Unhide" : "Hide"}
            >
              {pkg.is_hidden ? "🙈" : "👁"}
            </button>
          </div>
        )}
        {pkg.error
          ? pkg.error.length > 200 ? pkg.error.slice(0, 200) + "…" : pkg.error
          : pkg.has_preview && thumbFailed
            ? "no thumb yet"
            : !pkg.has_preview
              ? "no preview"
              : null}
        {!pkg.error && items.length > 0 && (
          <div className="tile-cat-strip" title={`Contains: ${itemsTooltip}`}>
            {items.map((i) => (
              <span key={i.label} className="tile-cat-chip">
                <span className="tile-cat-emoji">{i.emoji}</span>
                <span className="tile-cat-n">{i.n}</span>
              </span>
            ))}
          </div>
        )}
      </div>
      <div className="tile-meta">
        <div className="tile-header">
          <span className="tile-creator">{pkg.creator || "(no creator)"}</span>
          {pkg.package_mtime > 0 && (
            <span
              className="tile-date"
              title={`Packaged ${new Date(pkg.package_mtime * 1000).toLocaleString()}`}
            >
              {formatShortDate(pkg.package_mtime)}
            </span>
          )}
        </div>
        <div className="tile-name">{displayTitle}</div>
        <div className="tile-row">
          <span className={`tile-type-badge ${useHub ? "tile-type-badge-hub" : ""}`}>{displayBadge}</span>
          {isPaid && (
            <span
              className={`tile-paid-badge ${isOffsite ? "tile-paid-offsite" : ""}`}
              title={isOffsite ? "Paid (off-hub)" : `Paid: ${pkg.hub_billing_tier}`}
            >
              {isOffsite ? "$ offsite" : "$"}
            </span>
          )}
          <span>{formatSize(pkg.file_size)}</span>
        </div>
      </div>
    </div>
  );
}
