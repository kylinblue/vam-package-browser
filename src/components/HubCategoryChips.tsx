import type { HubCategoryCount } from "../lib/api";

/// Coarse emoji for each known hub category. Falls back to "📦" for any
/// category not in this map — categories occasionally evolve on the hub
/// (see Cowork recon 2026-05-17 for the canonical 19-entry list).
const HUB_CAT_EMOJI: Record<string, string> = {
  Scenes:                     "🎬",
  Looks:                      "🧍",
  Clothing:                   "👕",
  Hairstyles:                 "👩",
  Morphs:                     "💪",
  Poses:                      "🕺",
  "Mocap + Animation":        "🎞️",
  Textures:                   "🖼️",
  Environments:               "🏞️",
  "Lighting + HDRI":          "💡",
  "Assets + Accessories":     "📦",
  Audio:                      "🔊",
  "Plugins + Scripts":        "🔌",
  "Toolkits + Templates":     "🧰",
  "Comics + Storytelling":    "📖",
  "Voxta Content":            "💬",
  "Demo + Lite":              "🧪",
  Guides:                     "📚",
  Other:                      "❔",
};

interface Props {
  counts: HubCategoryCount[];
  selected: string | null;
  onSelect: (cat: string | null) => void;
  /** Optional count of packages with no hub_category (unmatched). When > 0
   *  we surface them as an "(unidentified)" chip the user can filter to. */
  unidentifiedCount?: number;
  onSelectUnidentified?: () => void;
  isUnidentifiedSelected?: boolean;
}

export function HubCategoryChips({
  counts,
  selected,
  onSelect,
  unidentifiedCount,
  onSelectUnidentified,
  isUnidentifiedSelected,
}: Props) {
  const total = counts.reduce((sum, c) => sum + c.count, 0);
  return (
    <div className="type-chips">
      <button
        type="button"
        className={`type-chip ${selected === null && !isUnidentifiedSelected ? "active" : ""}`}
        onClick={() => onSelect(null)}
        title="All hub-matched packages"
      >
        <span>All</span>
        <span className="type-chip-n">{total.toLocaleString()}</span>
      </button>
      {counts.map((c) => (
        <button
          type="button"
          key={c.hub_category}
          className={`type-chip ${selected === c.hub_category ? "active" : ""}`}
          onClick={() =>
            onSelect(selected === c.hub_category ? null : c.hub_category)
          }
          title={`Show only ${c.hub_category}`}
        >
          <span className="type-chip-emoji">
            {HUB_CAT_EMOJI[c.hub_category] ?? "📦"}
          </span>
          <span>{c.hub_category}</span>
          <span className="type-chip-n">{c.count.toLocaleString()}</span>
        </button>
      ))}
      {unidentifiedCount !== undefined && unidentifiedCount > 0 && onSelectUnidentified && (
        <button
          type="button"
          className={`type-chip type-chip-unidentified ${isUnidentifiedSelected ? "active" : ""}`}
          onClick={onSelectUnidentified}
          title="Packages without a hub match"
        >
          <span>(unidentified)</span>
          <span className="type-chip-n">{unidentifiedCount.toLocaleString()}</span>
        </button>
      )}
    </div>
  );
}
