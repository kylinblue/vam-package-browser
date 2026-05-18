# UI hookup for v4 tags + facets — TODO for a future session

## Goal

Surface the v4 family-level tag layer (just shipped — ~11.4k tag
assignments across 3706 families) in the existing React grid. Users
should be able to:

- See the tags assigned to each package family in detail/hover.
- Filter the grid by tag namespace (e.g. show only `kind:utility-plugin`,
  or only families tagged `function:audio-management`).
- Facet across multiple namespaces simultaneously (e.g.
  `kind:character-look` + `body:curvy` + `style:realistic`).
- Eventually, "find similar to this" — once the embedding pipeline
  ships (see [TODO-embedding-pipeline.md](TODO-embedding-pipeline.md)).
- Natural-language search bar — also once embeddings are live.

This is where the LLM tagging investment becomes user-visible value.

## What already exists

### Data layer (ready to consume)

- `package_family` table — 3706 rows, every one tagged. Columns
  include `id`, `creator`, `package_name`, `latest_package_id`,
  `purpose`, `tagging_state`, `tagging_model`, `taxonomy_version`.
  See migrations v11 + v12 in [src-tauri/src/index.rs](src-tauri/src/index.rs).
- `family_tags` — normalized tag rows. ~11,400 rows. PRIMARY KEY
  `(family_id, tag)`. Tag is a full namespaced string (e.g.
  `"kind:character-look"`, `"function:physics-collider"`).
- `taxonomy` table — 280 active v4 tags across 21 namespaces.
  Carries `namespace`, `applies_to_json`, `cardinality`,
  `description`, `examples_json`. Filter with `WHERE is_active = 1`.
- `packages.family_id` — links each .var row to its family.
- `taxonomy-v4.json` ([tagging/taxonomy-v4.json](tagging/taxonomy-v4.json))
  documents the design and is the source of truth on namespace
  semantics + cardinality.

### Backend layer

- Tauri commands live in [src-tauri/src/commands.rs](src-tauri/src/commands.rs).
- `query_packages` returns flat `PackageRow` (legacy, package-level).
  Doesn't include tags. Needs an update or a sibling command.
- `list_creators_with_counts`, `list_type_counts` for filter chips.
- `get_package_detail` returns full meta for one package.

The likely shape needed for the new UI:

- `query_families(filter)` — returns `FamilyRow[]` with tags joined.
- `get_family_detail(family_id)` — returns family + all tags + the
  full latest-package detail.
- `list_tag_counts(namespace?)` — `(tag, count)` pairs, optionally
  filtered to a namespace. Drives the facet chips.
- `list_namespaces()` — `(namespace, applies_to, cardinality, description, value_count)`.
  Powers the facet-chip panel.

### Frontend layer

- React + TS + Vite. See [src/](src/).
- `PackageGrid` (virtualized thumbnail grid), `FilterBar`, etc.
- Existing filters: creator dropdown, package_type chips, favorites,
  hidden, size, mtime, search.
- Thumbnails served via a `thumb://` URI protocol from
  [src-tauri/src/lib.rs](src-tauri/src/lib.rs).

## Suggested approach (incremental, in 3 phases)

### Phase 1 — read-only tag display (smallest delta, highest value)

- Add `tags: string[]` to `PackageRow` (or introduce a parallel
  `FamilyRow`). Backend joins `family_tags` per row.
- Show tags as compact chips on the hover/detail view for each
  thumbnail. Group chips by namespace, e.g.:
  ```
  kind:        character-look
  body:        curvy
  style:       realistic
  theme:       modern
  age-appearance: young-adult
  ```
- No new filter UI yet. Just visibility.

This is the minimum that proves the data layer reaches the UI.

### Phase 2 — facet filters

- New `FacetPanel` component on the side or top of the grid.
- For each namespace, show its top-N values as togglable chips with
  counts (e.g. `kind:character-look (948)`, `kind:clothing-item (826)`).
- Multi-select within a namespace = OR (union). Across namespaces =
  AND (intersection). Matches the existing filter chip UX pattern.
- Query layer: backend's `query_families` accepts a `tags: string[]`
  filter representing AND-of-OR semantics.

### Phase 3 — search (depends on embedding milestone)

- Replace the simple substring search with a hybrid:
  - SQLite FTS5 over `purpose` text (exact-token, fast)
  - Cosine similarity over family embeddings (semantic, fuzzy)
- "Find similar to this" button on detail view → backend search by
  embedding.
- Natural-language query bar → embed query + return top-N.

## Open design questions to ask the user

1. **Grid unit**: should the grid now show one tile per **family**
   (current latest version) or stay one tile per **package** (every
   version)? Family-tile is cleaner now that everything is tagged at
   family level, but older versions still need to be discoverable
   somehow. Possible answer: family-tile by default, "show all
   versions" toggle expands.

2. **Namespace prominence**: the v4 taxonomy has 21 namespaces. Some
   (kind:, body:, theme:) are top-shelf for filtering; others
   (material:, hair-region:, asset-type:) are niche. Should the
   FacetPanel show all 21 by default with pinning, or show only the
   most-used N with an "expand" affordance?

3. **`kind:` chip behavior**: should `kind:` be a *primary* axis that
   reshapes the grid (e.g. "show me Character Looks" hides the
   Plugin facets), or just another facet like the others? The former
   matches the multi-kind data shape better but is a bigger UX
   change.

4. **`out_of_scope` column deprecation**: it's now always 0 (every
   family has a kind). Existing `QueryFilter` might still reference
   it. Worth a cleanup pass or leave for later.

5. **Color-coding tags by namespace**: cheap visual win. Each
   namespace prefix gets a distinct color/icon. Especially helpful
   when chips stack on a detail view. Worth doing? (My take: yes,
   small effort, big readability gain.)

## Schema reminders (no migration needed for this milestone)

The backend has everything Phase 1 + Phase 2 need. No SQL changes,
just new query commands.

Phase 3 will need the embedding pipeline (which is a separate
milestone) but doesn't need schema work either — see migrations v8 +
v12 for the columns already in place.

## Pointers for the new session

- **Project conventions**: [CLAUDE.md](CLAUDE.md) (PowerShell quirks,
  build via `scripts\dev-env.cmd`, etc.).
- **Existing Tauri commands**: [src-tauri/src/commands.rs](src-tauri/src/commands.rs)
  — query patterns to mirror.
- **Existing React grid**: [src/](src/) — `PackageGrid.tsx`,
  `FilterBar.tsx`, plus state in `src/lib/api.ts`.
- **Taxonomy source of truth**: [tagging/taxonomy-v4.json](tagging/taxonomy-v4.json)
  documents what each namespace means + applies_to + cardinality.
- **Live tagging code** (for understanding data shape):
  [src-tauri/src/tagging/](src-tauri/src/tagging/).
- **Don't run `tag_library`** unless re-tagging is explicitly
  requested — there's no need; tagging is complete and stable.

## Definition of done

Phase 1:
- Grid tiles or detail view show the tags for the package's family.
- Tags rendered with namespace grouping.
- No new filters; data visibility only.

Phase 2:
- Per-namespace facet panel with multi-select chips and counts.
- Filter applies in real time; grid updates.
- AND-of-OR semantics across namespaces / within-namespace.

Phase 3 (after embedding milestone):
- Natural-language search bar with embedding-backed results.
- "Similar to this" affordance on detail view.

## Important non-goals (don't expand scope)

- The dependency viewer is a separate milestone (see
  [TODO-dependency-viewer.md](TODO-dependency-viewer.md)). Don't
  conflate.
- The thumbnail pipeline is stable; don't touch it.
- Re-tagging is not in scope for this milestone.
