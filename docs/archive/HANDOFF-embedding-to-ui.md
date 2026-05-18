# Handoff: embedding pipeline → UI integration

For the session(s) picking up [TODO-ui-tag-hookup.md](TODO-ui-tag-hookup.md)
Phase 3 ("search — depends on embedding milestone"). The embedding
backend is now shipped and validated end-to-end against the live 3706-
family corpus. This doc tells you what's available, what isn't, and
what we learned from smoke tests.

## Status (2026-05-17)

| Layer | State |
|---|---|
| Schema (migration v13: `family_embeddings` table) | shipped, applied |
| `embedding/` Rust module (model, storage, runner, search) | shipped |
| `embed_library` CLI binary (all flags) | shipped |
| Full encode pass over 3706 families × 4 variants (14,824 vectors) | done, 100% coverage |
| Smoke tests (`--search`, `--similar-to`, `--compare-search`) | passing |
| **Tauri commands exposing search to the frontend** | **NOT done — this is your job** |
| Frontend UI (search bar, "find similar" affordance) | not started |
| FTS5 hybrid (lexical + semantic) | not started (TODO Phase 3 mentions, out of embedding-milestone scope) |
| Query rewriting / synonym layer | not started |

## Backend API surface

Everything you need is in `src-tauri/src/embedding/`.

### Two functions you'll call

```rust
// Natural-language search bar — embed the user's query, return top-N families
embedding::search::search_text(
    conn: &rusqlite::Connection,
    model: ModelChoice,
    input: InputKind,
    query: &str,
    top_n: usize,
) -> anyhow::Result<Vec<SearchHit>>

// "Find similar to this" button on detail view — anchor by a known family
embedding::search::search_similar_to_family(
    conn: &rusqlite::Connection,
    model: ModelChoice,
    input: InputKind,
    family_id: i64,
    top_n: usize,
) -> anyhow::Result<Vec<SearchHit>>
```

### Result type (already `serde::Serialize`)

```rust
pub struct SearchHit {
    pub family_id: i64,
    pub creator: String,
    pub package_name: String,
    pub purpose: Option<String>,
    pub score: f32,
}
```

Serializes cleanly across Tauri's `invoke()` boundary — frontend gets it as a
plain JSON array of objects, no extra mapping needed.

### Enums

```rust
ModelChoice::BgeSmallEnV15       // 384-dim, ~130 MB, ~5ms query encode
ModelChoice::NomicEmbedTextV15   // 768-dim, ~250 MB, ~15ms query encode

InputKind::Purpose               // embed purpose text only
InputKind::PurposeWithTags       // embed "<tags joined by ' '> — <purpose>"
```

## Suggested Tauri command wrappers

The natural shape for `src-tauri/src/commands.rs`:

```rust
use crate::embedding::{search, InputKind, ModelChoice};

#[tauri::command]
pub async fn search_families(
    state: tauri::State<'_, AppState>,
    query: String,
    top_n: usize,
) -> Result<Vec<search::SearchHit>, String> {
    let conn = state.db.lock();
    search::search_text(
        &conn,
        ModelChoice::NomicEmbedTextV15,
        InputKind::Purpose,
        &query,
        top_n,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn search_similar_families(
    state: tauri::State<'_, AppState>,
    family_id: i64,
    top_n: usize,
) -> Result<Vec<search::SearchHit>, String> {
    let conn = state.db.lock();
    search::search_similar_to_family(
        &conn,
        ModelChoice::NomicEmbedTextV15,
        InputKind::Purpose,
        family_id,
        top_n,
    )
    .map_err(|e| e.to_string())
}
```

Register them in `lib.rs` alongside the existing `commands::*` entries in the
`invoke_handler` list.

### One important consideration: model load happens on first call

`search_text` lazily initializes the fastembed model on first invocation
(~1-3 sec for BGE, ~3-5 sec for nomic, plus ~130-250 MB download on the
very first run ever). On subsequent calls the model is cached in a
process-static `Mutex<Option<TextEmbedding>>` and a query encode is
~5-15ms.

For a snappy first-search experience, consider triggering a warm-up
encode in the Tauri `setup` hook (e.g. `search_text(..., "warmup", 1)`
spawned on a thread). Otherwise the user's first query in any session
will appear to hang for a few seconds.

## Variant defaults (from `--compare-search` smoke tests)

| Recommendation | Variant | Why |
|---|---|---|
| **Default** | `nomic / purpose` | Best recall on the natural-language test queries we ran. ~3× slower than BGE per query but still <20ms. |
| Fallback / experiment | `bge / purpose` | Close second on quality, fastest. Good if you want lower latency. |
| For category-word queries | `* / purpose-with-tags` | Specifically helps when the query mentions taxonomy concepts ("audio plugin", "post-processing scene"). Hurts otherwise. |

We embedded all four variants × 3706 families = 14,824 vectors (~34 MB
storage). The `family_embeddings` table is keyed by `(family_id, model,
input_kind)`, so you can switch variants per query without re-encoding
anything — just pass different `ModelChoice` / `InputKind` to
`search_text`.

If you want to expose variant selection in the UI (e.g. an advanced
"search mode" dropdown), the four variants are all live and ready.

## Limitations to surface in UX

1. **Lexical proximity dominates semantic intent.** Example: query
   "fix clothing tightness" surfaces clothes whose purpose literally
   contains "tight" (skinny jeans, tight gym bottoms) ahead of the
   one tagged `function:clothing-sim-texture-paint` whose purpose says
   "controls looseness". Both BGE and nomic do this. A query-rewriting
   / synonym layer at the UI level (or hybrid with FTS5) would help.
   For now, set user expectations: this is similarity search, not
   intent matching.

2. **Score scales differ across models.** BGE returns ~0.6-0.85 in our
   data; nomic ~0.55-0.75. Don't render raw scores in a way that
   invites cross-model comparison (e.g. don't show "0.78" without a
   normalization step). For single-variant search, you can show the
   score as a relevance bar; min/max within the result set is a fine
   normalizer.

3. **No bottom threshold on results.** `search_text` returns the top-N
   by cosine, period. If there are no good matches, you still get N
   results — they'll just have low scores. UI should probably hide
   results below a quality floor (~0.5 is a rough vibe) or show an
   "uncertain match" affordance.

## What's NOT done (and why)

- **Tauri command wrappers** — left to your session because the
  command surface lives in `commands.rs` which you're already touching
  for Phase 1+2. Easier to add the two new ones in the same diff than
  for me to wedge them in from this side.
- **FTS5 hybrid** — TODO Phase 3 mentions SQLite FTS5 over `purpose`
  as a complementary lexical layer. Not built. Probably worth doing
  for the "fix clothing tightness" class of query where lexical exact-
  match beats semantic.
- **Background model warm-up on app start** — see consideration above.
- **Embedding regeneration on re-tag** — the runner is idempotent
  (`--embed-all` only embeds families that are missing). If `purpose`
  text gets edited later, you'd need `--re-embed` to invalidate the
  stored vector. Not automated.

## CLI references (use these to debug from your shell during UI work)

```powershell
# What's currently embedded
src-tauri\target\debug\embed_library.exe --status

# Single-variant search
src-tauri\target\debug\embed_library.exe --search "<query>" [--model bge|nomic] [--input purpose|purpose-with-tags] [--top-n N]

# Side-by-side variant comparison (most informative for tuning)
src-tauri\target\debug\embed_library.exe --compare-search "<query>" [--top-n N]

# Similarity from an anchor family
src-tauri\target\debug\embed_library.exe --similar-to <family_id> [--model ...] [--input ...] [--top-n N]
```

The `--compare-search` output renders a rank matrix across all four variants
(showing where each variant places each family) plus a summary count
of consensus vs single-variant outliers. Useful for tuning the default
variant choice with real-data evidence rather than vibes.

## File pointers

| Path | Purpose |
|---|---|
| [src-tauri/src/embedding/mod.rs](src-tauri/src/embedding/mod.rs) | module entry, re-exports |
| [src-tauri/src/embedding/search.rs](src-tauri/src/embedding/search.rs) | the two functions you'll call |
| [src-tauri/src/embedding/model.rs](src-tauri/src/embedding/model.rs) | fastembed model wrappers, lazy init |
| [src-tauri/src/embedding/storage.rs](src-tauri/src/embedding/storage.rs) | BLOB serde + DB read/write |
| [src-tauri/src/embedding/runner.rs](src-tauri/src/embedding/runner.rs) | batched encode loop (not needed for UI) |
| [src-tauri/src/bin/embed_library.rs](src-tauri/src/bin/embed_library.rs) | CLI; reference for how to call the API |
| [src-tauri/src/index.rs](src-tauri/src/index.rs) | migration v13 + `family_embeddings` schema |
| [TODO-ui-tag-hookup.md](TODO-ui-tag-hookup.md) | your Phase 3 plan |
| [TODO-embedding-pipeline.md](TODO-embedding-pipeline.md) | original design doc, now done |

## Data state (the corpus is ready)

```
families:                3706 / 3706 with non-empty purpose
embeddings stored:       3706 × 4 variants = 14,824
storage footprint:       ~34 MB BLOB total in family_embeddings
last full embed:         2026-05-17, dev build, CPU
models used:             BGE-small-en-v1.5, nomic-embed-text-v1.5
```

No data prep needed before integration.

## One footnote: don't accidentally re-embed

The embedding runner is idempotent — it only encodes families with no
existing row in `family_embeddings` for the given `(model, input_kind)`.
But `--re-embed` will wipe and redo, and is destructive (the data
itself is recoverable since the underlying `purpose` text is preserved,
but you'd be re-spending ~15 min of CPU). Useful if a model is upgraded
or the input format changes; don't run reflexively.
