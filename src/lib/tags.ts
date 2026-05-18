// Tag-rendering helpers. Used to be a mock-data source; the backend now
// returns real family tags via `PackageRow.tags`, so this file only carries
// the static UI-side metadata (namespace colors, descriptions) plus pure
// helpers for parsing and grouping.
//
// Namespace descriptions come from tagging/taxonomy-v4.json. The backend
// `list_namespaces` command also returns descriptions on the active rows,
// but for chip rendering we want a stable client-side palette without an
// extra round-trip just to color a chip. The set of namespaces below mirrors
// what's currently active in v4; new namespaces fall back to a neutral color.

export interface NamespaceMeta {
  namespace: string;
  description: string;
  /** Used for chip border / tint and the namespace label color. */
  color: string;
}

const NS_META_LIST: NamespaceMeta[] = [
  { namespace: "kind",            color: "#7aa2ff", description: "Primary identity of the package." },
  { namespace: "function",        color: "#8fc8ff", description: "What a utility plugin does." },
  { namespace: "content",         color: "#ff9090", description: "Content rating for act scenes." },
  { namespace: "count",           color: "#a0d8a0", description: "Number of participants." },
  { namespace: "setting",         color: "#b0c4de", description: "Where the scene is set." },
  { namespace: "activity",        color: "#ffb380", description: "What's happening." },
  { namespace: "style",           color: "#d088e0", description: "Art style." },
  { namespace: "theme",           color: "#88e0c0", description: "Era, world, or persona theme." },
  { namespace: "aesthetic",       color: "#e0b888", description: "Descriptive tone." },
  { namespace: "body",            color: "#e0a878", description: "Body type of the depicted character." },
  { namespace: "age-appearance",  color: "#e08888", description: "Apparent age range." },
  { namespace: "ethnicity",       color: "#d8c078", description: "Apparent ethnicity." },
  { namespace: "hair-color",      color: "#e8b070", description: "Hair color(s)." },
  { namespace: "hair-length",     color: "#d8a060", description: "Hair length(s)." },
  { namespace: "hair-style",      color: "#c89050", description: "Hair styling." },
  { namespace: "hair-region",     color: "#b88040", description: "Hair body region." },
  { namespace: "material",        color: "#a8b8c8", description: "Material of clothing / texture." },
  { namespace: "clothing-region", color: "#98a8b8", description: "Body region the clothing covers." },
  { namespace: "asset-type",      color: "#909090", description: "Support-asset subtype." },
  { namespace: "audio-type",      color: "#a0e0d0", description: "Audio pack subtype." },
  { namespace: "morph-region",    color: "#c0a0c0", description: "Morph body region." },
];

const NS_META: Map<string, NamespaceMeta> = new Map(
  NS_META_LIST.map((m) => [m.namespace, m]),
);

/** Look up the registered NamespaceMeta. Falls back to a neutral entry for
 *  unknown names so consumers can still render. */
export function getNamespaceMeta(namespace: string): NamespaceMeta {
  return (
    NS_META.get(namespace) ?? {
      namespace,
      color: "#888",
      description: "",
    }
  );
}

/** Parse a namespaced tag into its parts. Returns null on malformed input
 *  (no colon). Tags always have exactly one colon. */
export function parseTag(tag: string): { namespace: string; value: string } | null {
  const idx = tag.indexOf(":");
  if (idx < 0) return null;
  return { namespace: tag.slice(0, idx), value: tag.slice(idx + 1) };
}

/** Group a tag list by namespace, preserving the declared NS_META_LIST order
 *  (kind first), so rendering is stable. Unknown namespaces come last. */
export function groupTagsByNamespace(
  tags: string[],
): Array<{ ns: NamespaceMeta; values: string[] }> {
  const map = new Map<string, string[]>();
  for (const t of tags) {
    const parsed = parseTag(t);
    if (!parsed) continue;
    if (!map.has(parsed.namespace)) map.set(parsed.namespace, []);
    map.get(parsed.namespace)!.push(parsed.value);
  }
  const ordered: Array<{ ns: NamespaceMeta; values: string[] }> = [];
  for (const ns of NS_META_LIST) {
    const vs = map.get(ns.namespace);
    if (vs && vs.length > 0) ordered.push({ ns, values: vs });
  }
  for (const [name, vs] of map) {
    if (!NS_META.has(name)) {
      ordered.push({ ns: getNamespaceMeta(name), values: vs });
    }
  }
  return ordered;
}
