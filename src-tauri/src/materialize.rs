//! Visibility-presets materialization: reconcile the active folder
//! (`addon_root`) against the closure of a `SeedSpec`.
//!
//! Public surface: `load(target_seeds)` makes the active folder contain
//! exactly the closure of `target_seeds` as NTFS hardlinks to files in
//! `managed_root`. `unload_all()` empties it. `verify_active_folder()`
//! is a read-only health check.
//!
//! Invariants (see TODO-visibility-presets.md for full design):
//! - Every file we create is a hardlink from a `managed_root` source to
//!   an `addon_root` destination. Same NTFS volume only.
//! - `active_folder_state` is the authoritative ledger of what we placed.
//!   We never touch a file in `addon_root` that isn't in the ledger.
//! - Every destination path is asserted inside `addon_root` before any
//!   FS write. No way for a misconfigured setting to chew elsewhere.
//! - Sync is per-file idempotent: re-running `load` with the same
//!   target after a crash converges.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use serde::Serialize;

use crate::{fsutil, index, setup, visibility::SeedSpec};

// --- public types -----------------------------------------------------------

#[derive(Debug, Clone, Serialize, Default)]
pub struct LoadResult {
    /// Packages newly hardlinked into the active folder in this call.
    pub added: i64,
    /// Packages whose hardlink was removed from the active folder.
    pub removed: i64,
    /// Packages that were already correctly materialized and didn't need
    /// touching. `kept + added == |closure(target_seeds)|` after a
    /// successful call.
    pub kept: i64,
    /// Per-package errors that didn't abort the whole sync. Most common:
    /// destination path is occupied by a file we don't manage; or the
    /// source `.var` is missing from `managed_root` (DB out of date).
    pub errors: Vec<LoadError>,
    /// Wall-clock duration of the sync, milliseconds.
    pub elapsed_ms: u128,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadError {
    pub package_id: i64,
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct VerifyResult {
    /// Total rows in `active_folder_state`.
    pub total: i64,
    /// Rows whose `active_path` exists AND points at the same NTFS
    /// inode as the matching `managed_root` source.
    pub ok: i64,
    /// Rows whose `active_path` is gone from disk (user manually
    /// deleted the file). Caller can drop these rows or reschedule a
    /// re-link via `load`.
    pub missing_in_active: Vec<i64>,
    /// Rows whose `active_path` exists but doesn't share an inode with
    /// the managed-side source. Means someone replaced the hardlink
    /// with a copy (e.g. cross-volume rename) or with a different file.
    pub inode_mismatch: Vec<i64>,
    /// Rows whose source in `managed_root` has gone missing (DB drift).
    pub missing_in_managed: Vec<i64>,
}

// --- public API -------------------------------------------------------------

/// Reconcile the active folder so it contains exactly the closure of
/// `target_seeds`. Hardlinks new packages in, removes ones that fell out
/// of the target set, and leaves the rest alone.
///
/// Returns counts + per-package errors. Hard errors (setup not complete,
/// volume mismatch, lookup failures on referenced packages) bubble up
/// as `Err`; per-file failures show up in `LoadResult.errors`.
pub fn load(conn: &mut Connection, target_seeds: &SeedSpec) -> Result<LoadResult> {
    let start = Instant::now();

    let (addon_root, managed_root) = read_roots(conn)?;
    same_volume_or_bail(&addon_root, &managed_root)?;

    // 1. Compute closure → target id set.
    let closure_ids = crate::visibility::compute_closure(conn, target_seeds)?;
    let target: HashSet<i64> = closure_ids.iter().copied().collect();

    // 2. Snapshot current active state.
    let current = read_active_state(conn)?;
    let current_ids: HashSet<i64> = current.keys().copied().collect();

    // 3. Diff.
    let mut to_add: Vec<i64> = target.difference(&current_ids).copied().collect();
    to_add.sort();
    let mut to_remove: Vec<(i64, String)> = current
        .iter()
        .filter(|(id, _)| !target.contains(id))
        .map(|(id, path)| (*id, path.clone()))
        .collect();
    to_remove.sort_by_key(|(id, _)| *id);
    let kept = target.intersection(&current_ids).count() as i64;

    // 4. Resolve source paths for adds (packages.var_path under managed_root).
    let source_paths = read_var_paths(conn, &to_add)?;

    let mut errors: Vec<LoadError> = Vec::new();
    let mut added: i64 = 0;
    let mut removed: i64 = 0;

    // 5. Apply adds in a single transaction (DB writes batched; FS
    //    writes happen mid-transaction but a commit failure rolls back
    //    the DB rows so the ledger never lies about extant files).
    if !to_add.is_empty() {
        let now = unix_now();
        let tx = conn.transaction()?;
        {
            let mut ins = tx.prepare(
                "INSERT INTO active_folder_state (package_id, active_path, materialized_at)
                 VALUES (?1, ?2, ?3)",
            )?;
            for id in &to_add {
                match link_one(&addon_root, &managed_root, &source_paths, *id) {
                    Ok(dest_str) => {
                        ins.execute(params![*id, dest_str, now])?;
                        added += 1;
                    }
                    Err(e) => errors.push(e),
                }
            }
        }
        tx.commit()?;
    }

    // 6. Apply removes. Same atomicity story.
    if !to_remove.is_empty() {
        let tx = conn.transaction()?;
        {
            let mut del = tx.prepare("DELETE FROM active_folder_state WHERE package_id = ?1")?;
            for (id, active_path) in &to_remove {
                match unlink_one(&addon_root, Path::new(active_path)) {
                    Ok(()) => {
                        del.execute(params![*id])?;
                        removed += 1;
                    }
                    Err(e) => errors.push(LoadError {
                        package_id: *id,
                        path: active_path.clone(),
                        reason: e.to_string(),
                    }),
                }
            }
        }
        tx.commit()?;
    }

    Ok(LoadResult {
        added,
        removed,
        kept,
        errors,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

/// Empty the active folder. Equivalent to `load(empty SeedSpec)` but
/// surfaces clearly as a distinct operation in the API.
pub fn unload_all(conn: &mut Connection) -> Result<LoadResult> {
    load(conn, &SeedSpec::default())
}

/// Dry-run for the load-visibility modal: closure preview + diff
/// against the current `active_folder_state`. Lets the UI render
/// "+A / −R / =K" before the user commits, without doing the FS work
/// twice. Pure SQL, no FS touch.
pub fn compute_load_plan(
    conn: &Connection,
    target_seeds: &SeedSpec,
) -> Result<LoadPlan> {
    let preview = crate::visibility::compute_preview(conn, target_seeds)?;
    let target: HashSet<i64> = preview.package_ids.iter().copied().collect();

    let current: HashSet<i64> = {
        let mut stmt = conn.prepare("SELECT package_id FROM active_folder_state")?;
        let ids = stmt
            .query_map([], |r| r.get::<_, i64>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?;
        ids
    };

    let will_keep = target.intersection(&current).count() as i64;
    let will_add = (target.len() as i64) - will_keep;
    let will_remove = (current.len() as i64) - will_keep;

    Ok(LoadPlan {
        currently_loaded: current.len() as i64,
        will_add,
        will_remove,
        will_keep,
        preview,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadPlan {
    /// Closure preview (counts + ids + unresolved deps). Carries through
    /// to the UI so it can show the author/package/dep breakdown.
    pub preview: crate::visibility::ClosurePreview,
    /// Count of rows currently in `active_folder_state`.
    pub currently_loaded: i64,
    /// Packages that would be newly hardlinked on commit.
    pub will_add: i64,
    /// Packages that would be unlinked (they're in the current active
    /// set but not in the closure of `target_seeds`).
    pub will_remove: i64,
    /// Packages already correctly materialized — no FS touch needed.
    pub will_keep: i64,
}

/// Walk every row in `active_folder_state` and report which entries are
/// still healthy. Read-only — does not mutate the ledger. Caller decides
/// whether to fix stale rows (call `load` to re-converge, or use a
/// future repair command).
pub fn verify_active_folder(conn: &Connection) -> Result<VerifyResult> {
    let (_addon_root, managed_root) = read_roots(conn)?;

    let rows: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT package_id, active_path FROM active_folder_state ORDER BY package_id",
        )?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    let mut result = VerifyResult {
        total: rows.len() as i64,
        ..VerifyResult::default()
    };

    let var_paths: HashMap<i64, String> = {
        let mut stmt = conn.prepare(
            "SELECT id, var_path FROM packages WHERE id IN (
                SELECT package_id FROM active_folder_state
             )",
        )?;
        let pairs = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        pairs.into_iter().collect()
    };

    for (id, active_path) in rows {
        let active = Path::new(&active_path);
        if !active.exists() {
            result.missing_in_active.push(id);
            continue;
        }
        let source_str = match var_paths.get(&id) {
            Some(s) => s,
            None => {
                // Package row missing entirely (shouldn't happen — FK
                // would have cascaded — but defensive).
                result.missing_in_managed.push(id);
                continue;
            }
        };
        let source = PathBuf::from(source_str);
        // Confirm source still under managed_root and exists.
        if !source.exists() {
            result.missing_in_managed.push(id);
            continue;
        }
        let _ = managed_root; // bound for debug logging in a future iteration
        if same_identity(active, &source).unwrap_or(false) {
            result.ok += 1;
        } else {
            result.inode_mismatch.push(id);
        }
    }

    Ok(result)
}

// --- internals --------------------------------------------------------------

fn read_roots(conn: &Connection) -> Result<(PathBuf, PathBuf)> {
    let setup_complete = index::get_setting(conn, setup::SETTING_SETUP_COMPLETE)?
        .as_deref()
        == Some("1");
    if !setup_complete {
        return Err(anyhow!(
            "load/unload requires setup to be complete; run the setup wizard first"
        ));
    }
    let addon_root = index::get_setting(conn, "addon_root")?
        .ok_or_else(|| anyhow!("addon_root not set"))?;
    let managed_root = index::get_setting(conn, setup::SETTING_MANAGED_ROOT)?
        .ok_or_else(|| anyhow!("managed_root not set despite setup_complete"))?;
    Ok((PathBuf::from(addon_root), PathBuf::from(managed_root)))
}

fn same_volume_or_bail(addon: &Path, managed: &Path) -> Result<()> {
    let same = fsutil::same_volume(addon, managed)
        .with_context(|| "checking same-volume invariant")?;
    if !same {
        return Err(anyhow!(
            "addon_root ({}) and managed_root ({}) are no longer on the same volume — \
             cannot hardlink",
            addon.display(),
            managed.display()
        ));
    }
    Ok(())
}

fn read_active_state(conn: &Connection) -> Result<HashMap<i64, String>> {
    let mut stmt =
        conn.prepare("SELECT package_id, active_path FROM active_folder_state")?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(rows)
}

/// (var_path, is_directory_package) tuple per id. Drives the link_one
/// branch between hardlink (file) and junction (directory).
struct SourceInfo {
    var_path: String,
    is_directory_package: bool,
}

fn read_var_paths(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, SourceInfo>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let mut out = HashMap::with_capacity(ids.len());
    let mut stmt = conn
        .prepare("SELECT var_path, is_directory_package FROM packages WHERE id = ?1")?;
    for &id in ids {
        let row: (String, i64) =
            stmt.query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        out.insert(
            id,
            SourceInfo {
                var_path: row.0,
                is_directory_package: row.1 != 0,
            },
        );
    }
    Ok(out)
}

fn link_one(
    addon_root: &Path,
    managed_root: &Path,
    source_paths: &HashMap<i64, SourceInfo>,
    id: i64,
) -> std::result::Result<String, LoadError> {
    let info = match source_paths.get(&id) {
        Some(s) => s,
        None => {
            return Err(LoadError {
                package_id: id,
                path: String::new(),
                reason: "package row missing var_path".into(),
            });
        }
    };
    let source = PathBuf::from(&info.var_path);

    // Subfolder-preserving destination. Compute the relative path of the
    // source under managed_root, then mirror it under addon_root.
    // Falls back to bare basename if (somehow) the var_path isn't under
    // managed_root — defensive, normally unreachable post-setup.
    let relative = relative_under(&source, managed_root).unwrap_or_else(|| {
        source
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_default()
    });
    if relative.as_os_str().is_empty() {
        return Err(LoadError {
            package_id: id,
            path: info.var_path.clone(),
            reason: "could not derive relative path under managed_root".into(),
        });
    }
    let dest = addon_root.join(&relative);

    // Belt-and-suspenders: dest must be inside addon_root.
    let dest_norm = normalize(&dest);
    let addon_norm = normalize(addon_root);
    if !dest_norm.starts_with(&addon_norm) {
        return Err(LoadError {
            package_id: id,
            path: dest.to_string_lossy().to_string(),
            reason: "refused: dest escaped addon_root".into(),
        });
    }

    // Refuse if dest exists and isn't ours. (If it is ours, we'd be in
    // the `keep` branch upstream, not here.)
    if dest.exists() {
        return Err(LoadError {
            package_id: id,
            path: dest.to_string_lossy().to_string(),
            reason: "destination occupied by an unmanaged file".into(),
        });
    }

    // Create the parent directory tree under addon_root if missing.
    // Mirrors the subfolder layout of managed_root for this entry.
    if let Some(parent) = dest.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return Err(LoadError {
                package_id: id,
                path: dest.to_string_lossy().to_string(),
                reason: format!("create_dir_all({}): {e}", parent.display()),
            });
        }
    }

    // Materialize: hardlink for file packages, NTFS junction for
    // directory packages (NTFS doesn't allow hardlinks on dirs).
    let materialized = if info.is_directory_package {
        make_junction(&source, &dest)
    } else {
        std::fs::hard_link(&source, &dest).map_err(|e| anyhow!("{e}"))
    };
    if let Err(e) = materialized {
        return Err(LoadError {
            package_id: id,
            path: dest.to_string_lossy().to_string(),
            reason: if info.is_directory_package {
                format!("junction failed: {e}")
            } else {
                format!("hard_link failed: {e}")
            },
        });
    }

    Ok(dest.to_string_lossy().to_string())
}

/// Create an NTFS directory junction at `dest` pointing at `target`.
/// Same-volume only; no admin / Dev-Mode required (junctions don't
/// count as symlinks for the SE_CREATE_SYMBOLIC_LINK_NAME privilege).
///
/// Implementation: shell out to `cmd /c mklink /J`. The Win32 way is
/// `DeviceIoControl(FSCTL_SET_REPARSE_POINT)` with a hand-built
/// REPARSE_DATA_BUFFER — about 30 lines of unsafe FFI we can do
/// later if the shell-out becomes a hot path. Junction creation is
/// once-per-package-load, so the process spawn isn't load-critical.
fn make_junction(target: &Path, dest: &Path) -> Result<()> {
    use std::process::Command;

    // `mklink /J <link> <target>` creates the link (dest, must not exist)
    // pointing at target. cmd /c is required since mklink is an internal
    // cmd command, not a standalone executable.
    let output = Command::new("cmd")
        .args(&[
            "/c",
            "mklink",
            "/J",
            &dest.to_string_lossy(),
            &target.to_string_lossy(),
        ])
        .output()
        .map_err(|e| anyhow!("spawn mklink: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "mklink /J failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }
    Ok(())
}

/// Strip a `parent` prefix from `child` case-insensitively, returning the
/// child's original-case suffix as a PathBuf. None when child doesn't sit
/// under parent.
fn relative_under(child: &Path, parent: &Path) -> Option<PathBuf> {
    let child_norm_lower = normalize(child);
    let mut parent_norm_lower = normalize(parent);
    if !parent_norm_lower.ends_with('\\') {
        parent_norm_lower.push('\\');
    }
    if !child_norm_lower.starts_with(&parent_norm_lower) {
        return None;
    }
    // Slice on the original-case (slash-normalized) child at the
    // byte-length of the lowercased prefix. Safe for ASCII path prefixes.
    let child_norm_orig = child.to_string_lossy().replace('/', "\\");
    Some(PathBuf::from(&child_norm_orig[parent_norm_lower.len()..]))
}

fn unlink_one(addon_root: &Path, active_path: &Path) -> Result<()> {
    // Same safety guard as link_one.
    let active_norm = normalize(active_path);
    let addon_norm = normalize(addon_root);
    if !active_norm.starts_with(&addon_norm) {
        return Err(anyhow!(
            "refusing to unlink: active_path escaped addon_root ({})",
            active_path.display()
        ));
    }
    // Detect what we placed: file vs. directory junction. On Windows,
    // `symlink_metadata().file_type()` for a junction reports
    // `is_symlink_dir() = true` (NOT `is_dir()` — that's set only for
    // non-reparse directories). `remove_dir` removes the junction
    // reparse point without touching the target; `remove_file` removes
    // a regular file. Idempotent on NotFound either way.
    #[cfg(target_os = "windows")]
    use std::os::windows::fs::FileTypeExt;
    let md = match std::fs::symlink_metadata(active_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(());
        }
        Err(e) => return Err(anyhow!("symlink_metadata: {e}")),
    };
    let ft = md.file_type();
    #[cfg(target_os = "windows")]
    let dir_like = ft.is_dir() || ft.is_symlink_dir();
    #[cfg(not(target_os = "windows"))]
    let dir_like = ft.is_dir();
    let result = if dir_like {
        std::fs::remove_dir(active_path)
    } else {
        std::fs::remove_file(active_path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!("remove: {e}")),
    }
}

/// Lowercase + slash-normalize for prefix checks. Same approach as
/// setup::is_nested — bypasses canonicalize's `\\?\` quirk on Windows
/// and works on non-existent paths.
fn normalize(p: &Path) -> String {
    p.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// Real NTFS identity check via `GetFileInformationByHandle`. Two paths
/// share the same underlying file (or directory) iff their volume serial
/// + nFileIndex pair match. Used by `verify_active_folder` to confirm a
/// hardlink still points at its managed-side source.
fn same_identity(a: &Path, b: &Path) -> Result<bool> {
    let (va, ia) = fsutil::file_identity(a)?;
    let (vb, ib) = fsutil::file_identity(b)?;
    Ok(va == vb && ia == ib)
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

    /// Build a post-setup workspace: real addon + managed folders on the
    /// same TempDir (same NTFS volume), in-memory DB with the schema
    /// columns load() touches, and setup-complete flags set.
    fn fixture() -> (TempDir, PathBuf, PathBuf, Connection) {
        let workspace = TempDir::new().unwrap();
        let addon = workspace.path().join("AddonPackages");
        let managed = workspace.path().join("AddonPackages_Managed");
        std::fs::create_dir_all(&addon).unwrap();
        std::fs::create_dir_all(&managed).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE app_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE packages (
                id                   INTEGER PRIMARY KEY,
                var_path             TEXT NOT NULL DEFAULT '',
                creator              TEXT NOT NULL DEFAULT '',
                package_name         TEXT NOT NULL DEFAULT '',
                version              TEXT NOT NULL DEFAULT '1',
                is_hidden            INTEGER NOT NULL DEFAULT 0,
                is_directory_package INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE package_dep_links (
                src_package_id INTEGER NOT NULL,
                dst_package_id INTEGER,
                raw_dep_key    TEXT NOT NULL,
                PRIMARY KEY (src_package_id, raw_dep_key)
             );
             CREATE TABLE active_folder_state (
                package_id      INTEGER PRIMARY KEY,
                active_path     TEXT NOT NULL,
                materialized_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        crate::index::set_setting(&conn, "addon_root", addon.to_string_lossy().as_ref()).unwrap();
        crate::index::set_setting(
            &conn,
            setup::SETTING_MANAGED_ROOT,
            managed.to_string_lossy().as_ref(),
        )
        .unwrap();
        crate::index::set_setting(&conn, setup::SETTING_SETUP_COMPLETE, "1").unwrap();

        (workspace, addon, managed, conn)
    }

    /// Insert a fake .var file in managed_root and a matching packages row.
    fn add_pkg(
        managed: &Path,
        conn: &Connection,
        id: i64,
        creator: &str,
        name: &str,
    ) {
        let basename = format!("{creator}.{name}.1.var");
        let path = managed.join(&basename);
        std::fs::write(&path, format!("fake var {creator}.{name}\n").as_bytes()).unwrap();
        conn.execute(
            "INSERT INTO packages (id, var_path, creator, package_name)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, path.to_string_lossy(), creator, name],
        )
        .unwrap();
    }

    fn add_dep(conn: &Connection, src: i64, dst: Option<i64>, key: &str) {
        conn.execute(
            "INSERT INTO package_dep_links (src_package_id, dst_package_id, raw_dep_key)
             VALUES (?1, ?2, ?3)",
            params![src, dst, key],
        )
        .unwrap();
    }

    fn active_paths(conn: &Connection) -> Vec<(i64, String)> {
        let mut stmt = conn
            .prepare("SELECT package_id, active_path FROM active_folder_state ORDER BY package_id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn entries_in(dir: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        out.sort();
        out
    }

    #[test]
    fn load_empty_seeds_is_no_op() {
        let (_w, addon, _managed, mut conn) = fixture();
        let res = load(&mut conn, &SeedSpec::default()).unwrap();
        assert_eq!(res.added, 0);
        assert_eq!(res.removed, 0);
        assert_eq!(res.kept, 0);
        assert!(entries_in(&addon).is_empty());
    }

    #[test]
    fn load_author_seed_hardlinks_packages() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        add_pkg(&managed, &conn, 2, "Alice", "Bar");
        add_pkg(&managed, &conn, 3, "Bob", "Baz");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let res = load(&mut conn, &seeds).unwrap();
        assert_eq!(res.added, 2);
        assert_eq!(res.errors.len(), 0);
        let mut got = entries_in(&addon);
        got.sort();
        assert_eq!(got, vec!["Alice.Bar.1.var", "Alice.Foo.1.var"]);

        // active_folder_state populated
        let st = active_paths(&conn);
        assert_eq!(st.len(), 2);
        let ids: Vec<i64> = st.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn load_then_reload_same_target_is_no_op() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        add_pkg(&managed, &conn, 2, "Alice", "Bar");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let r1 = load(&mut conn, &seeds).unwrap();
        assert_eq!(r1.added, 2);
        assert_eq!(r1.kept, 0);

        let r2 = load(&mut conn, &seeds).unwrap();
        assert_eq!(r2.added, 0);
        assert_eq!(r2.removed, 0);
        assert_eq!(r2.kept, 2);
        // Idempotency: addon still has the same two files.
        let entries = entries_in(&addon);
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn switching_targets_diffs_correctly() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        add_pkg(&managed, &conn, 2, "Alice", "Bar");
        add_pkg(&managed, &conn, 3, "Bob", "Baz");
        add_pkg(&managed, &conn, 4, "Bob", "Qux");

        // Load Alice's set.
        let alice = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        load(&mut conn, &alice).unwrap();
        assert_eq!(entries_in(&addon).len(), 2);

        // Switch to Bob.
        let bob = SeedSpec {
            creators: vec!["Bob".into()],
            package_ids: vec![],
        };
        let res = load(&mut conn, &bob).unwrap();
        assert_eq!(res.added, 2);
        assert_eq!(res.removed, 2);
        assert_eq!(res.kept, 0);
        let mut got = entries_in(&addon);
        got.sort();
        assert_eq!(got, vec!["Bob.Baz.1.var", "Bob.Qux.1.var"]);
    }

    #[test]
    fn closure_pulls_in_deps() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Scene"); // seed
        add_pkg(&managed, &conn, 2, "Bob", "Plugin"); // dep
        add_pkg(&managed, &conn, 3, "Carol", "OtherPlugin"); // unrelated
        add_dep(&conn, 1, Some(2), "Bob.Plugin.1");

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let res = load(&mut conn, &seeds).unwrap();
        assert_eq!(res.added, 2); // Alice.Scene + Bob.Plugin
        let mut got = entries_in(&addon);
        got.sort();
        assert_eq!(got, vec!["Alice.Scene.1.var", "Bob.Plugin.1.var"]);
    }

    #[test]
    fn unload_all_empties_active_folder() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        add_pkg(&managed, &conn, 2, "Alice", "Bar");
        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        load(&mut conn, &seeds).unwrap();
        assert_eq!(entries_in(&addon).len(), 2);

        let res = unload_all(&mut conn).unwrap();
        assert_eq!(res.removed, 2);
        assert_eq!(res.added, 0);
        assert_eq!(res.kept, 0);
        assert!(entries_in(&addon).is_empty());
        assert_eq!(active_paths(&conn).len(), 0);
    }

    #[test]
    fn dest_already_exists_is_an_error_not_an_overwrite() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        // Pre-populate addon with a file we didn't put there.
        std::fs::write(addon.join("Alice.Foo.1.var"), b"unmanaged\n").unwrap();

        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        let res = load(&mut conn, &seeds).unwrap();
        assert_eq!(res.added, 0);
        assert_eq!(res.errors.len(), 1);
        assert!(res.errors[0].reason.contains("unmanaged"));
        // Unmanaged file untouched.
        let bytes = std::fs::read(addon.join("Alice.Foo.1.var")).unwrap();
        assert_eq!(bytes, b"unmanaged\n");
        // State table is empty (the row was never inserted).
        assert_eq!(active_paths(&conn).len(), 0);
    }

    #[test]
    fn refuses_load_when_setup_incomplete() {
        let (_w, _addon, _managed, mut conn) = fixture();
        // Reset setup flag.
        crate::index::set_setting(&conn, setup::SETTING_SETUP_COMPLETE, "0").unwrap();

        let seeds = SeedSpec::default();
        let err = load(&mut conn, &seeds).unwrap_err();
        assert!(err.to_string().contains("setup"));
    }

    #[test]
    fn verify_reports_healthy_active_folder() {
        let (_w, _addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        load(&mut conn, &seeds).unwrap();

        let res = verify_active_folder(&conn).unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.ok, 1);
        assert!(res.missing_in_active.is_empty());
        assert!(res.inode_mismatch.is_empty());
        assert!(res.missing_in_managed.is_empty());
    }

    #[test]
    fn verify_catches_user_deleted_hardlink() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        load(
            &mut conn,
            &SeedSpec {
                creators: vec!["Alice".into()],
                package_ids: vec![],
            },
        )
        .unwrap();

        // User manually deletes the hardlink from the active folder.
        std::fs::remove_file(addon.join("Alice.Foo.1.var")).unwrap();

        let res = verify_active_folder(&conn).unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.ok, 0);
        assert_eq!(res.missing_in_active, vec![1]);
    }

    #[test]
    fn verify_catches_missing_managed_source() {
        let (_w, _addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        load(
            &mut conn,
            &SeedSpec {
                creators: vec!["Alice".into()],
                package_ids: vec![],
            },
        )
        .unwrap();

        // Simulate managed-side drift: delete the source. (Wouldn't
        // normally happen — managed_root is read-only by convention —
        // but verify should still surface it.)
        std::fs::remove_file(managed.join("Alice.Foo.1.var")).unwrap();

        let res = verify_active_folder(&conn).unwrap();
        assert_eq!(res.missing_in_managed, vec![1]);
    }

    #[test]
    fn load_plan_breaks_down_add_remove_keep() {
        let (_w, _addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        add_pkg(&managed, &conn, 2, "Alice", "Bar");
        add_pkg(&managed, &conn, 3, "Bob", "Baz");

        // Pre-load Alice's set.
        load(
            &mut conn,
            &SeedSpec {
                creators: vec!["Alice".into()],
                package_ids: vec![],
            },
        )
        .unwrap();

        // Plan a switch to Alice + Bob: Alice's 2 stay (keep),
        // Bob's 1 gets added.
        let plan = compute_load_plan(
            &conn,
            &SeedSpec {
                creators: vec!["Alice".into(), "Bob".into()],
                package_ids: vec![],
            },
        )
        .unwrap();
        assert_eq!(plan.currently_loaded, 2);
        assert_eq!(plan.will_add, 1);
        assert_eq!(plan.will_remove, 0);
        assert_eq!(plan.will_keep, 2);
        assert_eq!(plan.preview.total, 3);

        // Plan an unload-all: 2 currently loaded → 0 target = remove 2.
        let plan = compute_load_plan(&conn, &SeedSpec::default()).unwrap();
        assert_eq!(plan.currently_loaded, 2);
        assert_eq!(plan.will_add, 0);
        assert_eq!(plan.will_remove, 2);
        assert_eq!(plan.will_keep, 0);
    }

    /// Drop a fake .var directory in `managed`, register a matching DB
    /// row with is_directory_package=1.
    fn add_pkg_dir(
        managed: &Path,
        conn: &Connection,
        id: i64,
        creator: &str,
        name: &str,
    ) {
        let basename = format!("{creator}.{name}.1.var");
        let dir = managed.join(&basename);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), b"{\"creatorName\":\"x\"}").unwrap();
        std::fs::write(dir.join("payload.bin"), b"directory payload").unwrap();
        conn.execute(
            "INSERT INTO packages (id, var_path, creator, package_name, is_directory_package)
             VALUES (?1, ?2, ?3, ?4, 1)",
            params![id, dir.to_string_lossy(), creator, name],
        )
        .unwrap();
    }

    #[test]
    fn load_creates_junction_for_directory_package() {
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg_dir(&managed, &conn, 1, "DirAuthor", "Pkg");

        let res = load(
            &mut conn,
            &SeedSpec {
                creators: vec!["DirAuthor".into()],
                package_ids: vec![],
            },
        )
        .unwrap();
        assert_eq!(res.added, 1, "errors: {:?}", res.errors);
        assert_eq!(res.errors.len(), 0);

        let active = addon.join("DirAuthor.Pkg.1.var");
        // Junction reports as symlink_dir via symlink_metadata (NOT plain
        // is_dir — junctions have FILE_ATTRIBUTE_REPARSE_POINT set on top
        // of FILE_ATTRIBUTE_DIRECTORY). Following the junction via
        // regular `metadata()` reports the target's plain dir type.
        use std::os::windows::fs::FileTypeExt;
        let md = std::fs::symlink_metadata(&active).unwrap();
        assert!(md.file_type().is_symlink_dir());
        assert!(active.is_dir(), "should resolve to a directory via the junction");
        // Contents accessible through the junction.
        assert!(active.join("meta.json").exists());
        assert!(active.join("payload.bin").exists());

        // Unload: junction removed, original untouched.
        let res = unload_all(&mut conn).unwrap();
        assert_eq!(res.removed, 1);
        assert!(!active.exists());
        assert!(managed.join("DirAuthor.Pkg.1.var").join("meta.json").exists());
    }

    #[test]
    fn load_preserves_subfolder_structure() {
        let (_w, addon, managed, mut conn) = fixture();
        // Create a managed-side file inside a nested subfolder; load
        // should mirror that path under addon_root.
        let sub = managed.join("AuthorX").join("Group");
        std::fs::create_dir_all(&sub).unwrap();
        let p = sub.join("AuthorX.Foo.1.var");
        std::fs::write(&p, b"foo").unwrap();
        conn.execute(
            "INSERT INTO packages (id, var_path, creator, package_name)
             VALUES (1, ?1, 'AuthorX', 'Foo')",
            params![p.to_string_lossy()],
        )
        .unwrap();

        let res = load(
            &mut conn,
            &SeedSpec {
                creators: vec!["AuthorX".into()],
                package_ids: vec![],
            },
        )
        .unwrap();
        assert_eq!(res.added, 1, "errors: {:?}", res.errors);
        let expected = addon
            .join("AuthorX")
            .join("Group")
            .join("AuthorX.Foo.1.var");
        assert!(expected.exists());
    }

    #[test]
    fn user_deletion_then_reload_is_self_healing() {
        // The "stale ledger row" recovery story from the plan: user
        // manually deletes a hardlink → next load picks it up via
        // unlink-Ok-on-NotFound, then re-adds if still in target.
        let (_w, addon, managed, mut conn) = fixture();
        add_pkg(&managed, &conn, 1, "Alice", "Foo");
        let seeds = SeedSpec {
            creators: vec!["Alice".into()],
            package_ids: vec![],
        };
        load(&mut conn, &seeds).unwrap();
        std::fs::remove_file(addon.join("Alice.Foo.1.var")).unwrap();

        // Now load with empty seeds (the file is no longer in target,
        // but the ledger thinks it's there). Should clear cleanly even
        // though the file is already gone.
        let res = load(&mut conn, &SeedSpec::default()).unwrap();
        assert_eq!(res.removed, 1);
        assert_eq!(res.errors.len(), 0);
        assert_eq!(active_paths(&conn).len(), 0);
    }
}
