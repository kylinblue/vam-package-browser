//! Visibility-presets setup wizard backend.
//!
//! Owns the one-time migration that moves the user's `.var` files from
//! VaM's AddonPackages (the active folder going forward) into a sibling
//! managed library folder. Post-migration, the scanner reads from
//! `managed_root`; Load/Unload writes hardlinks into `addon_root`.
//!
//! See TODO-visibility-presets.md for the full design rationale,
//! particularly the "Setup wizard — one-time migration" section.

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::fsutil;

// --- settings keys ----------------------------------------------------------

/// Where the real `.var` files live post-setup. `app_settings.value` is
/// an absolute path. Unset while pre-setup.
pub const SETTING_MANAGED_ROOT: &str = "managed_root";

/// NTFS volume serial of `managed_root` captured at setup commit.
/// Re-checked on every Load to detect drive remaps / USB ejects before
/// any FS write.
pub const SETTING_MANAGED_VOLUME_ID: &str = "managed_volume_id";

/// `"1"` once the one-time migration finished. While `"0"` (or unset),
/// the scanner uses `addon_root` (legacy behavior).
pub const SETTING_SETUP_COMPLETE: &str = "setup_complete";

/// Unix seconds when setup completed. Informational.
pub const SETTING_SETUP_COMPLETED_AT: &str = "setup_completed_at";

// --- public types -----------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct SetupState {
    pub setup_complete: bool,
    pub addon_root: Option<String>,
    pub managed_root: Option<String>,
    pub managed_volume_id: Option<u32>,
    pub setup_completed_at: Option<i64>,
    /// True when `setup_complete = 0` but `managed_root` is set AND some
    /// `packages.var_path` rows already point inside `managed_root`.
    /// Means a previous migration was interrupted; the UI should offer
    /// to resume rather than start fresh.
    pub migration_in_progress: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeResult {
    pub addon_root: String,
    pub managed_root: String,
    /// True if every check passed and `begin_migration` would proceed.
    pub ok: bool,
    /// Detailed per-check status. Order is the validation order from the
    /// plan; first failure in the list is the most likely "fix me first".
    pub checks: Vec<ProbeCheck>,
    /// First failed check's message, hoisted for easy UI binding.
    pub diagnostic: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProbeCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationProgress {
    pub moved: i64,
    pub total: i64,
    pub current: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationResult {
    pub moved: i64,
    pub leftover_moved: i64,
    pub errors: Vec<MigrationError>,
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationError {
    pub path: String,
    pub reason: String,
}

// --- public API -------------------------------------------------------------

/// Read the current setup state from the DB. Detects mid-flight
/// migration by looking for `packages.var_path` rows that point under
/// `managed_root` while `setup_complete` is still `0`.
pub fn get_setup_state(conn: &Connection) -> Result<SetupState> {
    use crate::index::get_setting;

    let setup_complete = get_setting(conn, SETTING_SETUP_COMPLETE)?
        .as_deref()
        .map(|v| v == "1")
        .unwrap_or(false);
    let addon_root = get_setting(conn, "addon_root")?;
    let managed_root = get_setting(conn, SETTING_MANAGED_ROOT)?;
    let managed_volume_id = get_setting(conn, SETTING_MANAGED_VOLUME_ID)?
        .and_then(|v| v.parse::<u32>().ok());
    let setup_completed_at = get_setting(conn, SETTING_SETUP_COMPLETED_AT)?
        .and_then(|v| v.parse::<i64>().ok());

    // Mid-flight detection: setup not complete, managed_root is set, AND
    // at least one package row already lives under managed_root.
    let migration_in_progress = match (&managed_root, setup_complete) {
        (Some(mr), false) => count_packages_under(conn, mr)? > 0,
        _ => false,
    };

    Ok(SetupState {
        setup_complete,
        addon_root,
        managed_root,
        managed_volume_id,
        setup_completed_at,
        migration_in_progress,
    })
}

/// Run every pre-commit validation against a proposed `managed_root`.
/// Returns a structured result so the UI can render per-check status.
/// `addon_root` is read from settings; caller must have it configured.
pub fn probe_managed_path(conn: &Connection, managed_root: &str) -> Result<ProbeResult> {
    use crate::index::get_setting;

    let addon_root = get_setting(conn, "addon_root")?
        .ok_or_else(|| anyhow!("addon_root not set; configure the scanner first"))?;
    let addon_root_path = PathBuf::from(&addon_root);
    let managed_path = PathBuf::from(managed_root);

    let mut checks: Vec<ProbeCheck> = Vec::new();
    let mut diagnostic: Option<String> = None;

    let push = |checks: &mut Vec<ProbeCheck>,
                diagnostic: &mut Option<String>,
                name: &'static str,
                ok: bool,
                detail: String| {
        if !ok && diagnostic.is_none() {
            *diagnostic = Some(detail.clone());
        }
        checks.push(ProbeCheck { name, ok, detail });
    };

    // 1. addon_root must exist.
    let addon_exists = addon_root_path.is_dir();
    push(
        &mut checks,
        &mut diagnostic,
        "addon_root_exists",
        addon_exists,
        if addon_exists {
            format!("AddonPackages found at {addon_root}")
        } else {
            format!("AddonPackages does not exist at {addon_root}")
        },
    );

    // 2. managed_root must NOT be equal to or nested under addon_root.
    let nested = is_nested(&managed_path, &addon_root_path);
    push(
        &mut checks,
        &mut diagnostic,
        "managed_not_under_addon",
        !nested,
        if nested {
            format!(
                "Managed folder ({}) cannot be inside or equal to AddonPackages ({})",
                managed_path.display(),
                addon_root_path.display()
            )
        } else {
            "Managed folder is outside AddonPackages".into()
        },
    );

    // 3. managed_root: empty (or non-existent).
    let empty = if managed_path.exists() {
        fsutil::directory_is_empty(&managed_path).unwrap_or(false)
    } else {
        true
    };
    push(
        &mut checks,
        &mut diagnostic,
        "managed_empty",
        empty,
        if empty {
            "Managed folder is empty (or will be created)".into()
        } else {
            format!(
                "Managed folder {} must be empty before migration",
                managed_path.display()
            )
        },
    );

    // 4. same-volume probe. Skipped if either previous check failed
    // catastrophically (no addon_root, or nested) — would return
    // confusing errors.
    let mut same_vol = false;
    let mut vol_detail = String::from("(skipped — earlier check failed)");
    if addon_exists && !nested {
        // Need a probe target that exists. If managed doesn't, probe its
        // parent. If parent doesn't either, the path is unrecoverable.
        let probe_target = if managed_path.exists() {
            managed_path.clone()
        } else {
            managed_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| managed_path.clone())
        };
        match (
            fsutil::volume_info(&addon_root_path),
            fsutil::volume_info(&probe_target),
        ) {
            (Ok(va), Ok(vp)) => {
                same_vol = va.serial == vp.serial;
                vol_detail = if same_vol {
                    format!(
                        "Both on volume serial {:08X} ({})",
                        va.serial, va.filesystem
                    )
                } else {
                    format!(
                        "AddonPackages on serial {:08X} ({}); managed folder on serial {:08X} ({}). \
                         Hardlinks can't cross volumes.",
                        va.serial, va.filesystem, vp.serial, vp.filesystem
                    )
                };
            }
            (Err(e), _) => vol_detail = format!("AddonPackages volume query failed: {e:#}"),
            (_, Err(e)) => vol_detail = format!("Managed folder volume query failed: {e:#}"),
        }
    }
    push(
        &mut checks,
        &mut diagnostic,
        "same_volume",
        same_vol,
        vol_detail,
    );

    // 5. NTFS check. Reuses the filesystem name from step 4.
    let mut is_ntfs = false;
    let mut ntfs_detail = String::from("(skipped — same-volume check failed)");
    if same_vol {
        if let Ok(v) = fsutil::volume_info(&addon_root_path) {
            is_ntfs = v.is_ntfs();
            ntfs_detail = if is_ntfs {
                "Filesystem is NTFS".into()
            } else {
                format!(
                    "Filesystem is {} — hardlinks require NTFS",
                    v.filesystem
                )
            };
        }
    }
    push(
        &mut checks,
        &mut diagnostic,
        "ntfs",
        is_ntfs,
        ntfs_detail,
    );

    // 6. live hardlink probe. Last because it actually touches the FS.
    let mut hardlink_ok = false;
    let mut hardlink_detail = String::from("(skipped — earlier checks failed)");
    if empty && is_ntfs && addon_exists {
        // Need both endpoints to exist as directories for the probe.
        let probe_dest = if managed_path.exists() {
            managed_path.clone()
        } else {
            // Best-effort create for probe. Don't leave a stray dir if
            // the probe fails — we'll remove it.
            match std::fs::create_dir_all(&managed_path) {
                Ok(()) => managed_path.clone(),
                Err(e) => {
                    hardlink_detail = format!("Could not create managed folder for probe: {e}");
                    push(
                        &mut checks,
                        &mut diagnostic,
                        "hardlink_probe",
                        false,
                        hardlink_detail,
                    );
                    return Ok(ProbeResult {
                        addon_root,
                        managed_root: managed_root.to_string(),
                        ok: checks.iter().all(|c| c.ok),
                        checks,
                        diagnostic,
                    });
                }
            }
        };
        match fsutil::try_hardlink_probe(&addon_root_path, &probe_dest) {
            Ok(()) => {
                hardlink_ok = true;
                hardlink_detail = "Hardlink probe succeeded".into();
            }
            Err(e) => {
                hardlink_detail = format!("Hardlink probe failed: {e:#}");
            }
        }
        // If we auto-created the dir for the probe and it's still empty
        // (which it should be — probe cleans up), leave it. It's the
        // canonical managed_root location.
    }
    push(
        &mut checks,
        &mut diagnostic,
        "hardlink_probe",
        hardlink_ok,
        hardlink_detail,
    );

    let ok = checks.iter().all(|c| c.ok);
    Ok(ProbeResult {
        addon_root,
        managed_root: managed_root.to_string(),
        ok,
        checks,
        diagnostic,
    })
}

/// Execute the one-time migration: move every `.var` from `addon_root`
/// into `managed_root`, updating `packages.var_path` rows in lockstep.
/// Per-file idempotent — re-invocation after a crash resumes from
/// wherever the previous run left off.
///
/// `on_progress` fires after every batch (~500 files) so the UI can
/// render a progress bar. Caller is responsible for serializing this
/// with any other DB writer (per the CLAUDE.md DB-access protocol).
pub fn begin_migration(
    conn: &mut Connection,
    addon_root: &Path,
    managed_root: &Path,
    mut on_progress: impl FnMut(&MigrationProgress),
) -> Result<MigrationResult> {
    use crate::index::set_setting;

    let start = Instant::now();
    let mut errors: Vec<MigrationError> = Vec::new();

    // Pre-flight: managed_root must exist (create if needed) and be
    // hardlink-compatible. The probe already ran in the UI, but a
    // hostile environment could have changed state since then.
    std::fs::create_dir_all(managed_root)
        .with_context(|| format!("create managed_root {}", managed_root.display()))?;

    let addon_canonical = addon_root
        .canonicalize()
        .with_context(|| format!("canonicalize addon_root {}", addon_root.display()))?;
    let managed_canonical = managed_root
        .canonicalize()
        .with_context(|| format!("canonicalize managed_root {}", managed_root.display()))?;

    // Re-check same-volume right before we start touching files.
    if !fsutil::same_volume(&addon_canonical, &managed_canonical)? {
        return Err(anyhow!(
            "Refusing to migrate: addon_root and managed_root are no longer on the same volume"
        ));
    }

    // Persist managed_root immediately so a crash mid-migration is
    // detectable by `get_setup_state` (which flags
    // migration_in_progress when managed_root is set but
    // setup_complete is false).
    set_setting(conn, SETTING_MANAGED_ROOT, managed_root.to_string_lossy().as_ref())?;
    set_setting(conn, SETTING_SETUP_COMPLETE, "0")?;
    let volume_serial = fsutil::volume_info(&managed_canonical)?.serial;
    set_setting(
        conn,
        SETTING_MANAGED_VOLUME_ID,
        &volume_serial.to_string(),
    )?;

    // Collect rows to migrate. We snapshot up front so the
    // batched-commit loop has a stable view (and so we can report
    // accurate progress totals).
    let to_migrate: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, var_path FROM packages WHERE var_path <> '' ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    // Pre-flight: detect any pair of rows that would produce the SAME
    // destination path under managed_root after relative-path mapping.
    // Without this guard the second rename overwrites the first
    // (`fs::rename` on Windows replaces existing files), the user loses
    // data, and the DB update then collides on the var_path UNIQUE
    // constraint. Fail the whole migration cleanly before any FS write.
    {
        use std::collections::HashMap;
        let mut by_rel: HashMap<String, Vec<String>> = HashMap::new();
        for (_, old_path_str) in &to_migrate {
            let old_path = PathBuf::from(old_path_str);
            let rel = relative_under_addon_root(&old_path, addon_root);
            let rel_key = rel.to_string_lossy().to_lowercase();
            by_rel.entry(rel_key).or_default().push(old_path_str.clone());
        }
        let collisions: Vec<(String, Vec<String>)> = by_rel
            .into_iter()
            .filter(|(_, paths)| paths.len() > 1)
            .collect();
        if !collisions.is_empty() {
            let n = collisions.len();
            let preview: Vec<String> = collisions
                .iter()
                .take(5)
                .map(|(rel, paths)| {
                    let extras: Vec<String> = paths.iter().take(3).cloned().collect();
                    let more = if paths.len() > 3 {
                        format!(" (+{} more)", paths.len() - 3)
                    } else {
                        String::new()
                    };
                    format!("  {rel}\n    {}{}", extras.join("\n    "), more)
                })
                .collect();
            let more = if n > 5 {
                format!("\n  …and {} more collisions", n - 5)
            } else {
                String::new()
            };
            return Err(anyhow!(
                "{n} package(s) would collide on the same destination path in managed_root. \
                 The migration is aborted; no files have been moved. Resolve by \
                 renaming or removing duplicates from your library, then retry.\n\
                 Colliding groups (first 5 shown):\n{}{more}",
                preview.join("\n")
            ));
        }
    }

    let total = to_migrate.len() as i64;
    let mut moved: i64 = 0;
    let batch_size: usize = 500;

    for chunk in to_migrate.chunks(batch_size) {
        let tx = conn.transaction()?;
        {
            let mut upd = tx.prepare_cached(
                "UPDATE packages SET var_path = ?1 WHERE id = ?2",
            )?;
            for (id, old_path_str) in chunk {
                let old_path = PathBuf::from(old_path_str);
                let rel = relative_under_addon_root(&old_path, addon_root);
                if rel.as_os_str().is_empty() {
                    errors.push(MigrationError {
                        path: old_path_str.clone(),
                        reason: "could not derive relative path under addon_root".into(),
                    });
                    continue;
                }
                let new_path = managed_root.join(&rel);

                // Idempotent migration of one entry. Three cases:
                //   (a) entry at old_path, none at new_path → rename + update
                //   (b) entry at new_path (possibly also at old) → update only,
                //       BUT only if the two paths refer to the SAME NTFS file
                //       (same volume serial + same nFileIndex). If they're
                //       distinct files, record a collision and skip (this is
                //       belt-and-suspenders since the pre-flight should have
                //       caught it).
                //   (c) entry at neither → DB row points at nothing
                let old_exists = old_path.exists();
                let new_exists = new_path.exists();
                let new_path_str = new_path.to_string_lossy().to_string();

                if !old_exists && !new_exists {
                    errors.push(MigrationError {
                        path: old_path_str.clone(),
                        reason: "file missing at both old and new locations".into(),
                    });
                    continue;
                }
                if old_exists && new_exists && !same_file(&old_path, &new_path).unwrap_or(false) {
                    errors.push(MigrationError {
                        path: old_path_str.clone(),
                        reason: format!(
                            "collision: distinct files at both {} and {}",
                            old_path.display(),
                            new_path.display()
                        ),
                    });
                    continue;
                }
                if old_exists && !new_exists {
                    // Ensure the destination's parent directory tree exists.
                    if let Some(parent) = new_path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            errors.push(MigrationError {
                                path: old_path_str.clone(),
                                reason: format!(
                                    "create_dir_all({}): {e}",
                                    parent.display()
                                ),
                            });
                            continue;
                        }
                    }
                    if let Err(e) = std::fs::rename(&old_path, &new_path) {
                        errors.push(MigrationError {
                            path: old_path_str.clone(),
                            reason: format!("rename failed: {e}"),
                        });
                        continue;
                    }
                }
                // At this point new_path holds the file (either just renamed
                // or already there from a previous run). Update DB.
                upd.execute(params![new_path_str, id])?;
                moved += 1;
            }
        }
        tx.commit()?;
        on_progress(&MigrationProgress {
            moved,
            total,
            current: chunk
                .last()
                .map(|(_, p)| PathBuf::from(p).file_name().map(|s| s.to_string_lossy().to_string()))
                .flatten(),
        });
    }

    // Catch leftover `.var` files in addon_root that the DB didn't know
    // about (user added a .var since the last scan). Move them too —
    // they're still the user's content. A re-scan after migration will
    // pick them up in their new location.
    let leftover_moved = walk_and_move_leftovers(addon_root, managed_root, &mut errors)?;

    set_setting(conn, SETTING_SETUP_COMPLETE, "1")?;
    set_setting(
        conn,
        SETTING_SETUP_COMPLETED_AT,
        &unix_now().to_string(),
    )?;

    Ok(MigrationResult {
        moved,
        leftover_moved,
        errors,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

// --- internals --------------------------------------------------------------

fn count_packages_under(conn: &Connection, root: &str) -> Result<i64> {
    // LIKE-prefix match. Backslash literals are fine in SQLite (no escape
    // semantics on LIKE without ESCAPE clause). Use a trailing separator
    // to avoid `D:\AddonPackages_Managed_Old` matching when root is
    // `D:\AddonPackages_Managed`.
    let mut pattern = root.to_string();
    if !pattern.ends_with('\\') && !pattern.ends_with('/') {
        pattern.push('\\');
    }
    pattern.push('%');
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM packages WHERE var_path LIKE ?1",
        params![pattern],
        |r| r.get(0),
    )?;
    Ok(count)
}

/// True if `child` is equal to `parent` or sits under `parent`.
///
/// Comparison is lexical on lowercased / slash-normalized strings — robust
/// for non-existent child paths (where `canonicalize` would fail) and
/// matches Windows' case-insensitive path semantics. Inputs are expected
/// to be absolute (the setup UI uses a folder picker).
fn is_nested(child: &Path, parent: &Path) -> bool {
    let norm = |p: &Path| -> String {
        p.to_string_lossy().replace('/', "\\").to_lowercase()
    };
    let c = norm(child);
    let p = norm(parent);
    if c == p {
        return true;
    }
    // Treat parent as a directory prefix so C:\Foo doesn't match C:\FooBar.
    let mut p_dir = p;
    if !p_dir.ends_with('\\') {
        p_dir.push('\\');
    }
    c.starts_with(&p_dir)
}

/// Real NTFS file identity check. Two paths refer to the same underlying
/// inode iff they share volume serial AND nFileIndex (the
/// `GetFileInformationByHandle` triple). Replaces the previous
/// length-equality heuristic which gave false positives whenever two
/// distinct files happened to be the same size.
fn same_file(a: &Path, b: &Path) -> Result<bool> {
    let (va, ia) = fsutil::file_identity(a)?;
    let (vb, ib) = fsutil::file_identity(b)?;
    Ok(va == vb && ia == ib)
}

/// Compute the path of `child` relative to `addon_root` (case-insensitive
/// prefix match, slash-normalized). Returns the suffix as a `PathBuf`
/// using the child's original casing. Empty `PathBuf` when child equals
/// addon_root (caller treats as "no relative path"); also empty if child
/// is outside addon_root — falls back to the bare basename so the
/// migration still has a target name to use.
fn relative_under_addon_root(child: &Path, addon_root: &Path) -> PathBuf {
    let norm_lower = |p: &Path| -> String {
        p.to_string_lossy().replace('/', "\\").to_lowercase()
    };
    let child_norm_lower = norm_lower(child);
    let mut root_norm_lower = norm_lower(addon_root);
    if !root_norm_lower.ends_with('\\') {
        root_norm_lower.push('\\');
    }
    if child_norm_lower.starts_with(&root_norm_lower) {
        // Slice the child's original-case string at the byte length of
        // the lowercased root_norm. Safe because the underlying paths
        // are byte-equivalent in length under lowercase + slash normalize.
        let child_norm_orig = child.to_string_lossy().replace('/', "\\");
        let suffix = &child_norm_orig[root_norm_lower.len()..];
        return PathBuf::from(suffix);
    }
    // Outside addon_root — degrade to bare basename so the rename still
    // has something to land on under managed_root. This is the
    // exotic-case fallback (DB row from a different scan location).
    child
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// Walk addon_root recursively for `.var` files AND `.var` directories
/// that the DB didn't know about (e.g. user-dropped Hub downloads between
/// the last scan and the migration). Move each one into managed_root at
/// its relative position, preserving subfolder structure. Returns the
/// number moved. A re-scan afterward will index them at their new
/// location.
fn walk_and_move_leftovers(
    addon_root: &Path,
    managed_root: &Path,
    errors: &mut Vec<MigrationError>,
) -> Result<i64> {
    use walkdir::WalkDir;
    let mut count: i64 = 0;
    let mut it = WalkDir::new(addon_root).into_iter();
    while let Some(entry) = it.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let ext_ok = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("var"))
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        let ft = entry.file_type();
        if !(ft.is_file() || ft.is_dir()) {
            continue;
        }
        let rel = relative_under_addon_root(path, addon_root);
        if rel.as_os_str().is_empty() {
            continue;
        }
        let new_path = managed_root.join(&rel);
        if new_path.exists() {
            // Already there (partial earlier migration or pre-existing).
            // Don't clobber. Caller can sort it out via a follow-up scan.
            if ft.is_dir() {
                it.skip_current_dir();
            }
            continue;
        }
        if let Some(parent) = new_path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    errors.push(MigrationError {
                        path: path.to_string_lossy().to_string(),
                        reason: format!("create_dir_all({}): {e}", parent.display()),
                    });
                    if ft.is_dir() {
                        it.skip_current_dir();
                    }
                    continue;
                }
            }
        }
        match std::fs::rename(path, &new_path) {
            Ok(()) => {
                count += 1;
                if ft.is_dir() {
                    // After a successful directory rename, the iterator
                    // would otherwise try to descend INTO it (which is now
                    // moved). Skip what's no longer there.
                    it.skip_current_dir();
                }
            }
            Err(e) => {
                errors.push(MigrationError {
                    path: path.to_string_lossy().to_string(),
                    reason: format!("leftover rename failed: {e}"),
                });
                if ft.is_dir() {
                    it.skip_current_dir();
                }
            }
        }
    }
    Ok(count)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// --- tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Spin up a real on-disk addon root and managed root inside the same
    /// TempDir (so they're on the same NTFS volume), plus a fresh in-memory
    /// DB with the minimum schema for the migration. Returns
    /// (workspace, addon_root, managed_root, conn).
    fn fixture() -> (TempDir, PathBuf, PathBuf, Connection) {
        let workspace = TempDir::new().unwrap();
        let addon = workspace.path().join("AddonPackages");
        let managed = workspace.path().join("AddonPackages_Managed");
        std::fs::create_dir_all(&addon).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
             );
             CREATE TABLE packages (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                var_path     TEXT NOT NULL DEFAULT '',
                file_size    INTEGER NOT NULL DEFAULT 0,
                file_mtime   INTEGER NOT NULL DEFAULT 0,
                creator      TEXT NOT NULL DEFAULT '',
                package_name TEXT NOT NULL DEFAULT '',
                version      TEXT NOT NULL DEFAULT '',
                is_hidden    INTEGER NOT NULL DEFAULT 0
             );",
        )
        .unwrap();
        crate::index::set_setting(&conn, "addon_root", addon.to_string_lossy().as_ref()).unwrap();

        (workspace, addon, managed, conn)
    }

    fn add_var(addon: &Path, conn: &Connection, name: &str) {
        let p = addon.join(name);
        std::fs::write(&p, format!("fake var {name}\n").as_bytes()).unwrap();
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name)
             VALUES (?1, ?2, ?3)",
            params![p.to_string_lossy(), "Author", "Pkg"],
        )
        .unwrap();
    }

    #[test]
    fn probe_passes_on_clean_fixture() {
        let (_w, _addon, managed, conn) = fixture();
        let res = probe_managed_path(&conn, managed.to_string_lossy().as_ref()).unwrap();
        assert!(res.ok, "probe should pass; diagnostic = {:?}", res.diagnostic);
    }

    #[test]
    fn probe_rejects_nested_managed_path() {
        let (_w, addon, _managed, conn) = fixture();
        let nested = addon.join("inside");
        let res = probe_managed_path(&conn, nested.to_string_lossy().as_ref()).unwrap();
        assert!(!res.ok);
        let nested_check = res
            .checks
            .iter()
            .find(|c| c.name == "managed_not_under_addon")
            .unwrap();
        assert!(!nested_check.ok);
    }

    #[test]
    fn probe_rejects_nonempty_managed_path() {
        let (_w, _addon, managed, conn) = fixture();
        std::fs::create_dir_all(&managed).unwrap();
        std::fs::write(managed.join("preexisting.txt"), b"hi").unwrap();
        let res = probe_managed_path(&conn, managed.to_string_lossy().as_ref()).unwrap();
        assert!(!res.ok);
        let empty_check = res.checks.iter().find(|c| c.name == "managed_empty").unwrap();
        assert!(!empty_check.ok);
    }

    #[test]
    fn migration_moves_files_and_updates_db() {
        let (_w, addon, managed, mut conn) = fixture();
        add_var(&addon, &conn, "Author.Foo.1.var");
        add_var(&addon, &conn, "Author.Bar.1.var");
        add_var(&addon, &conn, "Other.Baz.1.var");

        let mut progress_count = 0;
        let res = begin_migration(&mut conn, &addon, &managed, |_| {
            progress_count += 1;
        })
        .unwrap();

        assert_eq!(res.moved, 3);
        assert_eq!(res.errors.len(), 0);
        assert_eq!(res.leftover_moved, 0);
        assert!(progress_count >= 1);

        // Files moved
        assert!(!addon.join("Author.Foo.1.var").exists());
        assert!(managed.join("Author.Foo.1.var").exists());
        assert!(managed.join("Other.Baz.1.var").exists());

        // DB updated
        let rows: Vec<String> = {
            let mut stmt = conn.prepare("SELECT var_path FROM packages ORDER BY id").unwrap();
            let v = stmt
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap();
            v
        };
        for r in &rows {
            assert!(
                r.starts_with(managed.to_string_lossy().as_ref()),
                "var_path {r} should be under {}",
                managed.display()
            );
        }

        // Settings persisted
        let state = get_setup_state(&conn).unwrap();
        assert!(state.setup_complete);
        assert_eq!(state.managed_root.as_deref(), Some(managed.to_string_lossy().as_ref()));
        assert!(state.managed_volume_id.is_some());
        assert!(state.setup_completed_at.is_some());
    }

    #[test]
    fn migration_handles_leftover_var_not_in_db() {
        let (_w, addon, managed, mut conn) = fixture();
        add_var(&addon, &conn, "Author.Foo.1.var");
        // Leftover not in DB — user dropped a Hub download in but never
        // re-scanned.
        std::fs::write(addon.join("Stranger.Drop.1.var"), b"leftover\n").unwrap();

        let res = begin_migration(&mut conn, &addon, &managed, |_| {}).unwrap();
        assert_eq!(res.moved, 1);
        assert_eq!(res.leftover_moved, 1);
        assert!(managed.join("Stranger.Drop.1.var").exists());
        assert!(!addon.join("Stranger.Drop.1.var").exists());
    }

    #[test]
    fn migration_is_idempotent_resume() {
        // Simulate a partial migration: pre-position one file at the new
        // location while the DB still points at the old. Migration should
        // recognize this and only update the DB (no rename), then move
        // the rest normally.
        let (_w, addon, managed, mut conn) = fixture();
        add_var(&addon, &conn, "Author.Foo.1.var");
        add_var(&addon, &conn, "Author.Bar.1.var");

        std::fs::create_dir_all(&managed).unwrap();
        // Move Foo manually to managed (simulating partial work).
        std::fs::rename(
            addon.join("Author.Foo.1.var"),
            managed.join("Author.Foo.1.var"),
        )
        .unwrap();

        let res = begin_migration(&mut conn, &addon, &managed, |_| {}).unwrap();
        // Both rows "moved" from the migration's POV — Foo by DB-update,
        // Bar by rename + DB-update.
        assert_eq!(res.moved, 2);
        assert_eq!(res.errors.len(), 0);
        assert!(managed.join("Author.Foo.1.var").exists());
        assert!(managed.join("Author.Bar.1.var").exists());
        assert!(!addon.join("Author.Foo.1.var").exists());
        assert!(!addon.join("Author.Bar.1.var").exists());
    }

    #[test]
    fn migration_records_missing_files_as_errors() {
        let (_w, addon, managed, mut conn) = fixture();
        // DB knows about a file that doesn't exist on disk.
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name)
             VALUES (?1, ?2, ?3)",
            params![addon.join("Ghost.NotReal.1.var").to_string_lossy(), "Ghost", "NotReal"],
        )
        .unwrap();
        // And one real file alongside.
        add_var(&addon, &conn, "Author.Real.1.var");

        let res = begin_migration(&mut conn, &addon, &managed, |_| {}).unwrap();
        assert_eq!(res.moved, 1);
        assert_eq!(res.errors.len(), 1);
        assert!(res.errors[0].reason.contains("missing"));
    }

    #[test]
    fn get_setup_state_initial_is_clean() {
        let (_w, _addon, _managed, conn) = fixture();
        let state = get_setup_state(&conn).unwrap();
        assert!(!state.setup_complete);
        assert!(state.managed_root.is_none());
        assert!(!state.migration_in_progress);
    }

    /// Drop a fake .var directory (the unpacked-package case) at
    /// `<addon>/<name>` and register a matching DB row.
    fn add_var_dir(addon: &Path, conn: &Connection, name: &str) {
        let dir = addon.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), b"{\"creatorName\":\"x\"}").unwrap();
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name)
             VALUES (?1, ?2, ?3)",
            params![dir.to_string_lossy(), "DirAuthor", "Pkg"],
        )
        .unwrap();
    }

    #[test]
    fn migration_preserves_subfolder_structure() {
        let (_w, addon, managed, mut conn) = fixture();
        // Place files in nested subfolders within addon_root. The
        // migration should mirror that structure under managed_root.
        let sub_a = addon.join("AuthorA");
        let sub_b = addon.join("AuthorB").join("Subgroup");
        std::fs::create_dir_all(&sub_a).unwrap();
        std::fs::create_dir_all(&sub_b).unwrap();
        let p1 = sub_a.join("Author.Foo.1.var");
        let p2 = sub_b.join("Author.Bar.1.var");
        std::fs::write(&p1, b"foo").unwrap();
        std::fs::write(&p2, b"bar").unwrap();
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name) VALUES (?1, 'A', 'Foo')",
            params![p1.to_string_lossy()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name) VALUES (?1, 'A', 'Bar')",
            params![p2.to_string_lossy()],
        )
        .unwrap();

        let res = begin_migration(&mut conn, &addon, &managed, |_| {}).unwrap();
        assert_eq!(res.moved, 2);
        assert_eq!(res.errors.len(), 0);

        // Files at mirrored locations under managed_root.
        assert!(managed.join("AuthorA").join("Author.Foo.1.var").exists());
        assert!(managed
            .join("AuthorB")
            .join("Subgroup")
            .join("Author.Bar.1.var")
            .exists());
        // Originals gone.
        assert!(!p1.exists());
        assert!(!p2.exists());
    }

    #[test]
    fn migration_refuses_basename_collisions_across_subfolders() {
        let (_w, addon, managed, mut conn) = fixture();
        // Two files in different subfolders sharing a relative-path
        // basename — the bug that crashed the migration before. With
        // subfolder preservation they'd land at different paths under
        // managed_root, so this case ACTUALLY no longer collides.
        // Instead test the *real* collision: two files in the SAME
        // subfolder (impossible on a real FS within one scan, but
        // simulate via DB rows pointing at the same path on disk).
        let dir = addon.join("Same");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("Author.Foo.1.var");
        std::fs::write(&p, b"foo").unwrap();
        // Two DB rows pointing at the same file — same var_path would
        // hit the UNIQUE constraint at insert time normally, but we
        // bypass by inserting via the no-conflict-clause INSERT.
        // Use distinct var_paths that resolve to the SAME relative
        // (impossible in practice, but tests the precheck).
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name) VALUES (?1, 'A', 'P')",
            params![p.to_string_lossy()],
        )
        .unwrap();
        // Now simulate a casing variant that maps to the same lowercased
        // relative key — the precheck should catch it.
        let p_alt = dir.join("Author.Foo.1.VAR");
        std::fs::write(&p_alt, b"alt").unwrap();
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name) VALUES (?1, 'A', 'P2')",
            params![p_alt.to_string_lossy()],
        )
        .unwrap();

        let err = begin_migration(&mut conn, &addon, &managed, |_| {}).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.to_lowercase().contains("collide"),
                "expected collision error, got: {msg}");
        // Pre-flight bailed BEFORE any move — both files still in place.
        assert!(p.exists());
        assert!(p_alt.exists());
        // Nothing under managed yet.
        assert!(!managed.exists() || managed.read_dir().unwrap().next().is_none());
    }

    #[test]
    fn migration_moves_directory_packages() {
        let (_w, addon, managed, mut conn) = fixture();
        add_var_dir(&addon, &conn, "DirAuthor.Pkg.1.var");

        let res = begin_migration(&mut conn, &addon, &managed, |_| {}).unwrap();
        assert_eq!(res.moved, 1);
        assert_eq!(res.errors.len(), 0);
        // Directory moved with its contents.
        let new_dir = managed.join("DirAuthor.Pkg.1.var");
        assert!(new_dir.is_dir());
        assert!(new_dir.join("meta.json").exists());
        assert!(!addon.join("DirAuthor.Pkg.1.var").exists());
    }

    #[test]
    fn get_setup_state_detects_mid_flight() {
        // Settings have managed_root set but setup_complete unset, AND
        // a package row already points under managed_root.
        let (_w, _addon, managed, conn) = fixture();
        crate::index::set_setting(
            &conn,
            SETTING_MANAGED_ROOT,
            managed.to_string_lossy().as_ref(),
        )
        .unwrap();
        let fake_new_path = managed.join("Half.Migrated.1.var");
        conn.execute(
            "INSERT INTO packages (var_path, creator, package_name)
             VALUES (?1, ?2, ?3)",
            params![fake_new_path.to_string_lossy(), "Half", "Migrated"],
        )
        .unwrap();

        let state = get_setup_state(&conn).unwrap();
        assert!(state.migration_in_progress);
        assert!(!state.setup_complete);
    }
}
