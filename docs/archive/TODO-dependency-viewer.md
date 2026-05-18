# Dependency viewer + reverse-dep — TODO for a future session

## Goal

Build a fast, browsable view of the dependency graph between locally-installed
.var packages, with both directions:

- **Forward**: what does this package depend on?
- **Reverse**: which other packages depend on this one? (the "where is this
  look used as part of a scene" question)
- Eventually: visualize complex tangles (dependency clusters, missing deps,
  version drift) and let the user navigate through the relationships.

This is a separate milestone from the LLM tagging pipeline that already
shipped; it works on data the scanner already extracts.

## What already exists

- `meta.json` carries a `dependencies` object keyed by
  `"<Author>.<Package>.<Version>"` (or `".latest"`). See [src-tauri/src/meta.rs](src-tauri/src/meta.rs).
- The scanner ([src-tauri/src/scanner.rs](src-tauri/src/scanner.rs)) parses these and writes them to a
  normalized table:
  ```
  package_dependencies (
      package_id INTEGER NOT NULL,
      dep_key    TEXT NOT NULL,           -- e.g. "AcidBubbles.Timeline.289"
      PRIMARY KEY (package_id, dep_key)
  )
  ```
  Set in v1 of the schema; see [src-tauri/src/index.rs](src-tauri/src/index.rs).
- `get_package_detail` ([src-tauri/src/commands.rs](src-tauri/src/commands.rs)) currently returns the top-level
  dependency keys when the detail view opens a package. No recursive
  resolution, no reverse lookups.
- A separate `packages` table holds local rows with `creator`, `package_name`,
  `version` parsed from the filename. That's the join target.

The data is there. What's missing is a real graph layer and a UI that
exposes it.

## Open design questions to ask the user

1. **Version-resolution policy.** Dep keys can be exact (`Author.Pkg.3`) or
   floating (`Author.Pkg.latest`). When the local library has versions 2, 3
   and 5 of the same package, which one does "latest" resolve to? Highest
   number? Most-recently-scanned? Configurable? The naive "highest semver
   integer" probably works.
2. **What counts as a transitive resolution?** Walk the graph depth-first
   until missing, or cap at depth N? For pathological cases (cycles —
   shouldn't happen in well-formed VaM packages but I wouldn't bet on it),
   need cycle detection.
3. **Reverse-dep cardinality.** Some core utilities (Timeline, Embody,
   Glance) are referenced by hundreds of packages. The reverse-dep view for
   those will be huge — need pagination/filtering, or special "popular dep"
   handling.
4. **UI surface.** Inline in the existing detail view, or a dedicated
   dependency-graph page? The user has explicitly mentioned wanting to
   "untangle complex relationships" which suggests at least a list view, but
   a force-directed graph viz could be the eventual goal.
5. **Missing-dep handling.** When `dep_key` doesn't resolve to any local
   row, that's a gap (user doesn't have the package installed). Should the
   UI show that distinctly? Offer a "go fetch this" affordance? (probably
   beyond scope — just surface clearly that it's missing.)

## Suggested data-layer work

Resolved-dep table (one-time pass, re-runnable):

```sql
CREATE TABLE package_dep_links (
    src_package_id INTEGER NOT NULL,         -- depends on...
    dst_package_id INTEGER,                  -- this row (NULL if not local)
    raw_dep_key    TEXT NOT NULL,            -- original key for debugging
    PRIMARY KEY (src_package_id, raw_dep_key),
    FOREIGN KEY (src_package_id) REFERENCES packages(id) ON DELETE CASCADE,
    FOREIGN KEY (dst_package_id) REFERENCES packages(id) ON DELETE SET NULL
);
CREATE INDEX idx_dep_links_dst ON package_dep_links(dst_package_id);
```

Population pass:
1. For each `(package_id, dep_key)` in `package_dependencies`, parse the
   key into `(creator, package_name, version)`.
2. Look up the matching local `packages` row (handle `latest` resolution).
3. Insert into `package_dep_links` with `dst_package_id` set (or NULL if
   unresolved).

Re-run after every scan; cheap because `package_dependencies` is small.

Migration version to add this: v10 (v9 was the last LLM-tagging migration).

## Suggested UI work

- **Detail view sidebar**: small panel with "Depends on" (list of resolved
  package thumbnails + version) and "Used by" (reverse-dep list, paginated
  if >20). Each entry clickable to navigate.
- **Filter chips on the grid**: "has missing deps", "uses Timeline",
  "depends on X creator" — drives library hygiene workflows.
- **Optional graph view**: force-directed render of the dep cluster
  containing the selected package (1-2 hops out). Reasonable for ~100-node
  clusters; not for the whole library.

## Stretch: integrate with the LLM tagging pipeline

Once both systems are live, there's an interesting join:

- "Scene packages that depend on `physics-genital` -tagged plugins" → likely
  adult scenes.
- "Looks that depend on `gaze-eye-tracking` plugins" → likely demo scenes
  for character showcase.
- This is a downstream query, not a schema change — just SQL across
  `packages`, `package_tags`, and `package_dep_links`.

Not part of MVP; mention it as future value the dep layer unlocks.

## Definition of done

- v10 migration adds `package_dep_links` table.
- A resolver function populates it (callable from scanner end, or via a
  new `--resolve-deps` flag on the CLI / Tauri command).
- A new Tauri command `get_package_relationships(id)` returns
  `{ depends_on: [...], used_by: [...] }`.
- Detail-view sidebar shows both lists with clickable navigation.
- Missing deps surface visually (e.g. greyed entry with a "(not installed)"
  badge).
- Re-running the scan + resolver keeps the graph in sync as packages are
  added/removed.

## Pointers for the new session

- Read [CLAUDE.md](CLAUDE.md) for project conventions (PowerShell quirks,
  read-only invariant on `AddonPackages`, etc.).
- Existing scanner code: [src-tauri/src/scanner.rs](src-tauri/src/scanner.rs)
- Existing migrations: [src-tauri/src/index.rs](src-tauri/src/index.rs)
  (currently at v9 after the LLM tagging milestone)
- Existing detail-view command: [src-tauri/src/commands.rs](src-tauri/src/commands.rs) → `get_package_detail`
- Frontend grid + detail: [src](src) (React + TS)
- LLM tagging artifacts live under [tagging/](tagging/) — not directly
  relevant to dep work, but the `package_tags` / `taxonomy` / `tagging_runs`
  tables co-exist in the same SQLite DB and shouldn't be touched by this
  milestone.
