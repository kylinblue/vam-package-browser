# Hub-sync match quality — TODO

The goal is to raise the quality of `packages.hub_*` columns set by the
hub-sync pipeline: catch more matchable resources (recall), avoid
collapsing distinct locals onto the wrong hub row (precision), and grow
training coverage for long-tail categories that the classifier work
currently can't reach.

This document is the scope brief for a **separate parallel session** that
runs alongside [TODO-classifier-residual.md](TODO-classifier-residual.md).
Read this and the code; the two TODOs are scoped to not collide.

## Scope boundary with the classifier session

| Session | Writes | Reads |
|---|---|---|
| **This session (hub-sync)** | `packages.hub_*` columns (truth side) | `packages.predicted_*` (for diff diagnostics only) |
| **Classifier session** | `packages.predicted_*` columns | `packages.hub_category` (as training labels) |

All three classifier predictors gate on `WHERE hub_category IS NULL`, so
whenever this session fills new truth, predictor outputs become moot for
those rows automatically. Definition-of-done in the classifier TODO
already covers this invariant (line 364 there).

**Shared-state coordination** (per [CLAUDE.md](CLAUDE.md)):

- **DB write lock** at `%APPDATA%\com.github.kylinblue.vam-package-browser\.session-active.lock`
  — hub-sync runs are DB writes and need the lock. Can't run concurrently
  with a predictor re-run from the classifier session. Code editing in
  parallel is fine.
- **Migration slot** — schema head is v16. If this session needs v17,
  announce before claiming the slot (the classifier session might want
  one too).
- **Dev server** — only one `tauri dev` across worktrees.

## Current state

Hub-sync code lives in [src-tauri/src/hub.rs](src-tauri/src/hub.rs) (HTTP
client + scrape parsers + `score_match`) and the two sync commands in
[src-tauri/src/commands.rs](src-tauri/src/commands.rs) (full sync at
~L760, keyword-fallback retry at ~L1000, manual lookup at ~L1150).

### Match methods today

Each package row carries `hub_match_method` (column added in migration
v? per [index.rs:109](src-tauri/src/index.rs:109)):

| method | how it fires | precision |
|---|---|---|
| `filename` | local `Creator.Package` matches the CDN-reported filename from `/download` HEAD probe | exact, trusted |
| `fuzzy_title` | paid-fallback only — score from [hub.rs:812 `score_match`](src-tauri/src/hub.rs:812) on the per-creator listing | dependent on score_match logic |
| `manual` | reserved for future UI correction | n/a |

`hub_sync_state` is one of `matched | not_found | failed | gate`.

### Two-phase sync

- **Phase B1** — per-creator listing
  ([commands.rs:645](src-tauri/src/commands.rs:645)): `search_resources_by_user(creator)`,
  HEAD each hosted result to populate `filename_map`, then for each local
  package check filename_map; on miss, try paid fallback via score_match.
- **Phase B2** — keyword retry for B1's `not_found` rows
  ([commands.rs:1000](src-tauri/src/commands.rs:1000)):
  `search_resources_for_user_keyword(creator, kw, max_pages)` with the
  package name as the keyword.

### What's already learned (don't re-derive)

1. **The Skynet sync bug, 2026-05-18.** Bare 1-token title overlap (the
   old `score_match` fallback at the bottom) used to score 10 and would
   win when nothing better matched, collapsing every unmatched local
   onto whatever generic-titled bundle the creator had on the hub. Fix:
   removed the fallback entirely; bare author-match alone now scores 0.
   See [hub.rs:838-844](src-tauri/src/hub.rs:838).

   **Don't reintroduce token-overlap fallbacks without a guard.** False
   negatives are honest; false-positive pins are not.

2. **XF search ranking is unreliable for old resources.** Why B2 exists:
   per-author listing returns its own ordering that buries old resources
   in deep tails. Keyword retry with `c[users]={creator} & keywords=X`
   surfaces them.

3. **The age gate is bypassed by `vamhubconsent=yes`** in the cookie jar
   ([hub.rs:84](src-tauri/src/hub.rs:84)). XF session cookies (xf_csrf,
   xf_session) flow automatically. `is_gate_page` detects the gate page
   and returns a typed error.

4. **Hub matching is family-consistent** (verified in
   [TODO-classifier-residual.md](TODO-classifier-residual.md) line 254).
   When the hub matches `Author.Package`, every locally-installed version
   of that family gets `hub_category` set. No family-sibling propagation
   work is needed here.

## What's not solved

### 1. False-negative rate is unmeasured

After dropping the token-overlap fallback, we accepted some false
negatives as the price of removing false-positive pins. We never measured
how many. The dominant questions:

- Of current `hub_sync_state = 'not_found'` rows, how many are actually
  matchable on the hub if a human searched manually?
- What miss modes dominate (creator-rename, package-rename, abbreviated
  titles, multi-pack umbrella resources, etc.)?

**Suggested first move:** hand-label a stratified sample of ~50
`not_found` rows. Bucket the miss modes. The result tells you whether
the next fix is a better matcher, more keyword variants in B2, or
something structural (e.g. creator-alias table).

### 2. Long-tail categories under-represented (§4 of classifier TODO)

Lighting+HDRI (n=8), Audio (n=1), and similar rare hub categories
short-cut the classifier — kNN can't reliably classify into a class with
n=1 training examples. Every additional hub match in these categories
both shrinks the unmatched set AND grows rare-category training data.

The classifier TODO frames this as "self-correcting via hub-sync
coverage". This session is where that work happens. Stratify the §1
audit by predicted category — if a `not_found` is predicted Audio, it's
disproportionately high-value to chase.

### 3. Production-population accuracy (Evaluation caveats §)

The classifier holdout numbers (90.1% / 64.3% / 95.1%) are honest only
for hub-*matched* packages. They don't measure accuracy on the
hub-*unmatched* population — which skews toward niche creators, paid
offsite resources, and thin metadata. The remedy listed in the
classifier TODO is a 30-50-row hand-labeled sample of currently-
unmatched packages stratified by `predicted_method` and `predicted_confidence`.

This is naturally hub-sync session work because:
- It overlaps with §1 (both want a labeled sample of unmatched packages).
- The labels feed back into both: hub-sync gets miss-mode signal, the
  classifier gets a production-accuracy number.

**Do them as one labeling pass.** Roughly: pull ~50 not_found rows
stratified by predicted method+confidence; for each, record (a) is this
matchable on the hub? (b) if yes, why did we miss? (c) is the predicted
category correct? That single CSV answers both §1 and the classifier's
production-eval question.

### 4. `score_match` is binary-ish

The current scoring tiers (exact 200 / title-contains-pkg 100 /
pkg-contains-title 60 / else 0) don't gracefully handle near-misses
where author and package both align fuzzily but neither contains the
other (`"AcidBubbles Tongue Plus 2"` vs local `"Tongue_Plus.2"`).

**Don't change `score_match` without §1.** The 2026-05-18 fix shows that
this function is the chokepoint where false positives leak in; change it
under the protection of a labeled eval set so you can measure precision
deltas.

### 5. Periodic re-sync cadence

The hub itself keeps adding resources. Current re-sync is manually
triggered. Open question: is there value in scheduling a sitemap-delta
re-sync (cheap — ~8 HTTP requests via
[hub.rs `fetch_sitemap_catalog`](src-tauri/src/hub.rs:369)) that
auto-runs on app launch when `hub_synced_at` is older than N days?

Defer until §1-§3 prove the matching quality is good enough that more
matches is straightforwardly a win.

## Suggested first move

In order of impact-per-effort:

1. **Hand-label ~50 not_found packages**, stratified by predicted method
   and confidence. Capture miss-mode + production-category-correctness
   per row in a CSV. This is THE blocker — every other task here is
   gated on knowing what we're missing and how often.

2. **Categorize the miss modes** from the CSV. Likely buckets (from
   hub.rs comments and prior recon): creator-name normalization gaps,
   package-name-vs-title divergence, abbreviated titles, umbrella-pack
   resources, paid resources w/o hub-hosted variants.

3. **Pick the highest-frequency miss mode and write the matcher fix
   under it**, with the labeled set as the precision/recall test. Don't
   touch `score_match` without this guardrail (see §4).

4. **Stratify the same CSV for the production-population classifier
   eval** (§3). Feed the result back as a new section in
   [TODO-classifier-residual.md](TODO-classifier-residual.md) so the
   classifier session knows the true accuracy on its production target.

5. **Long-tail-focused sync** (§2). Re-run B2 keyword retry with extra
   keyword variants for `not_found` rows whose predicted category is in
   the long-tail bucket (Audio, Lighting+HDRI, etc.). Each new match
   directly grows classifier training data.

## Open questions, deferred

1. **Creator-alias table.** Some creators publish under multiple names
   (display vs username vs old handle). If §1 shows this is a dominant
   miss mode, a small alias table mapping local-creator → hub-author
   could be a high-leverage fix.

2. **Bigram/trigram match score.** Token overlap was removed because it
   was too greedy at the unigram level. Bigram or normalized-token
   set-overlap might thread the needle. Only revisit after §1 + a labeled
   precision test set exist.

3. **Auto-sync on app launch.** Cheap sitemap delta; gated on §1-§3.

4. **Refresh hub_lastmod tracking.** `hub_resources.lastmod` is set on
   sitemap import; `packages.hub_lastmod` is denormalized at sync time.
   Are stale lastmods causing us to skip re-syncing rows whose hub
   resource updated? Worth a quick check after §1.

## Definition of done

- ✅ A labeled CSV of ~50 not_found packages exists with miss-mode +
  correctness annotations, committed to the repo at a stable path.
- ✅ A matcher change motivated by the CSV ships and measurably improves
  recall on the labeled set without dropping precision.
- ✅ The classifier TODO's "production-population eval" caveat is
  resolved with a real number.
- ⏳ Long-tail (Audio, Lighting+HDRI) hub matches grow to n ≥ 5 each, at
  which point the classifier session can revisit embed-knn coverage.
- ⏳ Sitemap-delta auto re-sync (deferred — gated on the above).

## Protocols to honor

- **Lock file** before any hub-sync run that writes DB
  (see [CLAUDE.md](CLAUDE.md) Database access protocol).
- **Migration coordination** — head is v16; coordinate with the
  classifier session before claiming v17.
- **One `tauri dev`** across all worktrees.
- **Rate-limit hub requests** — the existing client uses 15s connect /
  30s read timeouts; respect that. Don't parallelize per-creator HEAD
  probes without a token-bucket.
