//! App-data directory resolution, shared by the GUI and the CLI binaries.
//!
//! The Tauri bundle identifier changed from `com.github.kylinblue.vam-package-browser`
//! to `com.github.kylinblue.vam-package-browser` (2026-07), which moved the
//! `%APPDATA%` directory Tauri derives from it. `migrate_legacy_data_dir`
//! is the one-time shim that carries an existing install's data across the
//! rename; `default_db_path` keeps the CLI binaries working against either
//! location without a `--db` flag.

use std::path::{Path, PathBuf};

/// Current Tauri bundle identifier = `%APPDATA%` directory name.
/// Must match `identifier` in `tauri.conf.json`.
pub const APP_DIR_NAME: &str = "com.github.kylinblue.vam-package-browser";

/// Pre-rename identifier. Only referenced by the migration shim and the
/// CLI fallback below; new installs never create it.
pub const LEGACY_APP_DIR_NAME: &str = "com.github.kylinblue.vam-package-browser";

/// Default SQLite index path for the CLI binaries (`--db` overrides it).
///
/// Prefers the current app-data dir; falls back to the legacy dir when the
/// current one has no `index.sqlite` yet (i.e. the GUI hasn't run since the
/// identifier rename, so the one-time migration hasn't happened).
pub fn default_db_path() -> PathBuf {
    let base = PathBuf::from(std::env::var("APPDATA").unwrap_or_default());
    let current = base.join(APP_DIR_NAME).join("index.sqlite");
    if current.exists() {
        return current;
    }
    let legacy = base.join(LEGACY_APP_DIR_NAME).join("index.sqlite");
    if legacy.exists() {
        return legacy;
    }
    current
}

/// One-time migration for the identifier rename: if `data_dir` (the current
/// app-data dir) has no `index.sqlite` but a legacy sibling dir does, move
/// the whole legacy directory into place with a same-volume `fs::rename` —
/// instant, and it carries the DB, WAL sidecars, thumbnail cache, and
/// settings in one step.
///
/// Call before `create_dir_all(data_dir)` / opening the DB. A no-op once
/// migrated (or for fresh installs). Never deletes anything: if `data_dir`
/// already exists non-empty without an index, the legacy dir is left alone
/// and a warning is printed rather than guessing which copy wins.
pub fn migrate_legacy_data_dir(data_dir: &Path) {
    if data_dir.join("index.sqlite").exists() {
        return;
    }
    let Some(parent) = data_dir.parent() else { return };
    let legacy = parent.join(LEGACY_APP_DIR_NAME);
    if !legacy.join("index.sqlite").exists() {
        return;
    }
    // Tauri (or an earlier partial run) may have already created an empty
    // current dir; rename() needs the destination absent.
    if data_dir.exists() {
        if std::fs::remove_dir(data_dir).is_err() {
            eprintln!(
                "data-dir migration: {} exists non-empty but has no index.sqlite; \
                 leaving legacy data at {} untouched",
                data_dir.display(),
                legacy.display()
            );
            return;
        }
    }
    match std::fs::rename(&legacy, data_dir) {
        Ok(()) => eprintln!(
            "data-dir migration: moved {} -> {}",
            legacy.display(),
            data_dir.display()
        ),
        Err(e) => eprintln!(
            "data-dir migration: failed to move {} -> {}: {e}; \
             continuing with a fresh data dir (legacy data left in place)",
            legacy.display(),
            data_dir.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_legacy_dir_when_current_missing() {
        let root = tempfile::TempDir::new().unwrap();
        let legacy = root.path().join(LEGACY_APP_DIR_NAME);
        std::fs::create_dir_all(legacy.join("thumbs")).unwrap();
        std::fs::write(legacy.join("index.sqlite"), b"db").unwrap();
        let current = root.path().join(APP_DIR_NAME);

        migrate_legacy_data_dir(&current);

        assert!(current.join("index.sqlite").exists());
        assert!(current.join("thumbs").exists());
        assert!(!legacy.exists());
    }

    #[test]
    fn migrates_over_empty_current_dir() {
        let root = tempfile::TempDir::new().unwrap();
        let legacy = root.path().join(LEGACY_APP_DIR_NAME);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("index.sqlite"), b"db").unwrap();
        let current = root.path().join(APP_DIR_NAME);
        std::fs::create_dir_all(&current).unwrap();

        migrate_legacy_data_dir(&current);

        assert!(current.join("index.sqlite").exists());
        assert!(!legacy.exists());
    }

    #[test]
    fn noop_when_current_has_index() {
        let root = tempfile::TempDir::new().unwrap();
        let legacy = root.path().join(LEGACY_APP_DIR_NAME);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("index.sqlite"), b"old").unwrap();
        let current = root.path().join(APP_DIR_NAME);
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("index.sqlite"), b"new").unwrap();

        migrate_legacy_data_dir(&current);

        assert_eq!(std::fs::read(current.join("index.sqlite")).unwrap(), b"new");
        assert!(legacy.join("index.sqlite").exists());
    }

    #[test]
    fn leaves_nonempty_current_dir_alone() {
        let root = tempfile::TempDir::new().unwrap();
        let legacy = root.path().join(LEGACY_APP_DIR_NAME);
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("index.sqlite"), b"db").unwrap();
        let current = root.path().join(APP_DIR_NAME);
        std::fs::create_dir_all(&current).unwrap();
        std::fs::write(current.join("stray.txt"), b"x").unwrap();

        migrate_legacy_data_dir(&current);

        assert!(!current.join("index.sqlite").exists());
        assert!(legacy.join("index.sqlite").exists());
        assert!(current.join("stray.txt").exists());
    }

    #[test]
    fn noop_on_fresh_install() {
        let root = tempfile::TempDir::new().unwrap();
        let current = root.path().join(APP_DIR_NAME);

        migrate_legacy_data_dir(&current);

        assert!(!current.exists());
    }
}
