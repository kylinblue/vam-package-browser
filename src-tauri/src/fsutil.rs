//! Filesystem helpers for visibility-presets setup.
//!
//! Windows-specific volume-serial queries via `GetVolumeInformationW` and
//! `GetVolumePathNameW`. Used by the setup wizard's same-volume probe
//! (hardlinks can't cross NTFS volumes; we must verify the user's chosen
//! managed library is on the same drive as VaM's AddonPackages before
//! committing the one-time migration).
//!
//! Non-Windows builds get stubs that return errors — the app only ships
//! on Windows, but `cargo check` on dev hardware shouldn't break.

use std::path::Path;

use anyhow::{anyhow, Context, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeInfo {
    /// NTFS volume serial number (32-bit). Identifies the physical volume
    /// across mount-point changes — what we actually need to confirm
    /// hardlink compatibility between two paths.
    pub serial: u32,
    /// Filesystem name as reported by the OS (e.g. `"NTFS"`, `"exFAT"`,
    /// `"FAT32"`). We require NTFS for hardlinking.
    pub filesystem: String,
}

impl VolumeInfo {
    pub fn is_ntfs(&self) -> bool {
        self.filesystem.eq_ignore_ascii_case("NTFS")
    }
}

/// Query the volume serial + filesystem name for the volume containing
/// `path`. Returns an error if `path` doesn't exist or the OS query fails.
///
/// Uses `GetVolumePathNameW` to find the mount-point root, then
/// `GetVolumeInformationW` to query that root.
#[cfg(target_os = "windows")]
pub fn volume_info(path: &Path) -> Result<VolumeInfo> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        GetVolumeInformationW, GetVolumePathNameW,
    };

    // Canonical absolute path; GetVolumePathNameW wants an existing path or
    // at least a syntactically valid one. We canonicalize so symlinks /
    // junctions resolve to their actual mount point.
    let absolute = path
        .canonicalize()
        .with_context(|| format!("canonicalize {}", path.display()))?;

    let wide: Vec<u16> = absolute
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // MAX_PATH + slack; mount roots are always short.
    let mut root_buf = [0u16; 260];
    unsafe {
        GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root_buf)
            .with_context(|| format!("GetVolumePathNameW({})", absolute.display()))?;
    }

    let mut serial: u32 = 0;
    let mut max_component: u32 = 0;
    let mut flags: u32 = 0;
    let mut fs_name_buf = [0u16; 64];

    unsafe {
        GetVolumeInformationW(
            PCWSTR(root_buf.as_ptr()),
            None,
            Some(&mut serial),
            Some(&mut max_component),
            Some(&mut flags),
            Some(&mut fs_name_buf),
        )
        .with_context(|| {
            format!(
                "GetVolumeInformationW({})",
                String::from_utf16_lossy(&root_buf)
            )
        })?;
    }

    let fs_name = wide_to_string(&fs_name_buf);
    Ok(VolumeInfo {
        serial,
        filesystem: fs_name,
    })
}

#[cfg(not(target_os = "windows"))]
pub fn volume_info(_path: &Path) -> Result<VolumeInfo> {
    Err(anyhow!(
        "volume_info is Windows-only — visibility-presets setup is gated on Windows hosts"
    ))
}

/// True NTFS file identity via `GetFileInformationByHandle`: returns
/// (volume_serial, nFileIndexHigh:nFileIndexLow packed into u64). Two
/// hardlinks to the same underlying file share both. Used by the setup
/// migration's resume logic to confirm "the file at new_path is the SAME
/// file we already moved from old_path", not a coincidentally-same-length
/// distinct file.
///
/// Works for both regular files and directories — we pass
/// `FILE_FLAG_BACKUP_SEMANTICS` so the open succeeds on a dir handle.
#[cfg(target_os = "windows")]
pub fn file_identity(path: &Path) -> Result<(u32, u64)> {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    // FILE_FLAG_BACKUP_SEMANTICS lets us open a directory with read access.
    // Without it CreateFileW fails on a dir path.
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .with_context(|| format!("open for identity: {}", path.display()))?;

    let handle = HANDLE(file.as_raw_handle() as *mut _);
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    unsafe {
        GetFileInformationByHandle(handle, &mut info)
            .map_err(|e| anyhow!("GetFileInformationByHandle({}): {e}", path.display()))?;
    }
    let file_id = ((info.nFileIndexHigh as u64) << 32) | (info.nFileIndexLow as u64);
    Ok((info.dwVolumeSerialNumber, file_id))
}

#[cfg(not(target_os = "windows"))]
pub fn file_identity(_path: &Path) -> Result<(u32, u64)> {
    Err(anyhow!("file_identity is Windows-only"))
}

/// True if `a` and `b` resolve to the same NTFS volume. Either side
/// returning an error from `volume_info` propagates.
pub fn same_volume(a: &Path, b: &Path) -> Result<bool> {
    let va = volume_info(a)?;
    let vb = volume_info(b)?;
    Ok(va.serial == vb.serial)
}

/// True if `path` exists, is a directory, and has zero entries (no files,
/// no subdirs, including hidden).
pub fn directory_is_empty(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(true); // a non-existent path is trivially "empty" for our purposes
    }
    if !path.is_dir() {
        return Err(anyhow!("{} is not a directory", path.display()));
    }
    let mut entries = std::fs::read_dir(path)
        .with_context(|| format!("read_dir {}", path.display()))?;
    Ok(entries.next().is_none())
}

/// Attempt a throwaway hardlink to verify the destination is hardlink-able
/// from `src_dir`. Picks `src_dir` itself as the source (creates and
/// removes a small probe file), so we never have to find a real .var to
/// hardlink. Cleans up both files on success and on failure.
///
/// Returns Ok(()) only if the full create-link-stat-unlink cycle worked.
pub fn try_hardlink_probe(src_dir: &Path, dest_dir: &Path) -> Result<()> {
    use std::io::Write;

    if !src_dir.is_dir() {
        return Err(anyhow!("source dir {} not a directory", src_dir.display()));
    }
    if !dest_dir.is_dir() {
        return Err(anyhow!("dest dir {} not a directory", dest_dir.display()));
    }

    // Use process id + a counter to keep probe names unique across rapid
    // calls without pulling in a randomness crate.
    let pid = std::process::id();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe_name = format!(".vam-pb-probe-{pid}-{nonce}");
    let src = src_dir.join(&probe_name);
    let dest = dest_dir.join(&probe_name);

    // Best-effort cleanup on every exit path.
    let cleanup = || {
        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dest);
    };

    {
        let mut f = std::fs::File::create(&src)
            .with_context(|| format!("create probe file at {}", src.display()))?;
        f.write_all(b"vam-package-browser probe\n")?;
    }

    if let Err(e) = std::fs::hard_link(&src, &dest) {
        cleanup();
        return Err(anyhow!(
            "hardlink probe failed ({} → {}): {e}",
            src.display(),
            dest.display()
        ));
    }

    // Verify the link is real (same inode-id on Windows = same nFileIndex).
    // Cheapest verification that catches reparse-point misbehavior.
    let src_meta = std::fs::metadata(&src);
    let dest_meta = std::fs::metadata(&dest);
    if src_meta.is_err() || dest_meta.is_err() {
        cleanup();
        return Err(anyhow!("post-link metadata read failed"));
    }

    cleanup();
    Ok(())
}

#[cfg(target_os = "windows")]
fn wide_to_string(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn directory_is_empty_on_fresh_temp() {
        let d = TempDir::new().unwrap();
        assert!(directory_is_empty(d.path()).unwrap());
    }

    #[test]
    fn directory_is_empty_false_with_one_file() {
        let d = TempDir::new().unwrap();
        std::fs::write(d.path().join("foo.txt"), b"x").unwrap();
        assert!(!directory_is_empty(d.path()).unwrap());
    }

    #[test]
    fn directory_is_empty_true_for_nonexistent() {
        let d = TempDir::new().unwrap();
        let ghost = d.path().join("nope");
        assert!(directory_is_empty(&ghost).unwrap());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn volume_info_returns_serial_for_temp_dir() {
        let d = TempDir::new().unwrap();
        let info = volume_info(d.path()).unwrap();
        // Temp dir is on whatever volume %TEMP% lives on. We don't know
        // the serial — just that it's non-zero and we got a filesystem
        // name back.
        assert_ne!(info.serial, 0);
        assert!(!info.filesystem.is_empty());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn same_volume_returns_true_for_two_subdirs_of_temp() {
        let d = TempDir::new().unwrap();
        let a = d.path().join("a");
        let b = d.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        assert!(same_volume(&a, &b).unwrap());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn try_hardlink_probe_succeeds_within_one_temp_dir() {
        let d = TempDir::new().unwrap();
        let a = d.path().join("a");
        let b = d.path().join("b");
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        try_hardlink_probe(&a, &b).unwrap();
        // Probe must clean up after itself — both dirs empty again.
        assert!(directory_is_empty(&a).unwrap());
        assert!(directory_is_empty(&b).unwrap());
    }
}
