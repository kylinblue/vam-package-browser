# vam-package-browser

Local Windows desktop app: a visual browser/indexer for the user's VaM (Virt-A-Mate) `.var` package library. Tauri 2.x + React + TypeScript frontend, Rust backend, SQLite index, on-disk thumbnail cache.

The primary UI is a virtualized thumbnail grid (NOT a table). Browsing should feel like a fast local media gallery. Backend work (scanning archives, parsing meta.json, generating thumbnails, classifying packages, resolving dependencies) happens ahead of time so normal browsing only touches the SQLite index and thumb cache.

## Read-only invariant (critical)

The user's `.var` library is **strictly read-only**. The scanner opens `.var` files via read-only file handles and never writes back. All caches (`thumbnails/`, `index.sqlite`) live under `%APPDATA%\com.github.kylinblue.vam-package-browser\` (Tauri bundle-identifier path), never inside the library.

**Which folder is the library shifts after the Visibility-Presets setup wizard runs.** Code that handles `.var` files must respect the read-only invariant on whichever folder currently holds them, derived from the `setup_complete` setting (`app_settings.value` keyed by `setup_complete`).

- **Before Visibility-Presets setup** (legacy / new installs): the library is `addon_root` (default `D:\Games\VAM\AddonPackages` — wherever the user originally pointed the scanner). VaM also reads from this path. Read-only invariant applies here.
- **After Visibility-Presets setup**: the library has been migrated to `managed_root` (default `D:\Games\VAM\AddonPackages_Managed`). Read-only invariant moves with it. `addon_root` becomes the *active folder* — VaM still reads from it, but our tool freely populates and clears it via NTFS hardlinks pointing back to `managed_root`. The two folders must share a volume; the wizard enforces that with `GetVolumeInformationW`.

The Visibility-Presets setup wizard (`src-tauri/src/setup.rs::begin_migration`) is the *only* code path permitted to mutate the library — and only as the one-time migration that establishes the post-setup model. Materialization writes (`src-tauri/src/materialize.rs::load` / `unload_all`) write into the active folder, never into the library.

## Multi-session coordination

This repo is regularly worked on via parallel Claude Code sessions, one per `git worktree`. The conventions below are how those sessions stay out of each other's way without code-level enforcement.

### Worktree convention

One worktree per feature branch, placed at `../vam-package-browser-<short-branch>`:

```powershell
git worktree add ..\vam-package-browser-feat-search -b feat/search
```

Each worktree has its own `node_modules/`, `src-tauri/target/`, and `.fastembed_cache/` (the latter via `cwd`-relative fastembed defaults). Disk-heavy but isolated; no cross-worktree collisions on build outputs or deps.

### Database access protocol

**Before any operation that writes to `index.sqlite`, check the semaphore lock at `%APPDATA%\com.github.kylinblue.vam-package-browser\.session-active.lock`.**

All worktrees share the same `%APPDATA%\com.github.kylinblue.vam-package-browser\` directory via the Tauri bundle identifier in `tauri.conf.json`, so they share **one** `index.sqlite`. Two concurrent writers will race. This is honor-system coordination — no code-level enforcement today (see `TODO-db-env-override.md` for the deferred refactor).

**Operations that count as DB writes (must lock):**
- `tauri dev` — runs migrations and the scanner on startup.
- The four CLI binaries: `tag_library`, `embed_library`, `reclassify_sound`, `export_sample`.
- Any future code path that opens the DB read-write.

**Operations that do NOT need the lock:**
- Read-only queries from another worktree — SQLite WAL allows concurrent readers cleanly.

**Lock contents** (plain text, ~4 lines):
```
acquired:  2026-05-18T19:50:00+08:00
worktree:  C:\Users\<you>\projects\vam-package-browser
branch:    main
operation: tag_library full pass (~45 min)
```

**On collision:** if the lock already exists when you're about to start work, surface its contents to the user and ask. **Do not auto-clear** a stale-looking lock — let the user decide. On a normal exit, delete the lock.

### Shared-file etiquette

Before editing files that another worktree may also be touching, surface the change to the user first. High-risk files:

- `package.json`, `package-lock.json` — dependency changes
- `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock` — Rust dependency changes
- `src-tauri/src/index.rs` — schema migrations (see below)
- `src-tauri/tauri.conf.json`, `vite.config.ts` — runtime config
- `CLAUDE.md` itself, `TODO-*.md` at repo root

### Lockfile / migration coordination

- **Only one session at a time may modify `package-lock.json` or `src-tauri/Cargo.lock`.** Parallel dep changes produce nasty merge conflicts.
- **Only one session at a time may add a new schema migration** (a new `migrate_v<N>_to_v<N+1>` function in `src-tauri/src/index.rs`). Two parallel migrations claiming the same `v<N>` slot can't be cleanly merged. If you're planning one, say so before writing.

### Dev server port

Vite is hard-pinned at `port: 1420` with `strictPort: true` (see `vite.config.ts`), and `tauri.conf.json` couples its `devUrl` to that port. **Only one `tauri dev` instance can run at a time across all worktrees.** The second one will fail to bind. There is no per-worktree port override today; convention is "the worktree actively being tested runs `tauri dev`; others do code edits headlessly."

### Testing

The current test suite is the unit tests in `src-tauri/src/deps.rs` (6 tests for dep-key parsing and resolution). They have no I/O, no DB, no fixtures — safe in any worktree at any time.

```powershell
scripts\dev-env.cmd cargo test --manifest-path src-tauri\Cargo.toml
```

There are no frontend tests yet (no `npm test` script, no `vitest`/`jest` in deps). When tests are eventually added, the multi-worktree-safe ground rules are:

- **No hardcoded ports.** Read from env or use port 0 for OS-assigned.
- **No hardcoded temp paths.** Use `tempfile::TempDir` (Rust) or `os.tmpdir()` (Node).
- **No fixed shared DB.** Either `:memory:` SQLite or a per-test temp file. Never assume the production `index.sqlite` exists or is safe to touch.

The dominant parallel-session hazard today is **not** tests — it's the shared `%APPDATA%` DB. See the Database access protocol above.

## Toolchain quirks on this machine

- **PowerShell execution policy blocks `npm.ps1`** → always invoke as `npm.cmd` (or `& npm.cmd ...`) in PowerShell, never bare `npm`.
- **Cargo is not on PATH** → prepend `$env:USERPROFILE\.cargo\bin` to `$env:Path` at the start of any PowerShell call that runs `cargo`/`rustc`. Example: `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"; & cargo build`.
- **MSVC link.exe is not on PATH** → for `cargo build`/`cargo run` (which need the linker), import the VS BuildTools env first. Helper script `scripts\dev-env.cmd` sources `vcvars64.bat`, adds cargo to PATH, and runs whatever command follows. Use it as `scripts\dev-env.cmd cargo check ...`. We use a `.cmd` (not `.ps1`) because the PS execution policy on this machine blocks running unsigned `.ps1` files. VS path: `C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat`.

## Repo layout

```
src/                      React + TS frontend (Vite)
  components/             PackageGrid (virtualized), FilterBar, etc.
  lib/api.ts              Thin wrappers around Tauri invoke()
src-tauri/                Rust backend
  src/
    main.rs               Entry — registers commands, sets up state
    meta.rs               meta.json parser (serde)
    scanner.rs            Walks AddonPackages, opens .var, reads meta
    index.rs              SQLite schema + queries (rusqlite)
    thumbnails.rs         Extracts preview images → cache dir (milestone 2)
    commands.rs           #[tauri::command] handlers
  Cargo.toml, tauri.conf.json
```

## .var format notes (from real samples)

- ZIP archive with `meta.json` at root. Filename convention: `Author.Package.<version>.var`, matches `creatorName.packageName` in meta.
- Key meta fields: `creatorName`, `packageName`, `licenseType`, `programVersion`, `description`, `contentList` (paths), `dependencies` (recursive map keyed `Author.Package.Version|latest`).
- Package type is inferred from `contentList` path prefixes: `Saves/scene/` → Scene, `Custom/Atom/Person/Morphs/` → Morphs, `Custom/Atom/Person/Textures/` → Textures, `Custom/Atom/Person/Clothing/` → Clothing, `Custom/Atom/Person/Hair/` → Hair, `Custom/Scripts/` → Plugin, `Custom/Assets/` → Assets. Mixed-content packages get classified by the dominant prefix.
- Preview convention: a `.jpg` sibling to a `.json` (scenes) or `.cslist` (plugins) is the package preview. For Look/Appearance packages the preview is usually in `Saves/Person/appearance/<name>.jpg`.

## Commands cheatsheet

```powershell
& npm.cmd install               # frontend deps (npm.cmd, not bare npm)
scripts\dev-env.cmd cargo check --manifest-path src-tauri\Cargo.toml
scripts\dev-env.cmd npm.cmd run tauri dev    # dev (Vite + Rust)
scripts\dev-env.cmd npm.cmd run tauri build  # release build
scripts\dev-env.cmd cargo test --manifest-path src-tauri\Cargo.toml
```

## Conventions

Observed from the existing code, not prescriptive:

- **Rust code:** snake_case files; modules in `src-tauri/src/` (`meta.rs`, `scanner.rs`, `index.rs`, etc.) plus two grouped subdirs (`tagging/`, `embedding/`). Unit tests live inline as `#[cfg(test)] mod tests` at the bottom of the file under test (see `deps.rs`). No integration-test directory.
- **React code:** PascalCase `.tsx` files in `src/components/`. Function components with hooks. View-state (`viewMode`, `tileSize`) persisted to `localStorage` via small string keys.
- **Tauri commands:** `#[tauri::command] pub fn` in `src-tauri/src/commands.rs`, registered in the `invoke_handler!` list in `src-tauri/src/lib.rs`. Frontend wrappers live in `src/lib/api.ts` as camelCase functions calling `invoke<T>("snake_case_name", args)`.
- **CLI binaries:** four helpers in `src-tauri/src/bin/` (`tag_library`, `embed_library`, `reclassify_sound`, `export_sample`). All accept a `--db <path>` override; default to `%APPDATA%\com.github.kylinblue.vam-package-browser\index.sqlite`.
- **Schema migrations:** numbered `migrate_v<N>_to_v<N+1>` functions in `src-tauri/src/index.rs`, applied in order at app startup by `open_and_migrate`. Currently at v15. Don't ALTER an already-applied migration; add a new one.
- **SQLite:** bundled `rusqlite`, WAL mode. Concurrent readers safe; writers must coordinate via the Database access protocol above.
- **TODO files at repo root:** active milestone notes (`TODO-*.md`). Archived/shipped milestones live in `docs/archive/`.
