use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rayon::prelude::*;
use rusqlite::{params, Connection};
use serde::Serialize;
use walkdir::WalkDir;
use zip::ZipArchive;

use crate::deps;
use crate::fsutil;
use crate::meta::{self, PackageMeta, PackageType, PreviewableCounts};
use crate::tagging::family;

#[derive(Debug, Serialize)]
pub struct ScanResult {
    pub scanned: usize,
    pub errors: usize,
    pub elapsed_ms: u128,
}

#[derive(Debug)]
struct ScannedPackage {
    var_path: PathBuf,
    file_size: i64,
    file_mtime: i64,
    /// Max last-modified timestamp across all entries inside the .var
    /// (i.e. when the author zipped it). Distinct from `file_mtime` which is
    /// the outer .var's NTFS mtime. 0 if no entry had a parseable timestamp.
    package_mtime: i64,
    meta: Option<PackageMeta>,
    package_type: PackageType,
    preview_path: Option<String>,
    counts: PreviewableCounts,
    /// True when the .var is a directory rather than a ZIP archive.
    /// Drives the materialization branch (junction vs hardlink).
    is_directory_package: bool,
    error: Option<String>,
}

/// Walk the root for *.var files, parse each in parallel, then bulk-insert into SQLite.
/// `limit` caps the number of files inspected — useful for fast sampled scans.
/// `thumbs_dir` is used to invalidate stale thumbnail caches when a package's
/// `preview_path` changes (e.g. after we improved the picker).
pub fn scan(
    conn: &mut Connection,
    addon_root: &Path,
    thumbs_dir: &Path,
    limit: Option<usize>,
) -> Result<ScanResult> {
    if !addon_root.exists() {
        return Err(anyhow!("addon root does not exist: {}", addon_root.display()));
    }
    let start = Instant::now();

    // 1. Enumerate .var entries (both files and directories). VaM accepts
    // either form, so we do too. For directory packages we yield the dir
    // itself and call `skip_current_dir` to avoid walking into it (otherwise
    // every internal file would be a noisy candidate). Order is stable
    // because we sort the resulting paths.
    let mut var_files: Vec<PathBuf> = Vec::new();
    let mut it = WalkDir::new(addon_root).into_iter();
    while let Some(entry) = it.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let ext_is_var = entry
            .path()
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("var"))
            .unwrap_or(false);
        if !ext_is_var {
            continue;
        }
        let ft = entry.file_type();
        if ft.is_file() {
            var_files.push(entry.into_path());
        } else if ft.is_dir() {
            var_files.push(entry.into_path());
            // Don't descend INTO a .var directory — its contents are the
            // package's payload, not more package candidates.
            it.skip_current_dir();
        }
    }

    var_files.sort();
    if let Some(n) = limit {
        var_files.truncate(n);
    }

    // 2. Parse meta from each .var in parallel. Errors are captured per-file, not fatal.
    let scanned: Vec<ScannedPackage> = var_files
        .par_iter()
        .map(|path| parse_one(path))
        .collect();

    // 3. Bulk-insert into SQLite in a single transaction. Single connection, single writer.
    let tx = conn.transaction()?;
    let errors = {
        let mut errs = 0usize;
        let mut up_pkg = tx.prepare_cached(
            "INSERT INTO packages
                (var_path, file_size, file_mtime, creator, package_name, version,
                 license, program_version, description, package_type,
                 content_count, dep_count, has_preview, scanned_at, error,
                 preview_path,
                 scene_count, look_count, plugin_count, clothing_count,
                 hair_count, pose_count, subscene_count, package_mtime,
                 instructions, is_directory_package)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                     ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26)
             ON CONFLICT(var_path) DO UPDATE SET
                file_size            = excluded.file_size,
                file_mtime           = excluded.file_mtime,
                creator              = excluded.creator,
                package_name         = excluded.package_name,
                version              = excluded.version,
                license              = excluded.license,
                program_version      = excluded.program_version,
                description          = excluded.description,
                -- Preserve a user-set package_type override across rescans.
                -- The scanner always proposes its heuristic classification
                -- (`excluded.package_type`), but if the user has flagged a
                -- manual override, we keep what's already in the row.
                package_type         = CASE WHEN package_type_manual = 1
                                            THEN package_type
                                            ELSE excluded.package_type END,
                content_count        = excluded.content_count,
                dep_count            = excluded.dep_count,
                has_preview          = excluded.has_preview,
                scanned_at           = excluded.scanned_at,
                error                = excluded.error,
                preview_path         = excluded.preview_path,
                scene_count          = excluded.scene_count,
                look_count           = excluded.look_count,
                plugin_count         = excluded.plugin_count,
                clothing_count       = excluded.clothing_count,
                hair_count           = excluded.hair_count,
                pose_count           = excluded.pose_count,
                subscene_count       = excluded.subscene_count,
                package_mtime        = excluded.package_mtime,
                instructions         = excluded.instructions,
                is_directory_package = excluded.is_directory_package",
        )?;
        let mut sel_id = tx.prepare_cached("SELECT id FROM packages WHERE var_path = ?1")?;
        let mut sel_prev = tx.prepare_cached(
            "SELECT id, preview_path FROM packages WHERE var_path = ?1",
        )?;
        let mut del_deps = tx.prepare_cached("DELETE FROM package_dependencies WHERE package_id = ?1")?;
        let mut ins_dep  = tx.prepare_cached(
            "INSERT OR IGNORE INTO package_dependencies(package_id, dep_key) VALUES (?1, ?2)",
        )?;

        let now = unix_secs(SystemTime::now());
        for p in &scanned {
            if p.error.is_some() {
                errs += 1;
            }
            let (creator, name, license, prog, desc, instr, content_count, dep_count, deps) = match &p.meta {
                Some(m) => (
                    m.creator_name.clone(),
                    m.package_name.clone(),
                    m.license_type.clone(),
                    m.program_version.clone(),
                    m.description.clone(),
                    m.instructions.clone(),
                    m.content_list.len() as i64,
                    m.dependencies.len() as i64,
                    m.dependencies.clone(),
                ),
                None => (
                    String::new(),
                    String::new(),
                    None,
                    None,
                    None,
                    None,
                    0,
                    0,
                    vec![],
                ),
            };
            let version = parse_version_from_filename(&p.var_path).unwrap_or_default();
            let var_path_str = p.var_path.to_string_lossy().to_string();

            // If this package was previously scanned and its preview_path
            // changed, invalidate the cached primary thumb + any per-image
            // sub-thumbs (those were tied to the OLD picker choice).
            if let Ok(prev) = sel_prev.query_row(
                params![var_path_str],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)),
            ) {
                let (old_id, old_preview) = prev;
                if old_preview.as_deref() != p.preview_path.as_deref() {
                    let primary = thumbs_dir.join(format!("{old_id}.webp"));
                    let _ = std::fs::remove_file(&primary);
                    let sub_dir = thumbs_dir.join(format!("{old_id}"));
                    let _ = std::fs::remove_dir_all(&sub_dir);
                }
            }

            let has_preview = p.preview_path.is_some() as i64;
            up_pkg.execute(params![
                var_path_str,
                p.file_size,
                p.file_mtime,
                creator,
                name,
                version,
                license,
                prog,
                desc,
                p.package_type.as_str(),
                content_count,
                dep_count,
                has_preview,
                now,
                p.error.clone(),
                p.preview_path.clone(),
                p.counts.scene as i64,
                p.counts.look as i64,
                p.counts.plugin as i64,
                p.counts.clothing as i64,
                p.counts.hair as i64,
                p.counts.pose as i64,
                p.counts.subscene as i64,
                p.package_mtime,
                instr,
                p.is_directory_package as i64,
            ])?;

            let pkg_id: i64 = sel_id.query_row(params![var_path_str], |row| row.get(0))?;
            del_deps.execute(params![pkg_id])?;
            for d in &deps {
                ins_dep.execute(params![pkg_id, d])?;
            }
        }
        errs
    };

    // Resolve raw dep keys into local package_id edges. Runs inside the same
    // transaction so a failed resolve rolls back the entire scan rather than
    // leaving package_dep_links out of sync with package_dependencies.
    deps::resolve_all(&tx)?;
    tx.commit()?;

    // Auto-link any newly-scanned package to its package_family row.
    // Lives *outside* the scan transaction because family::recompute opens
    // its own internal transaction (SQLite doesn't allow nested BEGIN), and
    // because the operation is idempotent: if it fails partway, re-running
    // the scan (or `tag_library --recompute-families`) makes it whole. This
    // closes the wiring gap that previously left packages with NULL
    // family_id between scans and the next manual recompute, making them
    // invisible to the classifier predictors (kind-vote and embed-knn both
    // need family_id).
    family::recompute(conn)?;

    Ok(ScanResult {
        scanned: scanned.len(),
        errors,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

fn parse_one(path: &Path) -> ScannedPackage {
    let (stat_size, file_mtime) = stat(path).unwrap_or((0, 0));
    let is_dir = path.is_dir();
    // For directories, std::fs::metadata returns ~0 for len. UI / sort
    // expect a meaningful "size of the package" so we sum recursively.
    let file_size = if is_dir { dir_recursive_size(path) } else { stat_size };

    let result = if is_dir {
        read_meta_and_entries_from_dir(path)
    } else {
        read_meta_and_entries(path)
    };

    match result {
        Ok((mut meta, entries, package_mtime)) => {
            // Classify on contentList (author's official content list — best
            // for type intent). Pick preview & count items from the entry
            // list (catches files hidden behind directory-style contentList
            // entries, which is how many clothing/hair packages bundle
            // their previews).
            let ty = meta::classify(&meta.content_list);
            let preview_path = meta::pick_preview(&entries);
            let counts = meta::previewable_counts(&entries);

            // Merge in any extra dep keys from the .var.depend.txt sidecar.
            // meta.json is author-declared; the sidecar is VaM's runtime
            // resolution and is often a superset (it captures dynamically
            // discovered deps the author didn't enumerate). The bulk
            // insert uses INSERT OR IGNORE so dupes are cheap, but we
            // dedup here too for cleanliness.
            let sidecar_keys = read_sidecar_dep_keys(path);
            if !sidecar_keys.is_empty() {
                use std::collections::HashSet;
                let existing: HashSet<String> =
                    meta.dependencies.iter().cloned().collect();
                for k in sidecar_keys {
                    if !existing.contains(&k) {
                        meta.dependencies.push(k);
                    }
                }
            }

            ScannedPackage {
                var_path: path.to_path_buf(),
                file_size,
                file_mtime,
                package_mtime,
                meta: Some(meta),
                package_type: ty,
                preview_path,
                counts,
                is_directory_package: is_dir,
                error: None,
            }
        }
        Err(e) => ScannedPackage {
            var_path: path.to_path_buf(),
            file_size,
            file_mtime,
            package_mtime: 0,
            meta: None,
            package_type: PackageType::Unknown,
            preview_path: None,
            counts: PreviewableCounts::default(),
            is_directory_package: is_dir,
            error: Some(format!("{e:#}")),
        },
    }
}

/// Open .var as read-only zip; read meta.json and collect every entry's path.
/// File bodies (other than meta.json) are never read. Entry names come from
/// the central directory which `ZipArchive::new` already loaded, so this is
/// only a couple ms of extra work per package.
///
/// Also returns the max `last_modified` timestamp across all entries — used as
/// the "package was zipped at" date, independent of when the .var file was
/// downloaded/touched on disk. Returned as unix seconds, 0 if no entry had a
/// valid DOS timestamp.
fn read_meta_and_entries(path: &Path) -> Result<(PackageMeta, Vec<String>, i64)> {
    let file = OpenOptions::new()
        .read(true)
        .write(false)
        .open(path)
        .with_context(|| format!("open .var read-only: {}", path.display()))?;
    let mut zip = ZipArchive::new(file)
        .with_context(|| format!("read zip central dir: {}", path.display()))?;

    // Single pass over the central directory: collect every entry name, find
    // meta.json, and track the latest entry timestamp.
    let mut entries: Vec<String> = Vec::with_capacity(zip.len());
    let mut meta_idx: Option<usize> = None;
    let mut max_mtime: i64 = 0;
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i)?;
        let name = entry.name().to_string();
        if meta_idx.is_none() && name.eq_ignore_ascii_case("meta.json") {
            meta_idx = Some(i);
        }
        if let Some(t) = entry.last_modified().and_then(dos_dt_to_unix) {
            if t > max_mtime {
                max_mtime = t;
            }
        }
        entries.push(name);
    }
    let i = meta_idx.ok_or_else(|| anyhow!("meta.json missing"))?;
    let mut entry = zip.by_index(i)?;

    // Cap meta.json read to a sane size — should never be more than a few hundred KB.
    const MAX: u64 = 4 * 1024 * 1024;
    if entry.size() > MAX {
        return Err(anyhow!("meta.json suspiciously large: {} bytes", entry.size()));
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;

    let trimmed = strip_utf8_bom(&buf);
    let meta = meta::parse_meta(trimmed)
        .with_context(|| format!("parse meta.json from {}", path.display()))?;
    Ok((meta, entries, max_mtime))
}

/// Directory-form package: parse `<dir>/meta.json` directly from the filesystem
/// and walk the dir for content entries. Output shape matches
/// `read_meta_and_entries` so the calling pipeline is shape-symmetric across
/// file vs. directory packages.
///
/// Entry names use forward-slash separators to match ZIP convention (which the
/// downstream `classify` / `pick_preview` / `previewable_counts` functions
/// already canonicalize via `replace('\\', "/")`).
fn read_meta_and_entries_from_dir(dir: &Path) -> Result<(PackageMeta, Vec<String>, i64)> {
    // Locate meta.json case-insensitively at the directory root.
    let meta_path = WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.file_type().is_file()
                && e.file_name()
                    .to_str()
                    .map(|s| s.eq_ignore_ascii_case("meta.json"))
                    .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .ok_or_else(|| anyhow!("meta.json missing in directory package {}", dir.display()))?;

    let raw = std::fs::read(&meta_path)
        .with_context(|| format!("read {}", meta_path.display()))?;
    const MAX: usize = 4 * 1024 * 1024;
    if raw.len() > MAX {
        return Err(anyhow!(
            "meta.json suspiciously large: {} bytes",
            raw.len()
        ));
    }
    let trimmed = strip_utf8_bom(&raw);
    let meta = meta::parse_meta(trimmed)
        .with_context(|| format!("parse meta.json from {}", meta_path.display()))?;

    // Walk the directory for every file (any depth) and capture relative
    // forward-slash paths. Also track the latest file mtime — the directory-
    // package analogue of "max central-directory entry timestamp" for ZIPs.
    let mut entries: Vec<String> = Vec::new();
    let mut max_mtime: i64 = 0;
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = match entry.path().strip_prefix(dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        if !rel_str.is_empty() {
            entries.push(rel_str);
        }
        if let Ok(md) = entry.metadata() {
            if let Ok(modified) = md.modified() {
                if let Ok(d) = modified.duration_since(UNIX_EPOCH) {
                    let t = d.as_secs() as i64;
                    if t > max_mtime {
                        max_mtime = t;
                    }
                }
            }
        }
    }

    Ok((meta, entries, max_mtime))
}

/// Convert a zip entry's stored DOS time to unix seconds, treating the wall
/// clock as UTC (zip DOS times carry no tz). Returns None for out-of-range
/// timestamps (DOS time only spans 1980–2107).
fn dos_dt_to_unix(dt: zip::DateTime) -> Option<i64> {
    let y = dt.year();
    let m = dt.month();
    let d = dt.day();
    if y < 1980 || m == 0 || m > 12 || d == 0 || d > 31 {
        return None;
    }
    Some(ymdhms_to_unix(y, m, d, dt.hour(), dt.minute(), dt.second()))
}

/// Civil date → unix seconds (UTC). Hinnant's days_from_civil algorithm.
fn ymdhms_to_unix(y: u16, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> i64 {
    let y = y as i32 - if mo <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = (y - era * 400) as u32;
    let m = mo as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as u32 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146097 + doe as i64 - 719468;
    days * 86400 + h as i64 * 3600 + mi as i64 * 60 + s as i64
}

fn strip_utf8_bom(buf: &[u8]) -> &[u8] {
    if buf.len() >= 3 && &buf[0..3] == [0xEF, 0xBB, 0xBF] {
        &buf[3..]
    } else {
        buf
    }
}

/// Read `<var_path>.var.depend.txt` (VaM's runtime-resolved dep sidecar)
/// and return the dep keys it lists. Format: one entry per line, first
/// whitespace-separated token is the `Author.Package.Version` key. The
/// rest of the line is author/license/link metadata we ignore.
///
/// Returns an empty Vec on any failure (file missing, unreadable,
/// malformed) — sidecars are a best-effort augmentation, never load-
/// critical. Caller treats absent and empty identically.
fn read_sidecar_dep_keys(var_path: &Path) -> Vec<String> {
    const MAX_BYTES: u64 = 1 * 1024 * 1024;
    let sidecar = fsutil::sidecar_path(var_path);
    let md = match std::fs::metadata(&sidecar) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    if md.len() > MAX_BYTES {
        return Vec::new();
    }
    let body = match std::fs::read_to_string(&sidecar) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let token = trimmed.split_whitespace().next().unwrap_or("");
        if token.is_empty() {
            continue;
        }
        // Cheap sanity check: dep keys are Author.Package.Version, all
        // ASCII, no whitespace. Reject obvious junk (comment lines,
        // headers, etc.) without trying to fully validate.
        if token.contains('.') {
            out.push(token.to_string());
        }
    }
    out
}

fn stat(path: &Path) -> Option<(i64, i64)> {
    let md = std::fs::metadata(path).ok()?;
    let size = md.len() as i64;
    let mtime = md
        .modified()
        .ok()
        .map(unix_secs)
        .unwrap_or(0);
    Some((size, mtime))
}

/// Sum every regular-file byte under `dir` (recursive). Used for the
/// directory-package case so the UI shows a meaningful size instead of
/// the bare dir-entry size (which is ~0 on NTFS).
fn dir_recursive_size(dir: &Path) -> i64 {
    let mut total: i64 = 0;
    for entry in WalkDir::new(dir).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() {
            if let Ok(md) = entry.metadata() {
                total = total.saturating_add(md.len() as i64);
            }
        }
    }
    total
}

fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `Author.Package.<version>.var` → "<version>" (left empty if unparseable).
fn parse_version_from_filename(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    let last_dot = stem.rfind('.')?;
    Some(stem[last_dot + 1..].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn sidecar_parser_extracts_first_token_per_line() {
        let tmp = TempDir::new().unwrap();
        let var = tmp.path().join("Alice.Foo.1.var");
        std::fs::write(&var, b"fake var").unwrap();
        // Real-world sample: dep key + padding + "By: ..." metadata.
        let body = "\
AcidBubbles.Embody.58                         By: AcidBubbles  License: CC BY-SA  Link: https://github.com/acidbubbles/vam-embody
AcidBubbles.Timeline.214                      By: AcidBubbles  License: CC BY-SA
everlaster.TittyMagic.35                      By: everlaster   License: PC EA
everlaster.TittyMagic.9                       By: everlaster   License: CC BY-SA
hazmhox.vamlaunch.2                           By: hazmhox      License: FC

   \t  \n\
LFE.SoundtrackSync0.4                         By: LFE          License: CC BY-SA
TGC.Clothing_Ravenous_SecretVenusNo3.latest   By: TGC          License: CC BY-SA
";
        std::fs::write(
            tmp.path().join("Alice.Foo.1.var.depend.txt"),
            body.as_bytes(),
        )
        .unwrap();

        let keys = read_sidecar_dep_keys(&var);
        assert_eq!(
            keys,
            vec![
                "AcidBubbles.Embody.58",
                "AcidBubbles.Timeline.214",
                "everlaster.TittyMagic.35",
                "everlaster.TittyMagic.9",
                "hazmhox.vamlaunch.2",
                "LFE.SoundtrackSync0.4",
                "TGC.Clothing_Ravenous_SecretVenusNo3.latest",
            ],
            "should pull the first whitespace token from every non-empty line"
        );
    }

    #[test]
    fn sidecar_parser_returns_empty_when_absent() {
        let tmp = TempDir::new().unwrap();
        let var = tmp.path().join("Alice.Foo.1.var");
        std::fs::write(&var, b"fake var").unwrap();
        let keys = read_sidecar_dep_keys(&var);
        assert!(keys.is_empty());
    }

    #[test]
    fn sidecar_parser_rejects_lines_without_dots() {
        // Defensive: header / comment lines that lack the
        // Author.Package.Version dot structure are skipped.
        let tmp = TempDir::new().unwrap();
        let var = tmp.path().join("Alice.Foo.1.var");
        std::fs::write(&var, b"fake var").unwrap();
        std::fs::write(
            tmp.path().join("Alice.Foo.1.var.depend.txt"),
            b"# This is a header\nAcidBubbles.Embody.58\nGARBAGE\nReal.Pkg.1\n",
        )
        .unwrap();

        let keys = read_sidecar_dep_keys(&var);
        // "# This is a header" -> "#" -> no dot, rejected
        // "AcidBubbles.Embody.58" -> kept
        // "GARBAGE" -> no dot, rejected
        // "Real.Pkg.1" -> kept
        assert_eq!(keys, vec!["AcidBubbles.Embody.58", "Real.Pkg.1"]);
    }
}

