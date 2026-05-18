# Semantic search + similarity UI revival — TODO

The natural-language search bar ("Ask…") and the "Find similar to this"
affordance on the detail view were wired end-to-end, then deliberately
shelved. This doc consolidates the remaining work so a future session
can pick it up without spelunking the archived design docs.

Predecessors (now in [docs/archive/](docs/archive/)):
- `TODO-ui-tag-hookup.md` Phase 3 (semantic + "find similar" UI).
- `HANDOFF-embedding-to-ui.md` (backend handoff + UX findings).

## Status snapshot

| Layer | State |
|---|---|
| Embedding backend (`embedding/` module, `embed_library` CLI) | shipped |
| Migration v13 + `family_embeddings` populated (3706 × 4 variants) | shipped |
| Tauri commands `search_families`, `search_similar_families`, `get_packages_by_ids` | shipped |
| Frontend API wrappers in [src/lib/api.ts](src/lib/api.ts) | shipped |
| **Ask… search bar in main toolbar** | **shelved** (App.tsx) |
| **SimilarSection on detail view** | **shelved** (DetailView.tsx) |
| **Model warm-up at app launch** | **shelved** (lib.rs setup) |
| FTS5 lexical layer, query rewriting, score normalization | never built |

## Why shelved (not failed)

The semantic-search UI worked, but v4 tag quality was deemed too mixed
to ship behind a user-facing search bar — see the shelving notes in
[src/App.tsx](src/App.tsx) and [src/components/DetailView.tsx](src/components/DetailView.tsx).
The blocker is tag-data quality, not technical incompleteness.

**Don't unshelve until a v4 retag pass lands.** If you're picking this
up because a retag did land, verify by sampling family `purpose` text
against current tag assignments before re-enabling the UI.

## Step 1 — Unshelve the existing UI

Three shelved blocks (search for the word "shelved" to find them):

1. **[src/App.tsx](src/App.tsx) toolbar row + state** — restore the Ask…
   input, the semantic-search state, and the `visiblePackages` /
   `headerCount` branches the comment refers to. Original code is in
   git history at the shelving commit.

2. **[src/components/DetailView.tsx](src/components/DetailView.tsx)
   SimilarSection** — restore the section render block plus the
   `SimilarSection` component definition shelved at the bottom of the
   file.

3. **[src-tauri/src/lib.rs](src-tauri/src/lib.rs) `setup()`** —
   uncomment the `std::thread::spawn` warm-up. Without this the first
   query in any session appears to hang for 3-5 sec while fastembed
   lazy-loads the model.

Recommended call defaults (from earlier `--compare-search` testing):
`ModelChoice::NomicEmbedTextV15` + `InputKind::Purpose`. Best
recall on natural-language queries; ~15ms query encode after warm-up.

## Step 2 — UX hardening (new work, never addressed)

These came out of smoke-test findings and were not part of the original
shelved cut.

- **Result quality floor.** `search_text` returns top-N regardless of
  match quality. Either filter at render time below a cosine threshold
  (~0.5 is a rough vibe) or render an "uncertain match" affordance for
  low-score hits.
- **Score normalization.** Don't render raw scores in a way that
  invites cross-model comparison. BGE returns 0.6–0.85 in our corpus;
  nomic 0.55–0.75. If a relevance bar is shown, normalize within the
  result set (min/max).
- **Lexical-intent gap.** Queries like *"fix clothing tightness"*
  surface lexically-similar but semantically-wrong results (clothes
  whose `purpose` literally contains "tight" beat the actual
  sim-texture-paint plugin). Either ship the FTS5 hybrid (Step 3) or
  set user expectations in copy: *"similarity search, not intent
  matching."*

## Step 3 — Search quality, deferred

- **FTS5 hybrid.** SQLite FTS5 over `purpose` text, combined with
  cosine. Best path for queries where exact-match should dominate.
  Schema would need an FTS virtual table mirroring `package_family.purpose`.
- **Query rewriting / synonym layer.** Cheap mitigation for the
  lexical-intent gap. A static synonym map on the frontend before the
  call would help the obvious cases.
- **Re-embed automation on `purpose` edits.** Currently `--re-embed` is
  manual. After a retag pass that rewrites `purpose`, embeddings go
  stale silently. Either invalidate via a SQL trigger or scanner hook,
  or document the manual `embed_library --re-embed` step in the retag
  runbook.

## Variant defaults — already validated

The `family_embeddings` table holds four variants per family
(`model ∈ {bge, nomic}` × `input_kind ∈ {purpose, purpose-with-tags}`).
Switch variants per query by passing different `ModelChoice` /
`InputKind`; no re-encoding required.

| Use case | Variant |
|---|---|
| Default | `nomic / purpose` |
| Latency-sensitive fallback | `bge / purpose` (close on quality, ~3× faster) |
| Taxonomy-word queries | `* / purpose-with-tags` (helps when query mentions taxonomy concepts) |

Exposing a variant dropdown in an "advanced search" affordance is
optional; the four variants are all live and ready.

## Definition of done

- Ask… search bar renders in the main toolbar and returns relevant
  families for natural-language queries.
- "Find similar" affordance on the detail view returns relatives by
  family embedding (anchored by the current package's `family_id`).
- Cold-start first-query latency is <300 ms (warm-up at app launch
  amortizes the model load).
- Low-quality hits (cosine < threshold) are either hidden or visually
  distinguished from confident matches.
- No raw-score rendering that invites cross-model comparison.

## Pointers

- Project conventions: [CLAUDE.md](CLAUDE.md) (PowerShell quirks,
  MSVC linker via `scripts\dev-env.cmd`, read-only invariant on
  `AddonPackages`).
- Backend module: [src-tauri/src/embedding/](src-tauri/src/embedding/).
- Migration history: [src-tauri/src/index.rs](src-tauri/src/index.rs)
  (currently at v15; embeddings landed in v13).
- Shelving breadcrumbs: search `shelved` in [src/App.tsx](src/App.tsx),
  [src/components/DetailView.tsx](src/components/DetailView.tsx),
  [src-tauri/src/lib.rs](src-tauri/src/lib.rs).
- CLI for debugging from the shell:
  ```powershell
  src-tauri\target\debug\embed_library.exe --status
  src-tauri\target\debug\embed_library.exe --search "<query>" [--model bge|nomic] [--input purpose|purpose-with-tags] [--top-n N]
  src-tauri\target\debug\embed_library.exe --similar-to <family_id>
  src-tauri\target\debug\embed_library.exe --compare-search "<query>"
  ```

## What's explicitly NOT in scope

- Re-running the LLM tagging pipeline (`tag_library`). That's the
  *precondition* for unshelving — not part of this milestone.
- Re-running the embedding pass (`embed_library --embed-all`). Only
  needed if the retag pass rewrites `purpose` text; if so, run
  `--re-embed` for affected variants once before re-enabling the UI.
- Dependency-viewer work. That milestone shipped; see archived
  `TODO-dependency-viewer.md` for history.
