# Visibility presets — TODO

## Goal

Let the user pick a small working subset of their `.var` library, automatically
include every package that subset transitively depends on, and expose **only
that closure** to VaM — so VaM boots fast and its in-memory package index
isn't paying for thousands of packages the user isn't currently using.

**Headline workflow: per-author filtering.** The primary mental model is
"show me everything by Author X (plus what X's content needs to run)." The
user picks one or more creators; the system seeds the closure with every
locally-owned package by those creators, then walks the dep graph to pull in
shared plugins (Timeline, Embody, etc.) and any other supporting content.
Hand-picked individual packages remain a secondary seeding mode for
power-user cases (single scene, custom curation).

This is the "visibility presets" feature referenced in
[CLAUDE.md](CLAUDE.md) and [docs/archive/TODO-dependency-viewer.md](docs/archive/TODO-dependency-viewer.md);
the dep graph it depends on shipped, but the materialization layer and UI never landed.

## Why now

VaM's load time and RAM footprint scale roughly with `count(AddonPackages/*.var)`.
On a multi-thousand-package library the user pays that cost for every launch
even if they're only working on one scene. With a resolved dep graph already
in the DB ([src-tauri/src/deps.rs](src-tauri/src/deps.rs),
`package_dep_links`), we have everything needed to compute a minimal closure
and project it onto disk in a form VaM can consume.

## Architecture — inverted from CLAUDE.md's pre-feature model

The earlier draft of this plan tried to keep `D:\Games\VAM\AddonPackages`
read-only and direct VaM at a separate active folder. That requires the user
to reconfigure VaM, which they have to do outside this app, and is the kind
of friction that kills adoption.

The accepted design: **VaM keeps pointing at its existing AddonPackages
directory.** That directory becomes the **active folder** (managed by us,
populated with hardlinks). The real `.var` files move once, at setup time,
into a sibling **managed folder** (default name: `AddonPackages_Managed`)
which becomes the new read-only library.

**This changes the project-level read-only invariant** currently asserted
in [CLAUDE.md](CLAUDE.md). The new invariant set:

- **Pre-setup** (existing code path, today's state): `addon_root`
  (= `D:\Games\VAM\AddonPackages`) is the library and is strictly
  read-only. This matches CLAUDE.md as written. No change to scanner
  behavior until the user runs Setup.
- **Post-setup**: `managed_root` (= `D:\Games\VAM\AddonPackages_Managed`
  or wherever the user chose) is the library. **It** is the read-only
  invariant — never written/moved/deleted by anything but Setup itself.
  `addon_root` becomes the active folder: fully managed by this tool,
  freely populated and cleared.

A footnote in CLAUDE.md will be added when this feature lands so the
invariant text reflects post-setup state.

## Invariants (non-negotiable)

1. **The managed folder is read-only after setup.** Once setup completes,
   `managed_root` is never written, moved, renamed, or deleted from by any
   code path other than (a) the scanner reading it and (b) Setup itself.
2. **The active folder is `addon_root`** (the path VaM has always read
   from). After setup it contains only hardlinks placed by this tool.
3. **Managed and active folders must be on the same NTFS volume.**
   Hardlinks don't cross volumes, and hardlinking is the only materialization
   method we support. The Setup wizard blocks committing if the user picks
   a managed path that fails a same-volume probe.
4. **Setup runs at most once per install.** A persisted `setup_complete`
   flag gates it. Reversing setup is out of scope (see open questions);
   manual reversal is possible but unsupported.
5. **The managed folder must be empty (or non-existent) at Setup-commit
   time.** If it has any pre-existing content, Setup refuses to start the
   migration — collision risk is too high.
6. **Active folder destructive ops are sandboxed.** We only delete files we
   placed there (tracked in `active_folder_state`). We never
   `remove_dir_all` the active folder or any unknown directory.
7. **Every materialization is preceded by a dry-run preview.** User sees
   "+N add / −M remove / =K keep" before a single byte hits disk.
8. **A scan never touches the active folder.** Post-setup, scanning walks
   `managed_root` only. Load/Unload is the only code path that writes to
   the active folder.

## Conceptual model

```
   pre-setup                          post-setup
   ─────────                          ──────────

   ┌──────────────────────┐           ┌──────────────────────────────┐
   │  AddonPackages       │           │  AddonPackages_Managed       │
   │  D:\…\AddonPackages  │  setup    │  D:\…\AddonPackages_Managed  │
   │  every .var, real    │ ────────▶ │  every .var, real            │
   │  read-only by us     │  one-time │  read-only by us (LIBRARY)   │
   │  ↑                   │   move    │                              │
   │  VaM reads here      │           └──────────────┬───────────────┘
   └──────────────────────┘                          │ hardlink
                                                     │ (same NTFS volume)
                                                     ▼
                                      ┌──────────────────────────────┐
                                      │  AddonPackages               │
                                      │  D:\…\AddonPackages          │
                                      │  hardlinks only, managed     │
                                      │  by this tool (ACTIVE)       │
                                      │  ↑                           │
                                      │  VaM still reads here        │
                                      └──────────────────────────────┘
```

**Key property: VaM never sees the change.** It keeps reading from the same
path it always has. The migration shifts which path holds the real bytes,
but the path VaM cares about (the original `AddonPackages` directory) is
preserved.

**Same-volume constraint.** Hardlinks are NTFS directory entries pointing at
the same file extents as the source. They can't cross volumes — a hardlink
from `E:\AddonPackages\` to `D:\AddonPackages_Managed\foo.var` is not a
valid operation. The managed folder **must live on the same drive as VaM**
(and on an NTFS volume — not exFAT/FAT32). The Setup wizard enforces this
with a probe before saving the path.

## Setup wizard — one-time migration

Setup is the gateway. Nothing in the Load/Unload UI is reachable until
Setup has run; the wizard intercepts the first launch of the feature.

### Inputs

1. **AddonPackages path** (active folder, where VaM reads). Pre-populated
   from the existing `addon_root` setting — already known to the app from
   the scanner config. Read-only in the wizard; if it's wrong, the user
   fixes it in the existing scanner settings first.
2. **Managed path** (new library, where real `.var`s will move).
   Pre-filled with `<parent(addon_root)>/AddonPackages_Managed` —
   typically `D:\Games\VAM\AddonPackages_Managed`. Editable folder picker.

### Validation, in order, blocking commit on any failure

1. **Managed path is not equal to or under addon_root.** Refuse — they
   can't be nested.
2. **Managed path is on the same NTFS volume as `addon_root`.** Probe via
   `GetVolumeInformationW` + `GetVolumePathNameW` to compare volume
   serials. If they differ, refuse with the exact diagnostic
   (*"AddonPackages is on D: (serial ABCD1234); managed folder would be
   on E: (serial DEAD9876). Hardlinks can't cross drives."*).
3. **Managed path is NTFS.** Reject FAT32/exFAT; reject network shares.
4. **Managed path is empty.** If it doesn't exist, Setup will create it.
   If it exists, it must contain zero entries (no files, no subdirs,
   not even hidden ones). Anything else → refuse with a clear message
   listing what's in there. This is the user's explicit instruction
   ("The managed path MUST be empty"); no override.
5. **Live hardlink probe.** Pick any small `.var` from `addon_root`,
   attempt `hard_link` into the managed folder, stat the result, unlink.
   Any failure (permissions, AV interference) → refuse with the OS error.
6. **VaM not running.** Probe known VaM executable names via
   `CreateToolhelp32Snapshot` (or the simpler check: try to open one
   `.var` in `addon_root` with `OpenOptions::write(true)` — if it
   fails with `ERROR_SHARING_VIOLATION`, VaM still holds it). Refuse
   if any handles are open.

### Pre-commit warning modal

After all validations pass, before any FS write, surface a blocking
modal with the exact scope of what's about to happen:

> **About to move your VaM package library.**
>
> - **Source** (AddonPackages): `D:\Games\VAM\AddonPackages`
>   — 3,824 `.var` files, 247.3 GB
> - **Destination** (new managed folder): `D:\Games\VAM\AddonPackages_Managed`
>   — currently empty
>
> Every `.var` will be moved from the source to the destination.
> VaM will still read from `AddonPackages` — this tool will populate it
> with hardlinks based on what you choose to "load."
>
> **This is a one-time operation. Make sure VaM is closed.**
> **It cannot be cleanly undone from inside this app.**
>
>   ☐ I have closed VaM.
>   ☐ I understand the move is one-way.
>
> [ Cancel ]    [ Start migration ]

Both checkboxes must be ticked to enable the Start button.

### Migration algorithm

The move is per-file `std::fs::rename` from source to destination. On the
same NTFS volume, this is an O(1) MFT rewrite — no byte copy, no SSD
wear beyond directory-entry updates. Estimated runtime: a few seconds
even for thousands of files.

```
1. Take the DB write lock (per CLAUDE.md DB access protocol).
2. Create managed_root if missing.
3. Begin a SQLite transaction.
4. For each row in `packages`:
     old_path := row.var_path
     new_path := managed_root / basename(old_path)
     fs::rename(old_path, new_path)              -- atomic on same volume
     UPDATE packages SET var_path = new_path WHERE id = row.id
5. Commit transaction.
6. Walk addon_root for any leftover *.var (files that weren't in the DB
   because they were added after the last scan). Move each into managed_root
   too — these are still the user's content, just not yet indexed. Trigger
   a scan afterward so they get indexed in their new location.
7. Persist settings: managed_root, managed_volume_id, setup_complete=true,
   setup_completed_at=now.
8. Release the lock.
```

The walk in step 6 is a one-time catch-all; in steady state every `.var`
in `addon_root` is either a hardlink we placed or a leftover the user
dropped in (handled by the post-setup scanner — see "Post-setup
scanner behavior" below).

### Progress UI

Migration emits Tauri events as it runs:
- `migration.progress { moved: i64, total: i64 }` — for a determinate progress bar.
- `migration.file { path: String }` — current file being moved (optional, for
  log-style detail).
- `migration.error { path: String, err: String }` — non-fatal per-file errors.
- `migration.done { moved: i64, errors: i64, elapsed_ms: i64 }`.

### Crash / interruption recovery

`fs::rename` on the same NTFS volume is atomic per file. We commit the DB
row update in the same transaction as a logical group (e.g. batches of
~500 renames per commit) so a crash leaves a consistent partial state:
every `packages.var_path` either points at the old `addon_root` location
(file still there) or the new `managed_root` location (file there).

On next launch, if `setup_complete` is false but `managed_root` is set:
- Detect mid-flight migration: any `packages.var_path` rows pointing into
  `addon_root` while `managed_root` is set indicate "resume."
- Show: *"Migration was interrupted. N files were moved, M remain. Resume?"*
- Resume from step 4 of the algorithm.

### Post-setup scanner behavior

After setup, the scanner walks `managed_root` instead of `addon_root`.
The `addon_root` directory is the tool's domain — it expects to find only
hardlinks it placed.

If the scanner detects `.var` files in `addon_root` that are *not* in
`active_folder_state` (the user dropped a new .var into the VaM folder by
hand, e.g. via Hub download), it surfaces a banner:

> *"3 new packages found in AddonPackages. Move them into the managed
> library?"*  [ Move & re-link ] [ Ignore ]

"Move & re-link" runs a mini-migration on those files (rename →
managed_root, then re-hardlink into addon_root, then trigger scan to
index them). "Ignore" leaves them as real files in addon_root; they'll
show up in the index but won't be hardlinks.

### Reversal (out of scope, document the manual recipe)

We don't ship a "Reverse setup" command. If the user wants to undo:

1. Close VaM.
2. Clear the active folder (this tool's "Unload all" button).
3. Move every `.var` from `managed_root` back to `addon_root` by hand.
4. Delete `managed_root` when empty.
5. Re-point the scanner to `addon_root` and rescan; or clear the DB and
   start fresh.

Documented in the in-app Setup info screen as "If you want to undo this
later, here's the manual recipe…" so a user who regrets the move isn't
stranded.

## Data model

New tables (proposed migration v16):

```sql
CREATE TABLE visibility_presets (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    created_at  INTEGER NOT NULL,           -- unix seconds
    updated_at  INTEGER NOT NULL
);

-- Author seeds. Resolved dynamically at closure time (all current packages
-- where packages.creator = creator). New .var files scanned after seeding
-- automatically participate in the next apply.
CREATE TABLE visibility_preset_creators (
    preset_id INTEGER NOT NULL,
    creator   TEXT NOT NULL,                 -- matches packages.creator (case-sensitive)
    PRIMARY KEY (preset_id, creator),
    FOREIGN KEY (preset_id) REFERENCES visibility_presets(id) ON DELETE CASCADE
);

-- Individual-package seeds. Used for secondary "hand-picked" workflows
-- (single scene, curated demo set). Co-exists with creator seeds; the seed
-- set fed to closure is their union.
CREATE TABLE visibility_preset_packages (
    preset_id  INTEGER NOT NULL,
    package_id INTEGER NOT NULL,
    PRIMARY KEY (preset_id, package_id),
    FOREIGN KEY (preset_id)  REFERENCES visibility_presets(id) ON DELETE CASCADE,
    FOREIGN KEY (package_id) REFERENCES packages(id)            ON DELETE CASCADE
);

-- Tracks what we have actually materialized in the active folder right now,
-- so we can compute a clean diff and never delete a file we didn't create.
-- All entries are hardlinks (the only supported method); no method column.
CREATE TABLE active_folder_state (
    package_id      INTEGER PRIMARY KEY,
    active_path     TEXT NOT NULL,           -- absolute path inside active folder
    materialized_at INTEGER NOT NULL,
    FOREIGN KEY (package_id) REFERENCES packages(id) ON DELETE CASCADE
);

CREATE INDEX idx_preset_pkgs_pkg     ON visibility_preset_packages(package_id);
CREATE INDEX idx_preset_creators_cr  ON visibility_preset_creators(creator);
```

Settings additions (`settings` k/v table, no migration needed):

- `managed_root`        — absolute path of the managed library (real .var files post-setup).
- `managed_volume_id`   — NTFS volume serial captured at setup commit. Re-probed
  on every Load/Unload; mismatch aborts before any FS write.
- `setup_complete`      — `"1"` once the one-time migration finished.
- `setup_completed_at`  — unix seconds; informational.
- `active_preset_id`    — preset currently materialized in active folder (NULL = ad-hoc).

The existing `addon_root` setting keeps its meaning: where VaM reads from.
Pre-setup it holds the real `.var`s; post-setup it holds only hardlinks
placed by Load. The scanner reads from `managed_root` when `setup_complete`
is true, else from `addon_root` (existing behavior).

## Closure algorithm

Given a preset, the **seed set** `S` is the union of:
- Every `packages.id` where `packages.creator` ∈ this preset's creator seeds.
- Every `package_id` in this preset's package seeds.

The closure `C` is the smallest set such that:
- `S ⊆ C`
- For every `p ∈ C`, every locally-resolved dep of `p` is in `C`.

Iterative BFS in SQL, with the seed set itself computed inline:

```sql
WITH RECURSIVE
seeds(id) AS (
    -- author-seeded packages (resolved fresh every closure compute)
    SELECT p.id
      FROM packages p
      JOIN visibility_preset_creators c
        ON c.creator = p.creator
     WHERE c.preset_id = ?1
       AND p.is_hidden = 0
    UNION
    -- explicitly-picked packages
    SELECT package_id
      FROM visibility_preset_packages
     WHERE preset_id = ?1
),
closure(id) AS (
    SELECT id FROM seeds
    UNION
    SELECT l.dst_package_id
      FROM package_dep_links l
      JOIN closure cl ON cl.id = l.src_package_id
     WHERE l.dst_package_id IS NOT NULL
)
SELECT * FROM closure;
```

The author-seed branch reads `packages` fresh, so new .var files by a
seeded creator (added since the last apply) automatically participate in
the next closure. This is the "set and forget" behavior — once you've
seeded an author, future packages from them join the active set on the
next Apply without re-touching the preset.

`is_hidden = 0` excludes UI-hidden packages from auto-inclusion via author
seeding. Hand-picked package seeds bypass this — if the user explicitly
asked for it, we include it.

Missing-dep handling: `package_dep_links.dst_package_id IS NULL` means the
seed (or one of its transitive deps) references a package the user doesn't
own. We **don't fail** the preset — just surface the unresolved
`raw_dep_key`s in the dry-run output so the user knows what's broken.

Cycle handling: `WITH RECURSIVE … UNION` (not `UNION ALL`) terminates because
the working set monotonically grows. Cycles in well-formed VaM packages are
rare but possible; this query handles them implicitly.

## Materialization strategy

**Hardlinks only.** `std::fs::hard_link(<managed_root>/<basename>, <addon_root>/<basename>)`.
No copy fallback, no symlinks. Rationale:

- Hardlinks create a directory entry pointing at the existing file extents.
  Zero byte transfer, ~no SSD write beyond MFT metadata, no admin rights
  required, indistinguishable from a regular file to VaM's zip reader.
- Copies double disk usage and burn SSD writes for no benefit.
- Symlinks need admin / Developer Mode and add a reparse-point hop that
  some apps mishandle.

Same-volume enforcement happens at two points:

1. **Setup wizard** — probes the proposed managed path against `addon_root`
   before committing the migration. See "Setup wizard" above.
2. **Every Load/Unload** — re-checks the recorded `managed_volume_id`
   against the current volume serial. Drift (drive remap, USB eject)
   aborts before any FS write.

All entries in `active_folder_state` are by construction hardlinks; no
`method` column needed.

## Sync algorithm (Load / Unload)

Same diff logic whether the user is loading a preset, loading an ad-hoc
selection of creators, or unloading a subset.

```
target  := closure(seed_set)   -- creators ∪ explicit packages → closure
current := SELECT package_id FROM active_folder_state

add    := target - current     -- hardlink into addon_root
remove := current - target     -- unlink from addon_root
keep   := target ∩ current     -- verify still present
```

For each `add`:
  - Source = `<managed_root>/<basename(packages.var_path)>`.
  - Dest   = `<addon_root>/<basename(packages.var_path)>`.
  - Refuse if dest exists and isn't in `active_folder_state` (something
    else wrote there — bail rather than overwrite).
  - `std::fs::hard_link(source, dest)`. On error, surface and skip; do not
    try to "make it work" with a copy fallback.

For each `remove`:
  - Verify the file at `active_path` still matches what we recorded
    (size + inode-id via `GetFileInformationByHandle`) before unlinking.
    Skip with a warning if not — the user may have moved/edited it.
  - `std::fs::remove_file`; clear the `active_folder_state` row.

For each `keep`:
  - Stat the active path; if it's missing or its inode no longer matches
    the managed file's, drop the row from `active_folder_state` and
    reclassify as `add` for re-hardlinking.

**"Unload" as a first-class operation.** Unload is a sync where the
target seed set is the *current* seed set minus what the user wants to
remove. The diff falls out naturally — `target - current` is empty,
`current - target` is the unload set. Same code path, different UI label.

Atomicity: not transactional across the filesystem. Sync in two phases:
1. Stage all adds into `<addon_root>/.vam-pb-staging/`, hardlink there first.
2. On success, `fs::rename` each into place (O(1), same volume),
   then process removes.
3. On any failure mid-way, clean `.vam-pb-staging/` and leave the active
   folder in whatever partial state it had reached. DB state is updated
   only for files that actually made it into place.

Not crash-proof, but resumable: re-running converges. Recovery story is
"just hit Load again."

## Dry-run preview

Before any disk I/O, compute and surface to the UI:

- `seed_breakdown` — N packages from author seeds (with the creator list),
  M from explicit package seeds. Lets the user verify the per-author flow
  pulled in what they expected.
- `keep_count` — packages already correctly materialized.
- `add_count`, `add_size_bytes` — new hardlinks (size shown so the user
  can sanity-check totals; disk overhead itself is ~0).
- `remove_count` — packages to unlink (with a short list of names so the
  user can spot accidents like "wait, that's my whole Looks folder").
- `unresolved_deps` — list of `raw_dep_key` strings that couldn't be
  resolved in the closure, with the seed package that pulled them in.
- `volume_check` — same-volume probe result. Should always be green at
  this point (config blocks otherwise), but worth showing as reassurance
  before a destructive op.

The user clicks "Apply" or "Cancel". No other code path materializes.

## VaM hot-reload behavior (researched, 2026-05)

The user asked whether VaM can pick up active-folder changes without a
restart. Short answer: **additive hot-reload yes (documented), full
hot-swap no (undocumented, likely blocked by file locks).**

### What's confirmed

- VaM has a **"Rescan Add-on Packages"** action: Main UI → File
  (Open/Save) → *Rescan Add-on Packages*. Also surfaced as
  **"Rescan Packages"** at the top of the Package Manager. Documented
  use case: "If you had VaM running while you downloaded the VAR
  package, you need to press 'Rescan Packages' at the top." Sources:
  [aqxaromods VaM install guide](https://aqxaromods.com/virt-a-mate/guides-vam/10260-virt-a-mate-installing-var-packages.html),
  multiple community guides.
- VaM 1.21 ships a two-step startup scan (referenced .var files first,
  then async background scan of the rest) — explicit evidence that
  MeshedVR considers per-package overhead during scan to be a real
  cost. Source: [VaM 1.21 release notes](https://www.patreon.com/posts/vam-1-21-77428912).
- Third-party performance plugin **VaM_PerformancePlugin** patches
  hot paths "for large numbers of files" via HarmonyX, again confirming
  the load-time pain scales with package count. Source:
  [Playable2030/VaM_PerformancePlugin](https://github.com/Playable2030/VaM_PerformancePlugin).

### What's not documented (so we treat as unsafe)

- **Behavior when a `.var` is *removed* while VaM is running.** The
  rescan documentation only covers the additive case. No official
  source describes how VaM handles a `.var` disappearing from
  AddonPackages — particularly one that's referenced by a loaded
  scene or by another loaded package's deps. Most likely behavior:
  the missing reference surfaces in the standard "missing
  dependencies" error path (which the user-facing
  [VaM-X errors guide](https://vam-x.com/guide/fix-errors-virt-a-mate/)
  treats as routine), but anything from a degraded scene to a hard
  crash is possible if a critical type is mid-load.
- **File-handle locks.** If VaM has read handles open on a `.var`
  (e.g. for a loaded scene's textures), Windows will reject our
  `remove_file` with `ERROR_SHARING_VIOLATION (32)` unless VaM
  opened the file with `FILE_SHARE_DELETE` — uncommon in Unity-based
  apps. **Unlinking a hardlink while VaM holds a handle on the file
  will fail.** This is a hard OS-level wall, not a VaM choice.
- **Third-party tool guidance.** None of the major library tools
  ([varbsorb](https://github.com/acidbubbles/vam-varbsorb),
  [VamToolbox](https://github.com/Kruk2/VamToolbox),
  [iHV](https://github.com/BoominBobbyBo/iHV),
  [vam-party](https://github.com/vam-community/vam-party))
  document a "close VaM first" requirement, but none explicitly say
  it's safe with VaM open either. The silence is telling: the
  community treats library mutation as a between-sessions operation.

### Operational assumption for this feature

**Default workflow: user closes VaM → applies a preset → relaunches VaM.**

The UI must make this loud:

1. The Apply confirmation modal includes a "Close VaM before applying"
   checklist item. The user has to acknowledge it.
2. Before any FS write, we probe a small set of recorded
   `active_folder_state` paths for an open delete-blocking handle (try
   `OpenOptions::new().read(true).write(true).open()` and observe the
   error — `os error 32` means VaM still has it). If any are locked,
   refuse to start and tell the user.
3. After Apply succeeds, surface "Launch VaM" affordance (just
   `start "" <vam.exe>` for users who want it; optional, not
   automatic).

### Possible optimization: additive-only fast path

If Apply's diff is **add-only** (`remove_count == 0`, e.g. extending
an active preset with another creator), we can offer the user a
"Don't restart VaM — use Rescan Packages instead" path. This is the
documented happy case. The UI surface:

- Detect add-only diff at preview time.
- Show: *"This change only adds packages. After Apply, click 'Rescan
  Packages' in VaM's Package Manager — no restart needed."*
- Still require restart for any diff with removes.

Defer this to a polish phase; MVP just assumes restart.

### What we are NOT going to attempt

- **Automating the Rescan click from outside VaM.** No supported IPC.
- **Force-closing file handles.** Calling `NtClose`/`MoveFileEx` with
  `MOVEFILE_DELAY_UNTIL_REBOOT` on locked files is the kind of clever
  that turns into corruption. Don't.
- **Pretending hot-swap works.** If a user gets confused and applies
  a remove-bearing diff with VaM running, they'll get a clear error
  about locked files; we don't paper over it with a "it'll be fine"
  message.

## Tauri commands (new)

```rust
// Setup wizard
get_setup_state() -> SetupState               // setup_complete?, current paths
probe_managed_path(path: String) -> ManagedProbeResult
                                              // same-volume? ntfs? empty?
                                              // writable? full diagnostic
                                              // bundle for the UI
begin_migration(managed_path: String) -> ()   // validates again + persists
                                              // managed_root, kicks off
                                              // background migration task
get_migration_status() -> MigrationStatus     // for resume-detection
                                              // and polling
cancel_migration() -> ()                      // cooperative — checkpoint
                                              // at next batch boundary

// Closure (no presets required — works on ad-hoc seed sets too)
compute_closure(seeds: SeedSpec) -> ClosurePreview
                                              // SeedSpec = { creators, package_ids }
compute_preset_closure(preset_id: i64) -> ClosurePreview

// Load / Unload (the everyday operation)
load(seeds: SeedSpec) -> LoadResult           // diff vs current, sync
unload(seeds: SeedSpec) -> LoadResult         // removes the named seeds'
                                              // contribution from active
unload_all() -> LoadResult                    // empties active folder
                                              // (everything in active_folder_state)
get_active_state() -> ActiveState             // what's loaded right now

// Preset CRUD (optional save/recall layer over seed sets)
list_presets() -> Vec<PresetSummary>
create_preset(name: String, seeds: SeedSpec) -> i64
update_preset(id: i64, name: String, seeds: SeedSpec) -> ()
delete_preset(id: i64)
load_preset(preset_id: i64) -> LoadResult     // applies preset's seeds as
                                              // the new active set

// Health
verify_active_folder() -> VerifyResult        // walks active_folder_state,
                                              // reports stale entries
```

`SeedSpec`, `SetupState`, `ManagedProbeResult`, `MigrationStatus`,
`ClosurePreview`, `LoadResult`, `ActiveState`, `PresetSummary`,
`VerifyResult` are new structs in
[src-tauri/src/commands.rs](src-tauri/src/commands.rs).

Frontend wrappers go in [src/lib/api.ts](src/lib/api.ts).

## UI surfaces

Two distinct surfaces, each with a single job. They share no controls;
the user never sees Load/Unload mixed with path configuration.

---

### Surface 1: Setup wizard (one-time)

Only shown until `setup_complete = "1"`. Path: gated entry from any
attempt to use Load/Unload before setup, or via a "Set up library
management" link in Settings.

#### Layout

A single-screen wizard, no steps:

```
┌─────────────────────────────────────────────────────────────┐
│ Set up package library management                           │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ⓘ This is a one-time setup. It moves your existing .var   │
│    files to a managed library folder. VaM will keep         │
│    reading from the same AddonPackages path it always has.  │
│                                                             │
│  AddonPackages (where VaM reads — must be on same drive):  │
│  ┌─────────────────────────────────────────────────────┐    │
│  │ D:\Games\VAM\AddonPackages                          │    │
│  └─────────────────────────────────────────────────────┘    │
│  (set in scanner settings; this field is read-only here)    │
│                                                             │
│  Managed library (where real files will live):              │
│  ┌─────────────────────────────────────────────────────┐ 📁 │
│  │ D:\Games\VAM\AddonPackages_Managed                  │    │
│  └─────────────────────────────────────────────────────┘    │
│  🟢 Same volume (D:), NTFS, empty — ready                  │
│                                                             │
│  Files to move: 3,824 .var (247.3 GB)                       │
│  Estimated time: ~8 seconds (same-volume rename, no copy)   │
│                                                             │
│  ⚠ Close VaM before continuing.                            │
│                                                             │
│         [ Cancel ]                  [ Start migration ▶ ]   │
└─────────────────────────────────────────────────────────────┘
```

Status indicator under the managed-path field reflects the probe result
in real time as the user edits:

- 🟢 *Same volume, NTFS, empty* — Start enabled.
- 🟡 *Folder doesn't exist yet* — show "Will be created" + Start
       enabled (probe runs against the parent dir to confirm volume).
- 🟡 *Folder exists and is empty* — Start enabled.
- 🔴 *Folder exists and is NOT empty* — Start disabled, message:
       *"Managed library folder must be empty. Pick a different folder
       or empty this one first."*
- 🔴 *Different volume than AddonPackages* — Start disabled, message
       quotes both drive letters/serials.
- 🔴 *Non-NTFS / network / unwritable* — Start disabled, exact OS error.

#### Migration in progress

Same screen, controls swap to:

```
  Migrating package library…
  ┌─────────────────────────────────────────────────────┐
  │ ████████████████████░░░░░░░░░░  2,612 / 3,824       │
  └─────────────────────────────────────────────────────┘
  Currently moving: AcidBubbles.Timeline.289.var
  Errors: 0

                          [ Cancel ]
```

Cancel is cooperative — finishes the in-flight batch and stops cleanly.
Partial migrations are resumable on relaunch.

#### Post-migration confirmation

```
  ✓ Migration complete.
  Moved 3,824 .var files in 7.4 seconds. 0 errors.
  
  Next: pick what to load.        [ Go to Load/Unload → ]
```

---

### Surface 2: Load/Unload (everyday)

Only available post-setup. Replaces the previously-named "Presets" panel.

This is **separate from setup** — it owns *what's in the active folder*,
not *where the folders are*. The user lives here day to day.

#### Per-author flow (primary)

The dominant interaction.

```
┌─────────────────────────────────────────────────────────────┐
│ Load / Unload packages                                      │
├─────────────────────────────────────────────────────────────┤
│ Currently loaded: 247 packages (43 authors). [details ▼]    │
│                                                             │
│ Authors                                       [ ☐ select │
│ all loaded ]                                                │
│ ┌─────────────────────────────────────────────────────┐    │
│ │ 🔍 search authors…                                  │    │
│ ├─────────────────────────────────────────────────────┤    │
│ │ ☑ AcidBubbles            142 pkgs    ● loaded       │    │
│ │ ☑ MeshedVR                89 pkgs    ● loaded       │    │
│ │ ☐ Hunting-Succubus       312 pkgs                   │    │
│ │ ☐ VAMDeluxe               58 pkgs                   │    │
│ │ ☑ Stopper                 24 pkgs    ● loaded       │    │
│ │ …                                                   │    │
│ └─────────────────────────────────────────────────────┘    │
│                                                             │
│ Selection closure preview:                                  │
│ ┌─────────────────────────────────────────────────────┐    │
│ │  255 packages will be in active folder              │    │
│ │   ↳ 192 from selected authors                       │    │
│ │   ↳  63 added by dependency closure                 │    │
│ │   ↳   4 unresolved deps (not installed) [view ▼]    │    │
│ └─────────────────────────────────────────────────────┘    │
│                                                             │
│ Diff vs currently loaded:                                   │
│   + 12 add     − 4 remove     = 239 keep                    │
│                                                             │
│   ☐ I have closed VaM.                                      │
│                                                             │
│        [ Preview details ]     [ Apply changes ▶ ]          │
└─────────────────────────────────────────────────────────────┘
```

Key behaviors:

1. **Author list = `list_creators_with_counts`** reused from existing
   code at [src-tauri/src/commands.rs:282](src-tauri/src/commands.rs).
   Status dot reflects whether that creator is in the current loaded
   seed set.
2. **Live closure tally** debounce-recomputed on every checkbox change.
3. **Diff vs current** computed via `add/remove/keep` against
   `active_folder_state`. The "closed VaM" checkbox is required only
   when `remove > 0` (the add-only fast path doesn't need a restart;
   see "VaM hot-reload behavior").
4. **Apply changes** runs `load` with the new SeedSpec. If the diff is
   add-only and VaM is detected as running, surface the
   *"click Rescan Packages in VaM"* hint after.

#### Per-package flow (secondary)

Multi-select on the main grid → context menu *"Add to current selection"*
or *"Remove from current selection"*. The seed set tracks both creators
and individual packages; the closure unions them.

#### Presets (optional save/recall)

Sidebar of named presets. Each preset is just a saved `SeedSpec`. Clicking
*Load preset* sets the current seed set to that preset's spec, recomputes
the closure, runs the diff. Equivalent to manually selecting the same
authors/packages but persisted.

Affordances:
- *"Save current selection as preset"* — captures the working SeedSpec.
- *"Make preset from favorites"* — copies `is_favorite = 1` package set.
- Per-preset Edit / Rename / Delete.

#### Unload UI

Same screen, with an *Unload all* button that empties the active folder
(removes every entry in `active_folder_state`). For targeted unloads,
the user uncheckes authors / removes packages from the current seed set
and clicks Apply — the diff naturally surfaces the removes.

#### Health / housekeeping

A small *"Verify integrity"* link runs `verify_active_folder`. Reports:
hardlinks present and intact, missing entries (re-linkable), orphaned
files in active folder (not in our state — leave alone unless user
explicitly Clears).

`is_favorite` and `is_hidden` keep their existing UI-only semantics.
They do not auto-participate in Load unless the user explicitly bridges
("Make preset from favorites").

## Open design questions

1. **Default managed folder name.** Plan defaults to
   `AddonPackages_Managed` (per user direction). Alternatives discarded:
   `AddonPackages.library`, `.AddonPackagesLibrary` (hidden), `VaM-Library`.
   The `_Managed` suffix is explicit about ownership and visible enough
   that the user remembers what it is.

2. **Out-of-band drops into AddonPackages post-setup.** The user (or
   the VaM Hub installer) may drop new `.var` files directly into
   `addon_root`. The plan handles this with a scanner banner offering
   to migrate them into `managed_root`. Open: should we auto-migrate
   silently, or always ask? Lean: always ask (one extra click; avoids
   surprise).

3. **New packages by a seeded author.** If "MeshedVR" is in the active
   seed set and the user adds a new `MeshedVR.*.var` to managed_root,
   those join the next closure on the next Load. Default to auto-include;
   document clearly.

4. **What about non-.var content in master?** VaM's install also has
   `Custom/`, `Saves/`, scripts. Out of scope — this feature is
   `.var`-only. Document that loose content lives elsewhere and isn't
   affected.

5. **Hub-pinned packages.** Should "anything with `hub_billing_tier =
   'paid'`" be auto-included in every closure? Probably not — overreach.
   But the dependency resolver should surface "this paid package is
   referenced but you don't own it" clearly.

6. **Mtime / size verification.** For `keep`: is `file_size` enough or
   do we need a hash? Hashes are expensive every Load; size + inode-id
   (`nFileIndexHigh/Low` from `GetFileInformationByHandle`) is probably
   fine. The managed folder is read-only, so the source can't have
   shifted under us between scans.

7. **Load during scanner.** Scanner walks `managed_root` (read-only);
   Load writes to `addon_root`. No FS collision. But both are DB
   writers — must coordinate via the
   [Database access protocol](CLAUDE.md) lock file.

8. **Setup reversal.** Out of scope for v1; documented manual recipe
   only. Worth revisiting if users actually ask.

9. **Preset import/export.** JSON dump of `SeedSpec`. Defer; not MVP.

10. **Creator-name canonicalization.** Some packages have whitespace,
    case, or punctuation variants of the same author across releases.
    Today we match `creator` exactly. Should the author seed do
    case-insensitive matching? Lean yes — mirror
    `list_creators_with_counts` which uses `COLLATE NOCASE`.

11. **CLAUDE.md invariant update.** The "AddonPackages is read-only"
    statement in CLAUDE.md needs a footnote post-setup. When do we
    update it — in the migration commit, or in a separate doc commit
    after the feature is verified? Lean: separate doc commit so the
    invariant change is reviewable independently.

## Suggested phasing

**Phase 1 — backend data + closure (no UI).**
- Migration v16 (the four tables above).
- Volume-serial helpers (`GetVolumeInformationW` etc.) via the `windows`
  crate.
- `compute_closure` + the recursive CTE (works on `SeedSpec`, no preset
  required).
- Unit tests for closure: author-only seeds, package-only seeds, mixed
  seeds, chained deps, missing deps, cycles, duplicates, the
  "new package by seeded author auto-joins" case. Pattern after the
  existing `deps.rs` tests.

**Phase 2 — setup wizard + migration (gates everything).**
- `get_setup_state`, `probe_managed_path`, `begin_migration`,
  `get_migration_status`, `cancel_migration` commands.
- One-time-migration algorithm with batched DB-transactional moves.
- Resume detection on relaunch.
- Setup wizard UI (Surface 1 above).
- CLI binary `src-tauri/src/bin/setup_tool.rs` with `--probe`,
  `--begin`, `--status` for headless testing before UI work.
- Scanner gains the post-setup branch (walks `managed_root` when
  `setup_complete = 1`; legacy behavior otherwise).
- Tests: dry-run migration on a small synthetic library; resume from
  simulated mid-transaction crash; refuse on non-empty managed folder;
  refuse on cross-volume.

**Phase 3 — Load / Unload materialization.**
- The sync algorithm: stage → rename → diff. Hardlink-only.
- `active_folder_state` upkeep + verification on `keep`/`remove`.
- `load`, `unload`, `unload_all`, `verify_active_folder` Tauri commands.
- Test against the post-migration state from Phase 2.

**Phase 4 — Load/Unload UI (Surface 2).**
- Per-author picker (headline flow) with live closure tally.
- Per-package "Add to current selection" on the main grid.
- Diff preview, Apply button, VaM-closed checklist.
- Add-only fast-path detection + Rescan-Packages hint copy.

**Phase 5 — Presets (optional save/recall).**
- Preset CRUD on top of the SeedSpec foundation.
- Preset sidebar + load-preset action.
- "Make preset from favorites" affordance.

**Phase 6 — polish (deferred).**
- Multi-preset stacking, hub-paid warnings, import/export,
  "pending new packages" UX (open question #3).
- CLAUDE.md update reflecting the inverted invariant.

## Definition of done

### Setup (Phase 1–2)

- Migration v16 lands; the four new tables exist with no data on first
  upgrade.
- The setup wizard refuses managed paths that are on a different volume,
  non-empty, non-NTFS, or under `addon_root`. Each refusal surfaces the
  exact diagnostic.
- Running setup with VaM open is detected and refused.
- A successful migration moves every `.var` from `addon_root` →
  `managed_root` and updates `packages.var_path` for every row. Verifiable
  by counting files in each folder before/after and inspecting DB.
- An interrupted migration is detected on relaunch and resumable from
  the next un-moved row.
- Post-setup, the scanner walks `managed_root`. `addon_root` is empty
  (no real `.var`s) immediately after migration completes.

### Load/Unload (Phase 3–4)

- A user can select one creator, see a live closure tally, click Apply,
  and find that creator's packages + dep closure hardlinked into
  `addon_root`.
- Multi-creator selection unions correctly.
- A new `.var` from a selected creator (added to `managed_root` after
  the last Load) joins the active set on the next Load without re-touching
  the seed set.
- Per-package add works: explicitly-added packages get included in the
  closure even if their creator isn't in the seed set.
- `Unload all` removes only the files this tool placed; any non-tool
  files in `addon_root` are left alone with a banner.
- Re-Loading the same SeedSpec is a no-op (`add_count == 0`,
  `remove_count == 0`).
- Switching seed sets diffs correctly — only changed packages flip,
  no churn on stable ones.
- Every entry in `active_folder_state` is a real NTFS hardlink pointing
  at the same inode as the matching file in `managed_root` (verifiable
  by `GetFileInformationByHandle` returning matching `nFileIndexHigh/Low`).
- `managed_root` is untouched by Load/Unload — verify by hashing its
  contents before/after a full Apply cycle.
- Unresolved deps in a closure surface as visible warnings, not silent
  drops.

### VaM compat

- VaM launched after setup reads from `addon_root` as before, with no
  reconfiguration needed by the user.
- VaM sees hardlinked `.var`s as ordinary files (load/import succeed
  identically).
- Loading with VaM closed succeeds; Loading with VaM running and a
  remove-bearing diff fails with a clear "close VaM" error rather than
  partial corruption.

## Risks / things to be paranoid about

### Setup-specific

- **Migration interrupted with inconsistent DB.** `fs::rename` is per-file
  atomic, but the DB transaction batching has to keep `packages.var_path`
  in sync with what's actually on disk. Use small batches (~500 rows)
  with `COMMIT` between batches, so a crash leaves at most one batch in
  an in-doubt state — recoverable on relaunch by walking both folders
  and reconciling.
- **Managed folder gets content between probe and migration start.** If
  another process drops a file into the managed folder between
  "empty?" check and `rename`, our first `rename` could collide.
  Mitigation: re-check empty inside the migration transaction, and
  fail-fast on collision.
- **VaM launches during migration.** A user clicks our Start button
  then alt-tabs and launches VaM via shortcut. The first `rename`
  on a .var VaM is loading fails with `ERROR_SHARING_VIOLATION`.
  Mitigation: lock-handle probe at start AND check again per batch;
  abort cleanly if VaM appears mid-migration.
- **User picks managed_root inside `addon_root` despite validation.**
  Symlinks/junctions could confuse the check. Canonicalize both paths
  before comparing.

### Load/Unload-specific

- **Writing to managed_root by accident.** Every FS write path takes
  `addon_root` as input and asserts the destination is canonicalized
  inside `addon_root` and not under `managed_root`.
  `assert!(canonical(dest).starts_with(canonical(addon_root)))`.
- **Active folder pointing somewhere catastrophic.** If somehow
  `addon_root` got set to `C:\Windows\System32` and a user clicks
  *Unload all*, we must not iterate the directory. The
  `active_folder_state` table is the authoritative list — Unload
  iterates *only* those entries.
- **Cross-volume hardlink at Load time.** Drive remap or USB-eject
  could change the volume serial between setup commit and Load. The
  recorded `managed_volume_id` is re-checked on every Load; mismatch
  aborts before any FS write.
- **Hardlink count limits.** NTFS allows up to 1023 hardlinks per file.
  We're at 2 (1 managed + 1 active). Way under.
- **Antivirus interference.** Real-time scanners sometimes lock newly
  created files for a moment. Stage-then-rename + retry-once on
  `ERROR_SHARING_VIOLATION` (32) is the standard mitigation.
- **VaM running during Load.** If VaM has handles on `.var`s in
  `addon_root` we're trying to unlink, Windows blocks us. Detect
  the failure and surface "close VaM and retry." See the
  "VaM hot-reload behavior" section: VaM closed during Load is the
  default assumption except for the add-only fast path.
- **A user-deleted hardlink "going stale."** If the user manually
  deletes a hardlink from `addon_root`, the corresponding
  `active_folder_state` row becomes a phantom. Next Load's `keep`
  check stat-fails → reclassify as `add`. Idempotent recovery.
- **Lock collision with the scanner.** Both Load and the scanner are
  DB writers — must check the session-active lock per the
  [Database access protocol in CLAUDE.md](CLAUDE.md).

## Pointers

- Dep resolver: [src-tauri/src/deps.rs](src-tauri/src/deps.rs) (closure
  starts from `package_dep_links`).
- Relationships command pattern: `get_package_relationships` in
  [src-tauri/src/commands.rs:2174](src-tauri/src/commands.rs).
- Migrations: [src-tauri/src/index.rs](src-tauri/src/index.rs) (v16
  next; currently at v15).
- `is_favorite`/`is_hidden` flags (UI-only — separate concept from
  presets): see `set_favorite`, `set_hidden` in
  [src-tauri/src/commands.rs](src-tauri/src/commands.rs).
- Existing settings I/O: `get_setting` / `set_setting` in
  [src-tauri/src/index.rs](src-tauri/src/index.rs) — reuse for the new
  `managed_root` / `managed_volume_id` / `setup_complete` keys.
- Project conventions: [CLAUDE.md](CLAUDE.md) (PowerShell quirks, MSVC
  linker via `scripts\dev-env.cmd`, read-only invariant — **note: this
  feature changes the invariant location post-setup, see "Architecture"
  section above**, DB-write lock convention).
- Archived antecedent (dep graph milestone, now shipped):
  [docs/archive/TODO-dependency-viewer.md](docs/archive/TODO-dependency-viewer.md).
