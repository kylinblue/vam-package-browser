import { useEffect, useMemo, useState } from "react";
import {
  listNamespaces,
  listTagCounts,
  type Namespace,
  type TagCount,
} from "../lib/api";
import { getNamespaceMeta, parseTag } from "../lib/tags";

interface Props {
  /** Currently selected tag set (full `namespace:value` strings). */
  selectedTags: string[];
  onChange: (next: string[]) => void;
}

/** Per-namespace value cap before the "show more" affordance kicks in. The
 *  full v4 setting namespace has ~37 values; bedrooms / bathrooms dominate
 *  the head, so showing 8 by default surfaces what most users want without
 *  burying the panel under a wall of low-count chips. */
const HEAD_CHIPS_PER_NS = 8;

/// FacetPanel — side panel listing every active v4 namespace with chips for
/// its tag values + family counts. Multi-select chips drive `selectedTags`.
///
/// Loading model:
/// - `list_namespaces` fires once on mount (cheap; ~21 rows).
/// - `list_tag_counts` (no namespace filter) fires once on mount; we group
///   client-side rather than running 21 separate queries. ~280 rows is small.
///
/// Selection semantics (matches QueryFilter.tags on the backend):
/// - Within a namespace, multiple chips = OR.
/// - Across namespaces, AND.
/// The chip counts are *absolute* (how many families have that tag), not
/// "and how many also match the current filter". A future refinement can
/// thread the current selection into a re-query for contextual counts.
export function FacetPanel({ selectedTags, onChange }: Props) {
  const [namespaces, setNamespaces] = useState<Namespace[]>([]);
  const [tagCounts, setTagCounts] = useState<TagCount[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [showAll, setShowAll] = useState<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [ns, tc] = await Promise.all([listNamespaces(), listTagCounts()]);
        if (cancelled) return;
        setNamespaces(ns);
        setTagCounts(tc);
      } catch (e) {
        if (!cancelled) setError(String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Group tag counts by namespace, in the order returned by `list_namespaces`
  // (sorted by family_count desc). Within each namespace, chips are sorted
  // by count desc — matching what `list_tag_counts` returns.
  const byNs = useMemo(() => {
    const m = new Map<string, TagCount[]>();
    for (const tc of tagCounts) {
      const parsed = parseTag(tc.tag);
      if (!parsed) continue;
      if (!m.has(parsed.namespace)) m.set(parsed.namespace, []);
      m.get(parsed.namespace)!.push(tc);
    }
    return m;
  }, [tagCounts]);

  const selectedSet = useMemo(() => new Set(selectedTags), [selectedTags]);

  const toggleTag = (tag: string) => {
    if (selectedSet.has(tag)) {
      onChange(selectedTags.filter((t) => t !== tag));
    } else {
      onChange([...selectedTags, tag]);
    }
  };

  const toggleCollapsed = (ns: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(ns)) next.delete(ns);
      else next.add(ns);
      return next;
    });
  };

  const toggleShowAll = (ns: string) => {
    setShowAll((prev) => {
      const next = new Set(prev);
      if (next.has(ns)) next.delete(ns);
      else next.add(ns);
      return next;
    });
  };

  const clearAll = () => onChange([]);

  if (error) {
    return <div className="facet-panel facet-error">facet load error: {error}</div>;
  }

  return (
    <aside className="facet-panel">
      <div className="facet-header">
        <span className="facet-title">Facets</span>
        {selectedTags.length > 0 && (
          <button
            type="button"
            className="facet-clear"
            onClick={clearAll}
            title="Clear all selected tags"
          >
            clear ({selectedTags.length})
          </button>
        )}
      </div>

      {namespaces.length === 0 && tagCounts.length === 0 && !error && (
        <div className="facet-loading">Loading facets…</div>
      )}

      {namespaces.map((ns) => {
        const meta = getNamespaceMeta(ns.namespace);
        const values = byNs.get(ns.namespace) ?? [];
        if (values.length === 0) return null;
        const isCollapsed = collapsed.has(ns.namespace);
        const isShowAll = showAll.has(ns.namespace);
        const visible = isShowAll ? values : values.slice(0, HEAD_CHIPS_PER_NS);
        const hiddenCount = values.length - visible.length;
        const selectedHere = values.filter((v) => selectedSet.has(v.tag)).length;
        return (
          <div
            key={ns.namespace}
            className="facet-section"
            style={{ "--ns-color": meta.color } as React.CSSProperties}
          >
            <button
              type="button"
              className="facet-section-header"
              onClick={() => toggleCollapsed(ns.namespace)}
              title={meta.description || ns.namespace}
            >
              <span className="facet-section-caret">{isCollapsed ? "▸" : "▾"}</span>
              <span className="facet-section-name">{ns.namespace}</span>
              {selectedHere > 0 && (
                <span className="facet-section-badge">{selectedHere}</span>
              )}
              <span className="facet-section-meta">{ns.family_count}</span>
            </button>
            {!isCollapsed && (
              <div className="facet-section-body">
                {visible.map((tc) => {
                  const parsed = parseTag(tc.tag);
                  const valueLabel = parsed ? parsed.value : tc.tag;
                  const selected = selectedSet.has(tc.tag);
                  return (
                    <button
                      key={tc.tag}
                      type="button"
                      className={`facet-chip ${selected ? "selected" : ""}`}
                      onClick={() => toggleTag(tc.tag)}
                      title={tc.tag}
                    >
                      <span className="facet-chip-value">{valueLabel}</span>
                      <span className="facet-chip-count">{tc.count}</span>
                    </button>
                  );
                })}
                {hiddenCount > 0 && (
                  <button
                    type="button"
                    className="facet-chip facet-chip-more"
                    onClick={() => toggleShowAll(ns.namespace)}
                  >
                    +{hiddenCount} more
                  </button>
                )}
                {isShowAll && values.length > HEAD_CHIPS_PER_NS && (
                  <button
                    type="button"
                    className="facet-chip facet-chip-more"
                    onClick={() => toggleShowAll(ns.namespace)}
                  >
                    show fewer
                  </button>
                )}
              </div>
            )}
          </div>
        );
      })}
    </aside>
  );
}
