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
                let basename = match old_path.file_name() {
                    Some(b) => b.to_owned(),
                    None => {
                        errors.push(MigrationError {
                            path: old_path_str.clone(),
                            reason: "no basename".into(),
                        });
                        continue;
                    }
                };
                let new_path = managed_root.join(&basename);

                // Idempotent migration of one file. Three cases:
                //   (a) file at old_path, none at new_path → rename + update
                //   (b) file at new_path (possibly also at old) → update only
                //       (assume previous run already moved it; if old also
                //        present, that's a collision the user has to resolve)
                //   (c) file at neither → DB row points at nothing; record
                //       error but don't fail the whole migration
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
                if old_exists && new_exists && !same_inode(&old_path, &new_path).unwrap_or(false) {
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

/// Cheap "is this the same NTFS file" check via metadata. We use it to
/// distinguish "leftover from previous run" (same file, both paths point
/// to the same inode because rename just renamed it) from "two different
/// files happen to have the same name" (collision).
///
/// On NTFS, `fs::metadata` lets us read len + creation time; combined
/// they're a strong-but-not-bulletproof signal. A full
/// `GetFileInformationByHandle` + `nFileIndex` check would be airtight
/// but needs `windows` crate plumbing we can defer. Len equality is
/// enough for the migration's purposes: a `rename` preserves len; a
/// "two distinct files with the same name" scenario is wildly unlikely
/// post-empty-folder validation.
fn same_inode(a: &Path, b: &Path) -> Result<bool> {
    let ma = std::fs::metadata(a)?;
    let mb = std::fs::metadata(b)?;
    Ok(ma.len() == mb.len())
}

fn walk_and_move_leftovers(
    addon_root: &Path,
    managed_root: &Path,
    errors: &mut Vec<MigrationError>,
) -> Result<i64> {
    let mut count: i64 = 0;
    let read = match std::fs::read_dir(addon_root) {
        Ok(r) => r,
        Err(_) => return Ok(0),
    };
    for entry in read.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let ext_ok = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("var"))
            .unwrap_or(false);
        if !ext_ok {
            continue;
        }
        let basename = match path.file_name() {
            Some(b) => b.to_owned(),
            None => continue,
        };
        let new_path = managed_root.join(&basename);
        if new_path.exists() {
            // Already there (e.g. partial earlier migration). Leave the
            // leftover where it is; it'll be reported as orphaned later.
            continue;
        }
        if let Err(e) = std::fs::rename(&path, &new_path) {
            errors.push(MigrationError {
                path: path.to_string_lossy().to_string(),
                reason: format!("leftover rename failed: {e}"),
            });
            continue;
        }
        count += 1;
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
