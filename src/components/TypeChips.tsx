import type { PackageType, TypeCount } from "../lib/api";

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
  counts: TypeCount[];
  selected: PackageType | null;
  onSelect: (t: PackageType | null) => void;
}

export function TypeChips({ counts, selected, onSelect }: Props) {
  const total = counts.reduce((sum, c) => sum + c.count, 0);
  return (
    <div className="type-chips">
      <button
        type="button"
        className={`type-chip ${selected === null ? "active" : ""}`}
        onClick={() => onSelect(null)}
        title="All package types"
      >
        <span>All</span>
        <span className="type-chip-n">{total.toLocaleString()}</span>
      </button>
      {counts.map((c) => (
        <button
          type="button"
          key={c.package_type}
          className={`type-chip ${selected === c.package_type ? "active" : ""}`}
          onClick={() =>
            onSelect(selected === c.package_type ? null : c.package_type)
          }
          title={`Show only ${c.package_type}`}
        >
          <span className="type-chip-emoji">
            {TYPE_EMOJI[c.package_type] ?? "❓"}
          </span>
          <span>{c.package_type}</span>
          <span className="type-chip-n">{c.count.toLocaleString()}</span>
        </button>
      ))}
    </div>
  );
}
