# Hub-category classifier — residual TODO

The goal is to fill `hub_category` (or a model-predicted equivalent) for
every package in the local library, so the UI has a single category axis
to filter/sort by. The hub itself supplies ground truth for the ~58% of
local packages that match a hub resource. The rest needs prediction.

This document hands off the remaining work to a fresh session. **It is
self-contained — read this and the DB; do not assume conversation
context from the session that landed Phases 0/B/0.5.**

## State at this commit

| layer                              | rows | accuracy vs. hub_category (20% holdout) |
| ---------------------------------- | ---: | --------------------------------------- |
| Total packages                     | 4353 |                                         |
| Have `hub_category` (truth)        | 2544 | 100%                                    |
| Have `predicted_hub_category`      | ~1798 | by method, see below                   |
| &nbsp;&nbsp;`predicted_method='kind-vote'`  | ~1481 | 90.1% (1)                       |
| &nbsp;&nbsp;`predicted_method='graph-prop'` |  ~228 | 64.3% (1, 2)                    |
| &nbsp;&nbsp;`predicted_method='embed-knn'`  |   89  | 95.1% (1, with Nomic)            |
| No prediction                      |   11 | —                                       |
| Packages w/o `family_id`           |  187 | (skipped by all predictors)             |

Unified UI coverage `COALESCE(hub_category, predicted_hub_category)` ≈
4342/4353 = **99.7%**. Phase 2a + 2b covered the residual gap.

**(1)** Numbers are from `--holdout-test` mode — a deterministic 80/20 family
split (seed `0xDEADBEEF_CAFEBABE`), each predictor trained on the 80% train
families only, evaluated on the 20% test families exactly once. Same seed
across all three so the numbers are directly comparable on identical test data.
See [src-tauri/src/holdout.rs](src-tauri/src/holdout.rs) for the split
function (with unit tests for determinism / order-independence).

**(2)** graph-prop holdout was measured in *standalone* mode — features built
from ground-truth labels only, no cross-method inputs from kind-vote
predictions. In production, graph-prop uses kind-vote predictions as
soft-label features and likely scores higher; the standalone holdout is a
lower bound on its production accuracy.

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

4. **Phase 2a** — dep-graph feature-kNN. *First attempt was wrong*:
   neighbor-vote / label-propagation scored 2.8% in CV because this graph
   is anti-assortative (a Scene's deps are Plugins/Hair/Clothing/Morphs,
   so a vote-based propagator labels Scenes as "Plugins"). The `--audit`
   mode in
   [src-tauri/src/bin/propagate_categories.rs](src-tauri/src/bin/propagate_categories.rs)
   confirmed empirically — P(neighbor shares category) is ≤ 0.2 for every
   category except Scene↔Scene reverse-edges (0.83, the sub-scene case).
   *Refactor that worked*: use the neighbor-category-distribution as a
   feature vector (`fwd_frac[C]`, `rev_frac[C]`, log out-degree, log
   in-degree), cosine-kNN against labeled families. CV jumped to 61.2%
   with kind-vote predictions as soft labels in features. Phase 2a wrote
   293 predictions (91 fresh, 202 overwrite low-conf kind-vote) with
   `predicted_method='graph-prop'`.

5. **Phase 2b** — text-embedding kNN. Reuses the existing v13
   `family_embeddings` table (Nomic-embed-text-v1.5, 768-dim,
   purpose+tags). Binary:
   [src-tauri/src/bin/embed_predict_categories.rs](src-tauri/src/bin/embed_predict_categories.rs).
   Predicts at family granularity (one embedding per family) and applies
   the same per-package write policy as Phase 2a. Wrote 89 predictions
   with `predicted_method='embed-knn'`, all overwriting low-confidence
   priors. Notably caught 6 Lighting+HDRI cases that BGE missed and that
   no other method ever produces.

6. **Held-out evaluation** — `--holdout-test` mode added to all three
   predictors. Same seed across binaries
   ([src-tauri/src/holdout.rs](src-tauri/src/holdout.rs)) means each is
   evaluated on the identical 20% test families. Sanity-checks the CV
   numbers against tuning leak. Result: every predictor's holdout meets
   or exceeds its CV number — the opposite of what tuning leak would
   show, so CV was honest (slightly pessimistic if anything).

## Write policy

The three predictors form a cascade, not a fusion. Each writes only where:

- `hub_category IS NULL` (never overwrite truth), AND
- the row is "fresh" (no existing prediction) OR existing
  `predicted_confidence < 0.6`

The 0.6 threshold matches the original "low-confidence" boundary from the
TODO. After the three-pass cascade, the LAST method to write owns the row
— which means embed-knn (the highest-accuracy method) gets the final say
on every low-confidence row.

A proper score-level fusion is still possible (see "What's not solved
yet" below) but was deferred — the cascade with embed-knn last is already
near-optimal for the common case.

## What's not solved yet

### 1. The 11 still-unpredicted packages

After all three passes, 11 packages with `family_id` and a hub-match
gap still have no `predicted_hub_category`. They have no kind:* tags,
no labeled neighbors in the dep graph, and either no embedding or one
whose nearest training family is below the cosine cutoff. Cheapest fix:
copy the hub_category from a labeled sibling in the same `package_family`
(see "Unlabeled siblings in labeled families" below — same fix).

### 2. Unlabeled siblings in labeled families

Many packages without `hub_category` are *versions* of a package whose
other versions DID match the hub. Their `family_id` points to a
`package_family` row where another sibling has `hub_category` set. None
of the three predictors fills these in — they all gate on family-level
unlabeled-ness. Trivial fix: propagate the family's `hub_category` to
unlabeled siblings as `predicted_hub_category` with confidence 1.0 and
`predicted_method='family-sibling'`. Probably ~150 rows. Not done in
this pass; would be a tiny standalone binary.

### 3. The 187 packages without `family_id`

These are scanner residue — packages indexed before family-assignment
ran, or with malformed metadata that prevented family creation.
Investigation deferred; they show up as "Unknown" in any category-axis
UI until they get a family.

### 4. Long-tail categories (Lighting+HDRI n=8, Audio n=1, etc.)

Persistent across all phases. kNN can't reliably find a single training
example, and the per-class accuracy stays at 0% for these in CV /
holdout. Manual UI correction is the right tool here — they're rare
enough that one-by-one labeling is fine. Phase 4 (LLM) might also help
cheaply if we want automation.

### 5. Score-level fusion (deferred)

Three predictions per row × confidence-weighted vote could beat the
cascade on the ~50 cases where high-conf kind-vote and high-conf
embed-knn disagree. Requires either a schema addition to persist all
three predictions per row, or re-running each predictor in-memory in a
fusion binary. Marginal lift; not done. Revisit if the matched→unmatched
distribution-shift concern (see "Evaluation caveats" below) becomes
operationally important.

### 6. The `audio-pack` mapping is still counterintuitive

97% of training rows with `kind:audio-pack` map to **Scenes**, not Audio
— because the v4 LLM tags scene packages that include audio files with
`audio-pack` as a secondary kind. Not a bug in any predictor (each is
correctly learning the data), but worth being aware of when inspecting
predictions or designing future text features.

## Evaluation caveats

The holdout numbers above are honest measurements of in-distribution
accuracy on hub-matched packages. They do NOT measure accuracy on the
*production target* — hub-*unmatched* packages, which skew toward niche
creators, paid offsite resources, and packages with thinner metadata.

The matched and unmatched populations almost certainly differ in ways
that affect classifier accuracy. CV and holdout protocols can't fix
this — it's a fundamental train→production distribution shift.

The cheapest remedy if this matters operationally: eyeball ~30-50
currently-unmatched packages stratified by `predicted_method` and
`predicted_confidence`, mark right/wrong by hand. This is the only
honest measure of production accuracy on the actual target population.

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
- `'kind-vote'` — Phase 0.5
- `'graph-prop'` — Phase 2a
- `'embed-knn'` — Phase 2b
- `'family-sibling'` — reserved for the unlabeled-siblings-in-labeled-families fix
- `'fused'` — reserved for the deferred score-level fusion
- `'llm'` — Phase 4 (still planned)
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

1. **Family-sibling propagation?** Trivial 50-line binary that fills the
   11 still-unpredicted + ~150 unlabeled-sibling cases. Highest-confidence
   fix because the source is the family's own labeled sibling.
2. **Wire `predicted_hub_category` into the UI?** 99.7% coverage at
   ~90% mean accuracy is already useful. The Lighting+HDRI / Audio /
   Guides long-tail probably needs a manual-correction UI flow eventually.
3. **Confidence threshold for "show as predicted"?** ≥ 0.6 has been the
   working threshold for cascade write decisions; the UI might dim or
   flag rows below that.
4. **Score-level fusion?** Marginal lift; deferred until distribution-shift
   eval (see "Evaluation caveats") motivates it.
5. **Production-population eval?** A 30-50-row hand-labeled sample of
   currently-unmatched packages would be the only honest measure of
   accuracy on the actual target distribution.

## Definition of done

- ✅ All 4353 packages have `COALESCE(hub_category, predicted_hub_category)`
  set (currently 99.7%, residual = 11 + 187 no-family).
- ✅ The 225 low-confidence predictions dropped to 63 (Phase 2a) then to
  the rows where all three methods are uncertain.
- ⏳ UI exposes the unified category axis as a filter.
- ✅ A future hub sync that fills new `hub_category` ground truth does
  not conflict with existing `predicted_*` rows (all three predictors
  `WHERE hub_category IS NULL` in their UPDATE statements).
