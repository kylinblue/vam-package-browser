# Hub-category classifier — residual TODO

The goal is to fill `hub_category` (or a model-predicted equivalent) for
every package in the local library, so the UI has a single category axis
to filter/sort by. The hub itself supplies ground truth for the ~58% of
local packages that match a hub resource. The rest needs prediction.

This document hands off the remaining work to a fresh session. **It is
self-contained — read this and the DB; do not assume conversation
context from the session that landed Phases 0/B/0.5.**

## State at this commit

| layer                           | rows  | accuracy vs. hub_category |
| ------------------------------- | ----: | ------------------------- |
| Total packages                  |  4353 |                           |
| Have `hub_category` (truth)     |  2544 | 100%                      |
| Have `predicted_hub_category`   |  1707 | ~88% (5-fold CV)          |
| No prediction (no `kind:*` tag) |   102 | —                         |

Unified UI coverage `COALESCE(hub_category, predicted_hub_category)` =
4251/4353 = **97.7%**. Phase 0.5 covered the first 95% of the gap.

### What landed in Phases 0 / B / 0.5

1. **Phase 0** — established baseline. The scanner's contentList-prefix
   classification (`meta::classify` → `packages.package_type`) agrees
   with `hub_category` only **47%** of the time. Output kinds
   (Scene/Look/SubScene) are tiny in contentList vs. their constituent
   files, so dominance-by-count routes them wrong. The deeper finding:
   **contentList prefixes cannot distinguish hub-Look from hub-Scene
   structurally** — 96% of hub-Looks have `scene_count > 0` because
   creators distribute Looks as showcase scenes. This is a signal
   limitation, not a code bug.

2. **Phase B** — narrow scanner patch:
   [src-tauri/src/meta.rs](src-tauri/src/meta.rs) — when a scene file
   is present, suppress `Sound` from the dominance contest. Targets the
   109 hub-Scenes that contentList prefix logic was routing to Sound
   because of bundled audio. Net accuracy impact: **+0.1pp** (essentially
   a no-op). Kept anyway because it removes a wrong label without
   inventing a worse one. Backfilled via
   [src-tauri/src/bin/reclassify_sound.rs](src-tauri/src/bin/reclassify_sound.rs)
   on 165 affected rows.

3. **Phase 0.5** — kind-vote predictor. Trained on the 2544 labeled rows:
   for each `kind:*` tag in a package's family, learn
   P(hub_category | kind). Sum distributions across the row's kind set;
   argmax wins; confidence = winning_score / sum_of_scores. Schema
   migration **v16** added `predicted_hub_category`, `predicted_method`,
   `predicted_confidence` (see
   [src-tauri/src/index.rs](src-tauri/src/index.rs)). Predictor binary:
   [src-tauri/src/bin/predict_categories.rs](src-tauri/src/bin/predict_categories.rs)
   (writes `predicted_method='kind-vote'`).

## What's not solved

### 1. The 102 no-kind packages

These rows have no family in `family_tags` with a `kind:*` tag — either
the family was never tagged by v4, or the LLM returned no `kind:*`
namespace. The scanner's `package_type` for these is mostly
Clothing/Asset/Hair/Mixed (rule-based fallback).

Options:
- Run a v4 tagger pass over the missing families (existing
  infrastructure at `src-tauri/src/tagging/`,
  [src-tauri/src/bin/tag_library.rs](src-tauri/src/bin/tag_library.rs)).
- Or accept these 2.3% as residual and predict them via Phase 2a/2b/4.

### 2. The 225 low-confidence predictions (confidence < 0.6)

The dominant ambiguity is **`kind:morph-pack`**: the training data shows
P(Morphs | morph-pack) = 47%, P(Looks | morph-pack) = 39%. A package
tagged only `morph-pack` gets predicted Morphs by a hair, but ~40% of
such rows are actually hub-Looks (looks distributed via morph packs,
e.g. LDR's lean character packages).

The voting model has no way to disambiguate because it sees one
tag → one weighted vote. Co-occurrence patterns
(`{morph-pack, character-look}` → Look, `{morph-pack}` alone → Morphs)
need a richer model. This is exactly what Phase 2a (dep-graph) and
Phase 2b (embeddings) are for.

### 3. Long-tail hub categories never predicted

| hub_category         | hub labels | predicted (Phase 0.5) |
| -------------------- | ---------: | --------------------: |
| Lighting + HDRI      |          8 |                     0 |
| Toolkits + Templates |          6 |                     0 |
| Audio                |          1 |                     0 |
| Guides               |          1 |                     0 |

No clean `kind:*` value maps to these. They're rare enough that manual
UI correction (planned) probably handles them better than another model
pass. Leave alone unless Phase 4 (LLM) cheaply covers them.

### 4. The `audio-pack` mapping is counterintuitive

97% of training rows with `kind:audio-pack` map to **Scenes**, not Audio
— because the v4 LLM tags scene packages that include audio files with
`audio-pack` as a secondary kind. Not a bug in the voting model
(it's correctly learning the data), but worth being aware of when
inspecting predictions or designing Phase 2b text features.

## Planned phases

### Phase 2a — dependency-graph propagation

Semi-supervised label propagation on the package dependency graph.

**Concept**: a package's category is informed by its neighborhood. A
package whose deps are mostly Hair/Clothing/Morphs is structurally a
consumer (often a Look or Scene). A package depended on only by Scenes
is structurally a provider (often a Look or Asset).

**Signals**:
- Forward (what I depend on): distribution of dep categories.
  `frac_deps_hair`, `frac_deps_morph`, etc. as features.
- Reverse (what depends on me): distribution of consumer categories.

**Implementation sketch**:
- `package_dep_links` table already holds resolved forward/reverse
  edges (see migration v9 in `index.rs`).
- For each unlabeled or low-confidence node, gather one-hop
  hub_category counts from labeled neighbors.
- Iterative damped propagation (each hop weighted 0.5×). 3–5 passes,
  stop when label changes plateau.
- Combine with Phase 0.5 vote: a weighted sum where dep-graph votes
  break ties on morph-pack ambiguity.

**Expected lift**: target the 225 low-confidence predictions, especially
the morph-pack slice. Looks bundled as morph packs usually depend on
hair/clothing/textures — easily separable from real morph packs which
are dep-light.

**Storage**: overwrite `predicted_hub_category`/`predicted_confidence`
on rows where the new score is higher. Update `predicted_method` to
`'graph-prop'` so we know what made the call.

### Phase 2b — text-embedding kNN

Orthogonal signal to the structural features used so far.

**Concept**: encode a per-package text blob (description, contentList
path summary, creator, filename) with the existing fastembed pipeline
(BGE-small-en-v1.5, 384-dim — see `src-tauri/src/embedding/`). For each
unlabeled or low-confidence row, kNN against the 2544 labeled embeddings,
majority vote of top-K weighted by cosine similarity.

**Implementation sketch**:
- Reuse the `family_embeddings` infrastructure (migration v13). May need
  per-package embeddings rather than per-family; check current schema.
- Description meaningfulness gate — many `description` fields are empty
  or templated ("see hub for details"). Skip those; fall back to
  filename + contentList summary alone.
- Combine with Phase 2a via score fusion.

**Expected lift**: complements Phase 2a. Strong on packages with
distinctive filenames or descriptions but ambiguous structurally.

**Storage**: `predicted_method='embed-knn'`.

### Phase 4 — LLM disambiguation on the residual

Whatever's still ambiguous after 2a + 2b (probably < 100 rows):

- Prompt: description + contentList tree + sibling-family
  `kind:*` set + current best prediction + confidence.
- One-shot, cached. Use an inexpensive model.
- `predicted_method='llm'`. Reserve `predicted_method='manual'` for the
  future UI correction workflow.

## Schema reference

```sql
-- Added in migration v16 (this commit).
ALTER TABLE packages ADD COLUMN predicted_hub_category TEXT;
ALTER TABLE packages ADD COLUMN predicted_method       TEXT;
ALTER TABLE packages ADD COLUMN predicted_confidence   REAL;
CREATE INDEX idx_packages_predicted_hub
    ON packages(predicted_hub_category) WHERE predicted_hub_category IS NOT NULL;
```

Producer-tag vocabulary for `predicted_method`:
- `'kind-vote'` — Phase 0.5 (this commit)
- `'graph-prop'` — Phase 2a (planned)
- `'embed-knn'` — Phase 2b (planned)
- `'llm'` — Phase 4 (planned)
- `'manual'` — reserved for the future UI correction workflow

Unified UI query: `COALESCE(hub_category, predicted_hub_category)`.

## Protocols to honor

Multi-session conventions documented in [CLAUDE.md](CLAUDE.md):

- **Lock file** at
  `%APPDATA%/com.github.kylinblue.vam-package-browser/.session-active.lock`
  before any DB write. See `predict_categories.rs` lines around the
  `SessionLock` struct for a reference RAII implementation.
- **Migration coordination** — announce before adding a new
  `migrate_vN_to_vN+1` to `index.rs`. v16 is the current head.
- **Dev server port** — only one `tauri dev` across all worktrees.

## Open questions for the next session

1. **Tagger re-run for the 102 no-kind families?** Cheap, predictable
   ~88% accuracy via Phase 0.5 once tagged. The alternative is letting
   Phase 2/4 cover them blind.
2. **Confidence threshold for "show as predicted" in the UI?** Anything
   ≥ 0.6 is probably fine; below that the UI might dim or flag.
3. **Should `predicted_hub_category` be exposed as a filter axis in the
   UI now, or wait until 2a/2b lift the residual accuracy further?**
   97.7% coverage at ~88% mean accuracy is already useful.

## Definition of done

- All 4353 packages have `COALESCE(hub_category, predicted_hub_category)`
  set (currently 97.7%).
- The 225 low-confidence predictions drop below ~50 (or the morph-pack
  ambiguity is resolved by a co-occurrence signal).
- UI exposes the unified category axis as a filter.
- A future hub sync that fills new `hub_category` ground truth does not
  conflict with existing `predicted_*` rows (verify the predictor
  binary correctly skips `WHERE hub_category IS NULL`).
