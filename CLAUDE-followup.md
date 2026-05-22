# CLAUDE.md follow-ups — pending merge

Things that should land in [CLAUDE.md](CLAUDE.md) but aren't appropriate
to merge in *this* branch because they depend on downstream feature work
landing first. Update CLAUDE.md when the gated milestone ships.

---

## 1. Read-only-invariant inversion after Visibility-Presets setup

**Gating milestone:** Phases 2–4 of [TODO-visibility-presets.md](TODO-visibility-presets.md)
land (setup wizard + migration + Load/Unload).

**Current CLAUDE.md text** (in the "Read-only invariant (critical)" section):

> The user's `.var` library — by default `D:\Games\VAM\AddonPackages` —
> is **strictly read-only**.

**Why this needs updating:** the Visibility-Presets feature inverts the
naming of the read-only path. After a user runs Setup (a one-time
migration), all real `.var` files live in `D:\Games\VAM\AddonPackages_Managed`
(or wherever they picked) and `D:\Games\VAM\AddonPackages` itself
becomes the *active folder* — full of hardlinks placed by this tool,
freely populated and cleared. The read-only invariant moves with the
real bytes.

**Proposed replacement** (for the CLAUDE.md commit that ships with the
feature):

> The user's `.var` library is **strictly read-only**.
>
> - **Before Visibility-Presets setup** (legacy / new installs):
>   the library lives at `addon_root` (default
>   `D:\Games\VAM\AddonPackages`). Read-only invariant applies here.
> - **After Visibility-Presets setup**: the library has been moved to
>   `managed_root` (default `D:\Games\VAM\AddonPackages_Managed`).
>   Read-only invariant moves with it. `addon_root` is now the active
>   folder — managed by the tool, freely populated and cleared via
>   hardlinks.
>
> The Visibility-Presets setup wizard is the *only* code path
> permitted to mutate the library (one-time migration). Every other
> code path that handles `.var` files must respect the read-only
> invariant on whichever folder currently holds them, derived from
> the `setup_complete` setting.

Also: the example path snippet under "Multi-session coordination →
Worktree convention" doesn't need updating — it's about worktree
locations, not VaM library paths.

---

## 2. Migration v20 → v21 was claimed during merge with main

**Gating milestone:** N/A (resolved; informational).

Originally this branch claimed v15 → v16 for the visibility tables.
Main meanwhile shipped v15 → v16 for `predicted_hub_category` and
v16 → v20 for the override system. On merge, the visibility migration
was renumbered to **v20 → v21**. The tables themselves are unchanged
— self-contained, no FK refs to other migrations' columns — so the
schema converges regardless of order.

If another parallel session is planning a v21+ schema change, they
need to bump and rebase. No CLAUDE.md edit needed.

---

## 3. New module + Cargo.toml change pending

**Gating milestone:** Phase 2 of [TODO-visibility-presets.md](TODO-visibility-presets.md).

Phase 2 (setup wizard backend) needs Windows volume-serial helpers
(`GetVolumePathNameW` / `GetVolumeInformationW`) for the same-volume
probe. Implementation options:

- Add the `windows` crate to `src-tauri/Cargo.toml` (touches the
  lockfile — coordinate per CLAUDE.md's "only one session at a time
  may modify `package-lock.json` or `src-tauri/Cargo.lock`" rule).
- Or hand-roll FFI via `extern "system"` declarations against
  `kernel32.dll` (no new dep, fragile if MSVC import lib changes).

Recommendation: add `windows = { version = "...", features = ["Win32_Storage_FileSystem"] }`
when Phase 2 starts, in a dedicated commit, after coordinating with
any concurrent session.

No CLAUDE.md edit needed; the toolchain-quirks section is already
clear about MSVC linker + cargo PATH requirements.

---

## Maintenance

When you ship the gated milestone, fold the relevant section above
into CLAUDE.md and delete it from this file. When this file is empty,
delete it.
