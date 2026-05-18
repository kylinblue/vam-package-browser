# vam-package-browser

Local Windows desktop app: a visual browser/indexer for the user's VaM (Virt-A-Mate) `.var` package library. Tauri 2.x + React + TypeScript frontend, Rust backend, SQLite index, on-disk thumbnail cache.

The primary UI is a virtualized thumbnail grid (NOT a table). Browsing should feel like a fast local media gallery. Backend work (scanning archives, parsing meta.json, generating thumbnails, classifying packages, resolving dependencies) happens ahead of time so normal browsing only touches the SQLite index and thumb cache.

## Read-only invariant (critical)

The user's `.var` library — by default `D:\Games\VAM\AddonPackages` — is **strictly read-only**. The scanner opens `.var` files via read-only file handles and never writes back. All caches (`thumbnails/`, `index.sqlite`) live under `%APPDATA%\com.github.kylinblue.vam-package-browser\` (Tauri bundle-identifier path), never inside AddonPackages.

The future "visibility presets" feature WILL eventually create/move/symlink package files, but only into a *separate* active folder, with explicit user confirmation and a dry-run preview. It must never mutate the master library.

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
```
