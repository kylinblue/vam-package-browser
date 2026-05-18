use std::fs::{File, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use image::imageops::FilterType;
use zip::ZipArchive;

const MAX_DIM: u32 = 512;
const QUALITY: f32 = 80.0;
const MAX_SRC_BYTES: u64 = 64 * 1024 * 1024; // refuse to decode preview images > 64 MB

pub fn thumb_path(thumbs_dir: &Path, package_id: i64) -> PathBuf {
    thumbs_dir.join(format!("{package_id}.webp"))
}

/// Per-image thumb cache path: `<thumbs_dir>/<package_id>/<hash>.webp`. The
/// hash is a fast (non-crypto) hash of the in-zip entry path — only needs to
/// be collision-resistant enough for one package's set of images.
pub fn sub_thumb_path(thumbs_dir: &Path, package_id: i64, entry: &str) -> PathBuf {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    entry.hash(&mut h);
    let hash_hex = format!("{:016x}", h.finish());
    thumbs_dir
        .join(format!("{package_id}"))
        .join(format!("{hash_hex}.webp"))
}

/// Returns true if a fresh thumbnail exists for the package whose source .var
/// has the given mtime — i.e. thumb file mtime ≥ source mtime.
pub fn is_fresh(thumbs_dir: &Path, package_id: i64, source_mtime_unix: i64) -> bool {
    let p = thumb_path(thumbs_dir, package_id);
    let Ok(md) = std::fs::metadata(&p) else { return false };
    let thumb_mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    thumb_mtime >= source_mtime_unix
}

/// Read-only extract `preview_entry` from `var_path`, resize to fit in MAX_DIM,
/// encode as lossy WebP, write atomically to `<thumbs_dir>/<package_id>.webp`.
pub fn generate(
    var_path: &Path,
    preview_entry: &str,
    thumbs_dir: &Path,
    package_id: i64,
) -> Result<()> {
    let out_path = thumb_path(thumbs_dir, package_id);
    generate_to(var_path, preview_entry, &out_path)
}

/// Same as `generate` but writes to an explicit output path. Used by the
/// per-image (sub) thumbnail generator.
pub fn generate_to(
    var_path: &Path,
    preview_entry: &str,
    out_path: &Path,
) -> Result<()> {
    // 1. Read source image bytes from zip (read-only).
    let file = OpenOptions::new()
        .read(true)
        .write(false)
        .open(var_path)
        .with_context(|| format!("open .var read-only: {}", var_path.display()))?;
    let mut zip = ZipArchive::new(file)
        .with_context(|| format!("read zip central dir: {}", var_path.display()))?;

    // HashMap-based lookup; avoids O(N) iteration through the central directory.
    let mut entry = zip
        .by_name(preview_entry)
        .with_context(|| format!("preview entry not in zip: {preview_entry}"))?;
    if entry.size() > MAX_SRC_BYTES {
        return Err(anyhow!(
            "preview image too large ({} bytes) in {}",
            entry.size(),
            var_path.display()
        ));
    }
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;
    drop(entry);
    drop(zip);

    generate_from_bytes(&bytes, out_path)
}

/// Decode raw image bytes (JPG/PNG/WebP) → resize to fit MAX_DIM →
/// atomic-write a WebP file at `out_path`. Used for both the zip-entry path
/// (above) and hub-downloaded preview icons.
pub fn generate_from_bytes(bytes: &[u8], out_path: &Path) -> Result<()> {
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create thumbs parent {}", parent.display()))?;
    }

    // 2. Decode + resize. Apply explicit decoder limits so a pathological
    // input (huge dimensions, intentional bomb image) can't allocate hundreds
    // of MB or hang the decoder thread.
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .context("guess image format")?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let img = reader.decode().context("decode preview bytes")?;
    let (w, h) = (img.width(), img.height());
    let scaled = if w <= MAX_DIM && h <= MAX_DIM {
        img.to_rgb8()
    } else {
        img.resize(MAX_DIM, MAX_DIM, FilterType::Lanczos3).to_rgb8()
    };
    let (sw, sh) = (scaled.width(), scaled.height());

    // 3. Encode lossy WebP.
    let encoder = webp::Encoder::from_rgb(scaled.as_raw(), sw, sh);
    let webp_data = encoder.encode(QUALITY);

    // 4. Atomic write: .webp.tmp → .webp.
    let tmp_path = out_path.with_extension("webp.tmp");
    {
        let mut out = File::create(&tmp_path)
            .with_context(|| format!("create tmp thumb {}", tmp_path.display()))?;
        out.write_all(&webp_data)?;
        out.sync_all()?;
    }
    // ReplaceFileW semantics are what we want on Windows: overwrite existing.
    std::fs::rename(&tmp_path, out_path).with_context(|| {
        format!(
            "rename tmp thumb {} -> {}",
            tmp_path.display(),
            out_path.display()
        )
    })?;
    Ok(())
}
