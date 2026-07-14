# Development notes

Deep-dive documentation for contributors. For install/first-run instructions
see the top-level [README](../README.md); for machine-specific toolchain
quirks and the multi-session protocol see [CLAUDE.md](../CLAUDE.md).

## Status snapshot — 2026-05-22

Schema head **v16**. Library size at last census: **4353 packages** across
**2299 labeled families** / **3871 tagged families**.

| Area                                           | State                                                              |
| ---------------------------------------------- | ------------------------------------------------------------------ |
| Scanner + meta parser + dep resolver           | shipped                                                            |
| SQLite index + WAL                             | shipped (16 migrations)                                            |
| Virtualized thumbnail grid + detail view       | shipped                                                            |
| Thumbnail cache (WebP, sub-image pull-through) | shipped                                                            |
| LLM tagging pipeline (`tag_library`)           | shipped (v4 taxonomy, grok backend)                                |
| Family embeddings (Nomic + BGE, 4 variants)    | shipped (`embed_library`)                                          |
| Hub scrape + two-phase sync                    | shipped, precision-hardened (see below)                            |
| Hub-category classifier cascade                | shipped — **99.86%** category coverage (see below)                 |
| Classifier review / audit tooling              | shipped (see below)                                                |
| `Ask…` semantic-search bar + "find similar"    | wired end-to-end, **shelved** until v4 retag pass                  |
| Dependency viewer                              | shipped                                                            |
| Visibility presets (active-folder workflow)    | in flight on parallel branches; not yet on `main`                  |

## Hub-sync — XenForo scrape with precision guards

Hub matching is what gives the UI an authoritative `hub_category` axis and
metadata (license, billing tier, hub-hosted flag) for the ~58% of local
packages that exist on `hub.virtamate.com`. Code lives in
[src-tauri/src/hub.rs](../src-tauri/src/hub.rs) (HTTP client + scrape parsers +
`score_match`) and [src-tauri/src/commands.rs](../src-tauri/src/commands.rs)
(sync orchestrator, keyword-fallback retry, manual lookup).

What's interesting about the implementation:

- **Real XenForo search, not the listing query.** `/resources/?q=…` silently
  ignores `q`; we do the proper CSRF-token-then-POST flow against
  `/search/search` and follow the 303 to the results page.
- **Two-phase sync.** Phase B1 scrapes the per-creator listing and uses CDN
  `Content-Disposition` filenames as the primary match key
  (`hub_match_method='filename'`, exact and trusted). Phase B2 retries the
  B1 misses with keyword search, since XF's per-author ordering buries old
  resources.
- **Age-gate bypass** via `vamhubconsent=yes` in the cookie jar. XF session
  cookies flow automatically. A typed `gate` error keeps the run reportable
  if the bypass fails.
- **Sitemap-delta catalog.** `fetch_sitemap_catalog` cached in
  `hub_resources(resource_id, slug, lastmod, fetched_at)` so re-syncs are
  cheap delta passes, not full N-package crawls.
- **Family-consistent matching.** When a package family matches, every
  locally-installed version gets `hub_category` set in one go — no
  family-sibling propagation pass needed.
- **Precision over recall.** The old `score_match` token-overlap fallback
  collapsed every unmatched local onto whatever generic-titled bundle the
  creator had on the hub (the "Skynet sync bug" of 2026-05-18). It was
  removed; bare author-match now scores 0. False negatives are honest;
  false-positive pins are not.

Live progress streams over the `hub-sync-progress` Tauri event into
[HubSyncView.tsx](../src/components/HubSyncView.tsx), which also recovers
in-flight sync state across HMR reloads.

What's open: a labeled-sample miss-mode audit
([TODO-hub-sync.md](../TODO-hub-sync.md) §1) is the gate for the next round of
matcher work — `score_match` won't move without a precision/recall guardrail.

## Intelligent, sophisticated categorization

`hub_category` is the single category axis for the UI. The hub itself
supplies ground truth for hub-matched packages. The remaining ~42% — niche
creators, paid offsite resources, thin metadata — is predicted by a
three-method cascade, with each predictor only allowed to overwrite a row
that is either fresh or has `predicted_confidence < 0.6`.

| Phase | Method                                                                      | Holdout accuracy (20% test, seeded) | Binary                                                                                  |
| ----- | --------------------------------------------------------------------------- | ----------------------------------- | --------------------------------------------------------------------------------------- |
| 0.5   | **kind-vote** — `P(hub_category \| kind)` summed across a family's kind set | **90.1%**                           | [`predict_categories`](../src-tauri/src/bin/predict_categories.rs)                      |
| 2a    | **graph-prop** — neighbor-category distribution as feature, cosine-kNN      | 64.3% (standalone, lower bound)     | [`propagate_categories`](../src-tauri/src/bin/propagate_categories.rs)                  |
| 2b    | **embed-knn** — Nomic-embed-text-v1.5 family embeddings, cosine-kNN         | **95.1%**                           | [`embed_predict_categories`](../src-tauri/src/bin/embed_predict_categories.rs)          |

Unified UI query: `COALESCE(hub_category, predicted_hub_category)`. Current
coverage **4347 / 4353 = 99.86%**. The 0.14% residual is malformed-metadata
rows where the scanner couldn't extract a usable `creator.package_name`.

Things worth knowing about the design:

- **Cascade, not fusion.** embed-knn writes last, so the highest-accuracy
  method gets the final say on every low-confidence row. Score-level fusion
  was considered and deferred — the cascade is already near-optimal for the
  common case.
- **Honest evaluation.** Every predictor has a `--holdout-test` mode that
  trains on a fixed 80% family split and reports accuracy on the held-out
  20%. Same seed across binaries
  ([src-tauri/src/holdout.rs](../src-tauri/src/holdout.rs)) means the three
  methods are directly comparable on identical test families. Every
  predictor's holdout meets or beats its CV number — the opposite of what
  tuning leak would show.
- **Graph-prop is a feature kNN, not label propagation.** The first attempt
  was label propagation; CV came back at 2.8% because this dep-graph is
  anti-assortative (a Scene's neighbors are Plugins/Hair/Clothing, so a
  vote propagator labels Scenes "Plugins"). Refactor: use the neighbor
  category distribution as the feature vector. CV jumped to 61.2%.
- **embed-knn catches what kind-vote can't.** It found 6 Lighting+HDRI
  cases that no other method produces.
- **Self-healing scan.** `scanner::scan` ends with `family::recompute` so
  fresh installs never sit with `family_id = NULL`
  ([src-tauri/src/scanner.rs:222](../src-tauri/src/scanner.rs:222)).
- **Hub-truth is sacred.** All three predictors gate on
  `WHERE hub_category IS NULL` in their UPDATEs — model output never
  collides with ground truth.

Open work tracked in [TODO-classifier-residual.md](../TODO-classifier-residual.md):
auto-tag + auto-embed for newly-created families (the manual cycle costs
~$0.20/run for 165 families; structural wiring is the obvious next step),
a singleton-set rule for the `audio-pack`-when-alone case, UI exposure of
the prediction axis with a confidence threshold.

## Performant review + audit tooling

Categorization at this scale is only credible if you can spot-check it
without opening every `.var`. Four read-only binaries make that fast:

- **[`classifier_gaps_census`](../src-tauri/src/bin/classifier_gaps_census.rs)** —
  the one-shot dashboard. Buckets every package into A (hub truth) /
  B (predicted) / C (unpredicted-with-family) / D (no family_id), counts
  each, shows first-N examples per bucket, then breaks B and C down by
  whether the family has a labeled sibling. Single command, no flags,
  ground truth for "how much residual is left and where." Read-only; safe
  to run from any worktree.
- **[`sample_predictions`](../src-tauri/src/bin/sample_predictions.rs)** —
  deterministic stratified sampler. Picks N predicted rows, writes a
  Markdown file with one section per package showing predicted category +
  method + confidence + scanner type + family purpose + `kind:*` tags.
  Enough to eyeball whether a prediction is right without touching the
  archive. Seedable so reviews are reproducible.
- **[`propagate_categories --audit`](../src-tauri/src/bin/propagate_categories.rs)** —
  empirical check of dep-graph assortativity. Reports
  `P(neighbor shares category)` per category; the file that proved the
  first label-propagation attempt was hopeless and motivated the feature-kNN
  refactor.
- **[`reclassify_sound`](../src-tauri/src/bin/reclassify_sound.rs)** —
  targeted backfill helper. Used to apply the narrow scanner patch that
  suppresses `Sound` from the contentList dominance contest when a scene
  file is present (109 hub-Scenes were being mislabeled because creators
  bundle audio). The pattern is reusable for future targeted re-classifies.

All four respect a `--db <path>` override; default is the standard
`%APPDATA%` index. None of them lock — they're read-only by design and the
multi-session DB protocol only locks writers (SQLite WAL handles concurrent
readers cleanly).

Held-out evaluation flag on every predictor (`--holdout-test`) reuses
`src-tauri/src/holdout.rs` for a deterministic, order-independent 80/20
family split with unit-tested splitters. Run any predictor in holdout mode
and you get an honest accuracy number in one command.

## Architecture

```
src/                      React + TS frontend (Vite, port 1420 pinned)
  components/             PackageGrid (virtualized), HubSyncView, DetailView,
                          FacetPanel, HubCategoryChips, TagChips, …
  lib/api.ts              Thin wrappers around Tauri invoke()
src-tauri/                Rust backend
  src/
    main.rs / lib.rs      Entry, command registration, thumb:// protocol
    meta.rs               meta.json parser (serde)
    scanner.rs            Walks AddonPackages, opens .var, reads meta
    index.rs              SQLite schema + 16 migrations (rusqlite, WAL)
    hub.rs                XenForo scrape client + score_match
    deps.rs               Dep-key parsing + recursive dep resolution
    holdout.rs            Deterministic 80/20 family split (seeded)
    embedding/            fastembed + ONNX pipeline (Nomic, BGE)
    tagging/              v4 taxonomy LLM tagger (grok backend)
    commands.rs           #[tauri::command] handlers
    bin/                  CLI tools: tag_library, embed_library,
                          predict_categories, propagate_categories,
                          embed_predict_categories, classifier_gaps_census,
                          sample_predictions, reclassify_sound, export_sample
```

Stack notes: bundled `rusqlite` with WAL mode (concurrent readers, single
writer), `fastembed` for local ONNX embeddings (~250 MB model weights cached
to `.fastembed_cache/`), `ureq` + `scraper` for the hub client, `image` +
`webp` for thumbnail generation. The thumb protocol serves either
pre-generated WebP from cache or streams sub-image source bytes straight
out of the .var archive with a size cap, so the gallery doesn't pile up
browser RAM on huge texture maps.

## Multi-session coordination

This repo is regularly worked on via multiple parallel Claude Code sessions
(one per `git worktree`). They share one `%APPDATA%` SQLite index via the
Tauri bundle identifier, coordinated by an honor-system lock file at
`%APPDATA%\com.github.kylinblue.vam-package-browser\.session-active.lock`. See
[CLAUDE.md](../CLAUDE.md) for the full protocol (lock requirements, migration
slot coordination, dev-server port pinning, shared-file etiquette).

## Releasing

Releases are built by GitHub Actions
([.github/workflows/release.yml](../.github/workflows/release.yml)):

1. Bump `version` in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml`,
   and `package.json` (keep them in sync).
2. Commit, tag `v<version>`, push the tag.
3. The workflow builds on `windows-latest` (`tauri build --no-bundle`) and
   attaches a portable zip — the exe plus any runtime DLLs — to a **draft**
   release. Review and publish it by hand. No installer is produced; the
   documented install path is building from source via `run-dev.bat`.

`workflow_dispatch` runs the same build without creating a release (artifacts
only), useful for validating the pipeline.
