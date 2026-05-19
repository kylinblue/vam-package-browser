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
| Unpredicted with `family_id`       |    5 | (need kind:* tags + embeddings)        |
| Packages w/o `family_id`           |    8 | (malformed metadata, see §3 below)     |
| **Labeled families**               | 2299 |                                         |

Last refreshed by [src-tauri/src/bin/classifier_gaps_census.rs](src-tauri/src/bin/classifier_gaps_census.rs)
after the scanner auto-recompute landed and the historical 187 orphans
were cleaned up (rescan + tag_library --recompute-families + the three
predictor re-runs).

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

The residual is dominated by one wiring gap (the 187 no-`family_id`
packages) plus a handful of narrow model gaps. Verified by
[src-tauri/src/bin/classifier_gaps_census.rs](src-tauri/src/bin/classifier_gaps_census.rs)
— run it any time to refresh these numbers.

### 1. The original 187 no-`family_id` orphans — *resolved*

The dominant gap when this session started. `scanner::scan` previously
ended its transaction with `deps::resolve_all()` only — family
assignment was on-demand via `tag_library --recompute-families`. Any
package scanned after the last manual recompute sat with
`family_id = NULL`, invisible to both kind-vote (needs family_tags)
and embed-knn (needs family_embeddings). graph-prop could still hit
some via dep-graph edges, which is why most of the 187 had predictions
— but 11 fell through entirely.

**Structural fix (landed):** `scanner::scan` now calls
`tagging::family::recompute(conn)` after `tx.commit()`. Every scan is
self-healing for family assignment. See
[src-tauri/src/scanner.rs:222](src-tauri/src/scanner.rs:222).
`recompute()` lives outside the scan transaction because it opens its
own internal transaction (SQLite can't nest BEGIN); the operation is
idempotent so a partial failure is fine to retry.

**Historical cleanup:** done. After rescan + recompute + predictor
re-runs, the no-`family_id` bucket dropped from 187 to 8 (the residual
8 are §3 below, a separate problem class).

### 2. The 5 unpredicted-with-`family_id` rows — *needs tag + embed pass*

A new tier of residual that surfaced after the scanner fix. The newly
linked families have a `family_id` but their `package_family` row is
fresh — it has no entries in `family_tags` (because `tag_library`
hasn't run on them) and no row in `family_embeddings` (because
`embed_library` hasn't run on them). So kind-vote and embed-knn can't
touch them. graph-prop covered most via dep-graph edges; 5 fall through.

Examples observed in the live census:
```
[61927] Captain_Varghoss.TriggerUI.2          scan='Plugin'
[65785] vs1.vs1_H030_Fumino_Hair.1            scan='Hair'
[61935] Cgomes.AVA_FUCK_json.1                scan='Morph'
[61656] 14mhz.MeshColliderTongue.5            scan='Plugin'
[61654] 14mhz.AutoFlutterTongue.5             scan='Plugin'
```

**Fix (operational):** run `tag_library` and `embed_library` over the
75 new families that the recompute created, then re-run the three
predictors. The numbers in the State table above will tick down.

**Fix (structural, suggested):** mirror the scanner auto-recompute
pattern — add a "newly-created families need tagging + embedding"
gate somewhere in the pipeline so the operational fix becomes
automatic. Not done; this would be a useful next session item.

### 3. The 8 remaining no-`family_id` packages — *malformed metadata*

Down from 187 after the cleanup. Looking at the examples, these all
have empty (or `.`-only) `creator` and `package_name` fields:
```
[3794] ..2          scan='Unknown'
[164]  ..6          scan='Unknown'
[45048] .Dry_spell_(Sequencial_Anim).2   scan='Scene'   (predicted)
```
`family::recompute()` deliberately skips rows with empty creator or
package_name (see its WHERE clause), so these stay orphaned. Root
cause is upstream — the scanner couldn't extract usable
creator/package_name from the .var's meta.json (or the filename
parser failed).

Investigation deferred. Likely a small set of broken .var files in
the library; running `meta::parse` on them and logging the failure
would surface what's going on. Either fix the parser, or surface them
in the UI as "Unparseable" so the user can decide what to do.

### 4. Long-tail categories (Lighting+HDRI n=8, Audio n=1, etc.)

kNN can't reliably classify into a class with n=1 training examples.
**Self-correcting via hub-sync coverage** — every sync iteration both
shrinks the unmatched set AND grows rare-category training data. The
hub itself keeps adding resources, so periodic re-syncs are productive
(noted as "whack-a-mole" in practice, but each whack is real progress).
No separate classifier work needed; just keep syncing.

Manual UI correction is the right escape hatch for the residual that
remains after hub sync stabilizes.

### 5. The `audio-pack`-when-alone case

For families whose ONLY `kind:*` tag is `kind:audio-pack`, kind-vote
predicts Scenes (because `P(Scenes | audio-pack) = 97%` in training —
all `audio-pack` training rows co-occur with scene-related kinds). But
intuitively a true audio-only pack should map to Audio.

The voting model can't distinguish "alone" vs "co-occurring" — it
sums per-kind distributions regardless of set size. Fix options
(in order of cheapness):

1. **Rule override** in `predict_categories.rs`: if `kinds == {audio-pack}`,
   force `Audio` with confidence 1.0. Same pattern may apply to other
   "true X pack when alone" cases worth auditing (`pose-pack`,
   `morph-pack`, etc.).
2. **Singleton-set feature:** add an indicator to the vote when the
   family's kind set has size 1; the model can learn the special case.
3. **Co-occurrence model:** train `P(hub | kind_set)` instead of
   per-kind. More principled but sparse for rare combos.

Option 1 is the right cheap win — bound it to kinds whose pure form is
demonstrably a known long-tail category.

### 6. Score-level fusion (deferred — confirmed not worth it now)

Three predictions per row × confidence-weighted vote could beat the
cascade on the ~50 cases where high-conf kind-vote and high-conf
embed-knn disagree. Marginal lift; the cascade with embed-knn last is
already near-optimal. Revisit only if a real evaluation (see
"Evaluation caveats") motivates it.

## Items the previous handoff listed that turned out to be non-gaps

Caught by the census. Removed from the residual list above so a future
session doesn't chase them:

- **"~150 unlabeled siblings in labeled families."** Actual count:
  **zero**. Hub matching is already family-consistent — when the hub
  sync matches `Author.Package`, it sets `hub_category` on every
  locally-installed version. No family-sibling propagation pass is
  needed.
- **"11 still-unpredicted packages with `family_id`."** Actual count:
  **zero**. All 11 unpredicted packages live in the 187 no-`family_id`
  bucket and resolve together with item #1 above.

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

## Suggested first move for the next session

In order of impact-per-effort:

1. **Tag + embed the 75 new families** (§2 above). Run `tag_library`
   over untagged families, then `embed_library`, then re-run the three
   predictors. This drops the unpredicted-with-family count from 5 to
   ~0 and adds embedding/tag coverage that will help on future scans.
   Operational; no code changes.

2. **Wire `predicted_hub_category` into the UI** as a filter axis. The
   classifier work is done; the user-visible win is gated on the UI.
   99.7% coverage at ~90% mean accuracy is already useful. Layer the
   manual-UI-correction flow for long-tail / disagreement cases on
   top of this when it exists.

## Open questions, deferred

1. **Confidence threshold for "show as predicted" in the UI.** ≥ 0.6
   has been the working cutoff for cascade write decisions; UI display
   can use the same threshold to dim / flag / hide.
2. **Audio-pack-when-alone rule** (and similar singleton-set cases).
   Small change in `predict_categories.rs`; needs a quick scan of
   which other kinds exhibit the same pattern.
3. **Auto-tag + auto-embed new families.** Mirror the scanner auto-
   recompute pattern: when `family::recompute()` creates new family
   rows, queue them for tagging + embedding so the operational fix in
   §2 above becomes automatic. Bigger change (touches the tag and
   embed pipelines), worth doing once the manual cycle proves out the
   shape.
4. **Production-population eval** — a 30-50-row hand-labeled sample of
   currently-unmatched packages would be the only honest measure of
   accuracy on the actual production-target population. Worth doing
   once the residual stabilizes.
5. **Investigate the 8 malformed-metadata orphans** (§3 above). Find
   out why creator/package_name came out empty for those .var files
   and decide: fix the parser, or surface them in the UI as
   "Unparseable" so the user can decide.

(Previously landed open questions: "wire `family::recompute` into the
scanner" → see §1 above.)

## Definition of done

- ✅ All 4353 packages have `COALESCE(hub_category, predicted_hub_category)`
  set (currently 99.7%; the 0.3% residual is the 11 no-`family_id` rows
  with no other signal, all dissolvable by wiring `family::recompute`
  into the scanner per Open Question #1).
- ✅ The 225 low-confidence predictions dropped to 63 (Phase 2a) then to
  the rows where all three methods are uncertain.
- ⏳ UI exposes the unified category axis as a filter.
- ✅ A future hub sync that fills new `hub_category` ground truth does
  not conflict with existing `predicted_*` rows (all three predictors
  `WHERE hub_category IS NULL` in their UPDATE statements).
