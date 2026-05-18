import { groupTagsByNamespace, type NamespaceMeta } from "../lib/tags";

interface Props {
  tags: string[];
  /** Optional click handler — gets the full `namespace:value` string back.
   *  Phase 2 will use this to add the clicked tag to the facet filter. */
  onTagClick?: (tag: string) => void;
}

/** Render a tag list grouped by namespace, one row per namespace. The
 *  namespace name acts as a label on the left; values render as chips with
 *  a color tied to their namespace.
 *
 *  Mock data right now (see src/lib/mockTags.ts). The shape is stable, so
 *  swapping the data source will not require touching this component. */
export function TagChips({ tags, onTagClick }: Props) {
  const groups = groupTagsByNamespace(tags);
  if (groups.length === 0) return null;
  return (
    <div className="tag-chips">
      {groups.map(({ ns, values }) => (
        <TagRow key={ns.namespace} ns={ns} values={values} onTagClick={onTagClick} />
      ))}
    </div>
  );
}

function TagRow({
  ns,
  values,
  onTagClick,
}: {
  ns: NamespaceMeta;
  values: string[];
  onTagClick?: (tag: string) => void;
}) {
  return (
    <div className="tag-row" style={{ "--ns-color": ns.color } as React.CSSProperties}>
      <span className="tag-row-label" title={ns.description}>
        {ns.namespace}
      </span>
      <div className="tag-row-values">
        {values.map((v) => {
          const full = `${ns.namespace}:${v}`;
          const clickable = !!onTagClick;
          return (
            <button
              key={full}
              type="button"
              className={`tag-chip ${clickable ? "clickable" : ""}`}
              onClick={clickable ? () => onTagClick(full) : undefined}
              title={full}
              disabled={!clickable}
            >
              {v}
            </button>
          );
        })}
      </div>
    </div>
  );
}
