# Env-var DB path override — TODO

Add support for redirecting the SQLite index path away from the default
`%APPDATA%\com.github.kylinblue.vam-package-browser\index.sqlite` via an environment
variable (proposed: `VAM_INDEX_DB`). Enables per-worktree databases for
parallel Claude Code sessions without colliding on the shared default.

## Why this is deferred

The agreed mitigation for the shared-DB problem is the lock-file
convention — see the `Database access protocol` section in
[CLAUDE.md](CLAUDE.md). This refactor is the harder-enforcement
fallback **only if** the convention fails in practice (e.g. a corrupted
DB from a missed-lock race, or it turns out the honor system isn't
holding up).

Don't pre-emptively refactor.

## Scope

Small. The four CLI binaries already accept `--db <path>` overrides:

- [src-tauri/src/bin/embed_library.rs:281](src-tauri/src/bin/embed_library.rs)
- [src-tauri/src/bin/export_sample.rs:262](src-tauri/src/bin/export_sample.rs)
- [src-tauri/src/bin/tag_library.rs:293](src-tauri/src/bin/tag_library.rs)
- [src-tauri/src/bin/reclassify_sound.rs:132](src-tauri/src/bin/reclassify_sound.rs)

So just teach each binary to read `VAM_INDEX_DB` as the default-path
fallback when `--db` isn't passed. The Tauri runtime path at
[src-tauri/src/lib.rs:97](src-tauri/src/lib.rs) needs the same
treatment — read env var first, fall back to
`app_data_dir().join("index.sqlite")`.

Total: ~5 lines in `lib.rs` + ~3 lines in each of the four binaries.

## Open design questions

1. **Thumbnail cache path.** Derived from the same `app_data_dir` as the
   DB. Options:
   - Keep thumbnails on the *shared* default path (content-addressable by
     `(package_id, mtime)`, safe to share across worktrees).
   - Redirect thumbnails alongside the DB via a paired env var.

   Default to shared. Thumbnails are deterministic outputs, not racy state.

2. **Discovery / ergonomics.** How does a worktree set its `VAM_INDEX_DB`?
   - Extend `scripts\dev-env.cmd` to optionally accept a DB-name argument
     and export `VAM_INDEX_DB` before invoking the command.
   - Or document a manual `$env:VAM_INDEX_DB = ...` step in CLAUDE.md.
   - PowerShell execution policy blocks `.ps1` here (see CLAUDE.md), so
     a `direnv`-style auto-load is harder than on POSIX.

## Definition of done

- `VAM_INDEX_DB=C:\path\to\dev.sqlite scripts\dev-env.cmd npm.cmd run tauri dev`
  opens that path instead of the `%APPDATA%` default.
- All four CLI binaries respect `VAM_INDEX_DB` when `--db` isn't passed,
  and `--db` still wins when both are set.
- `Database access protocol` in CLAUDE.md gains a note documenting the
  env var and a recommended per-worktree convention.
- The lock-file convention remains useful for the default-shared-DB case
  (since most sessions will still use the default); not removed by this
  refactor.

## Trigger to revisit

Skip this until one of:
- Lock-file convention is demonstrably failing (corrupted DB, lost work).
- A workflow emerges that genuinely needs two simultaneous writers
  (e.g. running a long embed pass in worktree A while tagging in
  worktree B).

Otherwise the lock-file convention is sufficient.
