# Local embedding pipeline — TODO for a future session

## Goal

Build a local (no-network) embedding layer over the family-level `purpose`
text so the UI can offer:

- **"Find similar to this package"** — cosine nearest-neighbors over a
  family's embedding.
- **Natural-language search** — embed the user's query text, return
  top-N matching families. Lets users find packages with prompts like
  "fix clothing tightness" without needing to know the exact tag.

Companion to the LLM tagging milestone (which just shipped — see
[tagging/taxonomy-v4.json](tagging/taxonomy-v4.json) for the live
taxonomy). Tags give you discrete filtering; embeddings give you fuzzy
semantic match. Both together cover the search surface.

## What already exists

- `package_family.purpose` (text, populated for all 3706 families) —
  short LLM-cleaned summary of what the package is for. This is the
  natural input for embedding; it's clean and dense.
- `package_family.embedding` (BLOB, NULL for all rows) — schema slot
  reserved for the f32 vector. See migration v8 in
  [src-tauri/src/index.rs](src-tauri/src/index.rs).
- `package_family.embedding_model` (TEXT) — name of the model that
  produced the BLOB. Lets us version embeddings without losing prior
  ones to a re-embed pass.
- `package_family.embedded_at` (INTEGER unix seconds).
- SQLite is in WAL mode; concurrent readers + one writer is fine.

The schema is **ready**. No migration needed for the basic milestone.

## Suggested approach

- **Library**: [`fastembed-rs`](https://github.com/Anush008/fastembed-rs)
  — pure-Rust ONNX runtime, no Python, no network at inference time.
  Bundles known-good models.
- **Model**: BGE-small-en-v1.5 (384-dim, ~130 MB ONNX). Good
  quality/cost tradeoff; sub-100 ms encode per record on CPU. Auto-
  downloaded on first run to a cache dir.
- **Storage**: f32 array as BLOB on `package_family.embedding`. 384 × 4
  bytes = 1.5 KB per row. ~5.5 MB total for 3706 families. Negligible.
- **Search**: brute-force cosine in Rust over the BLOB array. ~5K
  vectors is sub-millisecond on a modern CPU. No ANN index needed yet.
  If library grows past ~50K, drop in
  [`sqlite-vec`](https://github.com/asg017/sqlite-vec) without changing
  the storage layer.

### New binary: `embed_library`

Modeled after `tag_library` (see
[src-tauri/src/bin/tag_library.rs](src-tauri/src/bin/tag_library.rs)).
Suggested flags:

- `--embed-all` — embed every family with NULL `embedding`. Default
  action.
- `--re-embed` — clear all embeddings and redo. For when the model
  changes.
- `--limit N` — bounded test.
- `--model <name>` — override default BGE-small.
- `--search "<query>"` — embed the query, return top-20 families by
  cosine similarity (id, family-name, purpose, score). CLI smoke test
  before UI integration.
- `--similar-to <family_id>` — same shape, but use a stored embedding
  as the query.
- `--db <path>` — override SQLite path. Default mirrors `tag_library`.
- `--status` — show embedded count vs total.

## Open design questions to surface to the user

1. **What to embed.** Two reasonable inputs:
   - (a) `purpose` only — cleanest signal, narrowest match.
   - (b) `purpose + tag list` (e.g. `"kind:utility-plugin function:audio-management — Plugin for importing..."`) — adds taxonomy signal to the vector. Better recall on queries that mention what kind of thing is wanted, at the cost of muddier "find similar by purpose" matches.

   Default (a). Offer (b) as a `--include-tags` flag if the user
   wants to experiment.

2. **Embedding model freedom.** BGE-small is the obvious default;
   the user might want to try nomic-embed-text-v1.5 (768-dim, larger
   model, better quality) for a quality comparison. The
   `embedding_model` column already lets us A/B without overwriting.

3. **Hybrid search.** SQLite has built-in FTS5. Wiring FTS over
   `purpose` would give exact-text matches alongside semantic.
   Reasonable v2. The user has not yet asked for hybrid — but if they
   want strong "fix clothing tightness" recall, FTS5 + cosine combined
   is the proper search stack.

4. **Search UI surface.** CLI-only initially is fine. UI integration
   is a separate milestone (see [TODO-ui-tag-hookup.md](TODO-ui-tag-hookup.md)).

## Suggested architecture for the new module

```
src-tauri/src/embedding/
  mod.rs          # pub re-exports
  model.rs        # fastembed init + cached singleton
  storage.rs      # BLOB serde (f32 array <-> Vec<u8>)
  runner.rs       # batched encode loop, writes to package_family.embedding
  search.rs       # cosine top-N, query-text encode, similar-to-family
src-tauri/src/bin/
  embed_library.rs
```

The `tagging` module is a fair shape to mirror (it has runner.rs,
record.rs, prompt.rs, seeder.rs — analogous pieces apply).

## Cost / wall time

- **API cost**: zero. fastembed is fully local.
- **First-run cost**: ~130 MB model download (one-time, cached). Or
  ship the .onnx with the app eventually.
- **Encode throughput**: ~10-50 records/sec on CPU depending on
  hardware. Whole library (~3706 families) in ~5-10 minutes.
- **Search latency**: sub-millisecond brute-force cosine.

## Definition of done

- `embed_library --embed-all` produces non-NULL `embedding` on every
  family with a non-empty `purpose`.
- `embed_library --search "fix clothing tightness"` returns
  SIMTexturePainter (or its v4 equivalent — `function:clothing-sim-texture-paint`)
  in the top results.
- `embed_library --similar-to <luxury-ship-family-id>` returns other
  vehicle-interior or post-processing-bundled environments.
- `--status` shows `embedded: 3706 / 3706` after a clean run.
- `--re-embed` cleanly clears + redoes.

## Pointers for the new session

- **Project conventions**: read [CLAUDE.md](CLAUDE.md) (PowerShell
  quirks, MSVC linker via `scripts\dev-env.cmd`, read-only invariant
  on AddonPackages, schema migration patterns).
- **Tagging module to mirror**: [src-tauri/src/tagging/](src-tauri/src/tagging/)
- **Migration history**: [src-tauri/src/index.rs](src-tauri/src/index.rs) — currently at v12.
- **Where to put the binary**: `src-tauri/src/bin/embed_library.rs`,
  add `pub mod embedding;` to [src-tauri/src/lib.rs](src-tauri/src/lib.rs).
- **API key for parity with tagging**: not needed — local model only.
- **Don't touch the LLM tagging code** — it's stable, independent
  module.

## Open question to ask before implementing

The user said BGE-small at the design stage. Worth confirming model
choice and "embed purpose only" vs "embed purpose + tags" at session
start — those calls shape the whole pipeline.
