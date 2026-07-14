//! App-data directory resolution, shared by the GUI and the CLI binaries.

use std::path::PathBuf;

/// Tauri bundle identifier = `%APPDATA%` directory name.
/// Must match `identifier` in `tauri.conf.json`.
pub const APP_DIR_NAME: &str = "com.github.kylinblue.vam-package-browser";

/// Default SQLite index path for the CLI binaries (`--db` overrides it).
pub fn default_db_path() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_default())
        .join(APP_DIR_NAME)
        .join("index.sqlite")
}
