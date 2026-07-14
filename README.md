# VaM Package Browser

A fast, gallery-style visual browser for your Virt-A-Mate `.var` package
library. Point it at your `AddonPackages` folder once, let it index and
thumbnail everything, then browse thousands of packages like a local media
gallery — filter by type, creator, hub category, and tags.

**Your `.var` files are never modified.** The app opens them read-only and
stores everything it derives (index, thumbnails, settings) in its own data
folder — see [Where your data lives](#where-your-data-lives).

## What it does

- **Virtualized thumbnail grid** — smooth scrolling across thousands of
  packages, with adjustable tile size and a detail view per package.
- **Automatic classification** — Scene / Looks / Clothing / Hair / Morphs /
  Textures / Plugin / Assets, inferred from package contents.
- **Hub sync (optional)** — matches your local packages against
  `hub.virtamate.com` to pull authoritative categories, license, and paid/free
  status for everything the hub knows about.
- **Dependency viewer** — see what each package pulls in, recursively.
- **Load/Unload (optional)** — feed VaM a curated subset of your library via
  NTFS hardlinks instead of keeping everything active at once.

## Requirements

- Windows 10/11, 64-bit.
- Microsoft Edge WebView2 runtime (preinstalled on Windows 11 and most
  up-to-date Windows 10 systems).
- A Virt-A-Mate `AddonPackages` folder full of `.var` files.
- Build tools (you build the app yourself — see below):
  [Node.js](https://nodejs.org) 20+, [Rust](https://rustup.rs) (stable,
  MSVC toolchain), and Visual Studio Build Tools with the
  **"Desktop development with C++"** workload.

## Getting started

There are no prepackaged downloads — you build and run the app from source.
It's two commands after the prerequisites are installed:

```powershell
git clone <this repo>
cd vam-package-browser
npm install
```

Then **double-click `run-dev.bat`** (or run it from a terminal). It locates
your MSVC/cargo toolchain and launches the app in dev mode. The first build
compiles the Rust backend and takes several minutes; later launches are fast.

If you'd rather have a standalone optimized exe instead of running through
`run-dev.bat` each time:

```powershell
scripts\dev-env.cmd npm run tauri build
```

The exe lands at `src-tauri\target\release\vam-package-browser.exe` — copy it
anywhere and run it. (Release builds use LTO and take a while; that's
expected. A portable zip of the same exe is also produced by the
[CI release workflow](.github/workflows/release.yml) for tagged versions.)

## First run

1. **Launch the app.** The path box in the toolbar defaults to
   `D:\Games\VAM\AddonPackages` — change it to wherever your VaM
   `AddonPackages` folder actually is.
2. **Click "Scan sample (200)"** for a quick taste, or **"Scan all"** to
   index the whole library. Scanning reads each `.var`'s `meta.json` and
   preview images; a few thousand packages take a couple of minutes.
3. **Click "Generate thumbnails"** to build the WebP thumbnail cache. This
   is the slowest one-time step; the grid fills in as it runs.
4. **Browse.** Filter by type or creator, search by name, open a package for
   details and dependencies.

That's the whole required setup. Re-run "Scan all" whenever you add new
packages — scans are incremental and only touch what changed.

### Optional, after the basics work

- **Hub sync** (Advanced mode) — fetches category/license/paid-status
  metadata from the VaM Hub for your packages. Needs internet; everything is
  cached locally so re-syncs are cheap.
- **"Set up library…"** — the managed-library wizard for the Load/Unload
  workflow. This is the one feature that *does* move your `.var` files (a
  one-time migration into a managed folder, with the original folder becoming
  a hardlink-populated "active" folder VaM reads from). It explains itself
  before touching anything and requires explicit confirmation. Skip it
  entirely if you just want to browse.

## Where your data lives

Everything the app derives is stored in one folder:

```
%APPDATA%\com.github.kylinblue.vam-package-browser\
```

(typically `C:\Users\<you>\AppData\Roaming\com.github.kylinblue.vam-package-browser\`)

| Item           | What it is                                                        |
| -------------- | ----------------------------------------------------------------- |
| `index.sqlite` | The package index: metadata, classifications, hub data, settings. |
| `thumbs\`      | Generated WebP thumbnail cache.                                   |

Nothing is ever written into your `.var` library folder. Deleting the data
folder is always safe — the app just starts from scratch.

## Reset / purge everything

There is deliberately no "delete all my data" button in the app. To wipe the
index and thumbnail cache manually:

1. **Close VaM Package Browser** (it holds the database file while running).
2. Run [`scripts\purge-app-data.cmd`](scripts/purge-app-data.cmd) and type
   `YES` at the prompt — or do it by hand:

   ```powershell
   Remove-Item -Recurse -Force "$env:APPDATA\com.github.kylinblue.vam-package-browser"
   ```

Your `.var` files are untouched either way. One caveat: if you previously ran
the "Set up library…" wizard, purging also forgets which packages were
hardlinked into the active folder. No files are lost (the managed folder
holds the originals), but you'll need to re-run the wizard and re-scan to
manage the library again.

## Development

Contributor documentation lives in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md):
architecture, the hub-sync scraper, the category-classifier cascade, audit
tooling, and the CLI binaries. Machine-specific toolchain notes and the
multi-worktree coordination protocol are in [CLAUDE.md](CLAUDE.md).

## License

[Apache License 2.0](LICENSE).
