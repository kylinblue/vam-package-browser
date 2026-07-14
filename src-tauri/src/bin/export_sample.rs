//! Export a calibration sample of packages from the existing SQLite index as
//! JSONL — one record per line, ready to paste to Grok for tag-taxonomy
//! validation. Buckets spread across PackageType so a single pass exercises
//! both confirmed tags and intentional negatives.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use rusqlite::params;
use serde::Serialize;
use vam_package_browser_lib::{index, meta};
use zip::ZipArchive;

/// Counts by PackageType for the default ~50-package calibration sample.
/// Plugin gets the largest share since utility plugins are the primary thing
/// we want Grok to label well. Asset/SubScene/Mixed/Scene each contribute a
/// chunk because the location-vs-act-scene distinction lives in those types.
/// A small tail of Look/Clothing/Hair/Morph/Pose acts as out-of-scope
/// negatives — Grok should refuse to assign utility/location tags to them.
const CALIBRATION_BUCKETS: &[(&str, usize)] = &[
    ("Plugin", 15),
    ("Asset", 8),
    ("SubScene", 5),
    ("Mixed", 8),
    ("Scene", 8),
    ("Look", 2),
    ("Clothing", 1),
    ("Hair", 1),
    ("Morph", 1),
    ("Pose", 1),
];

/// Pilot sample: ~200 packages, distributed for statistical-meaningful pipeline
/// shakedown before the full ~5K production tagging pass. Same ROW_NUMBER
/// PARTITION BY creator mechanism as calibration, just larger N per bucket.
/// Excludes any packages already force-included via --include-jsonl.
const PILOT_BUCKETS: &[(&str, usize)] = &[
    ("Plugin", 75),
    ("Asset", 40),
    ("Mixed", 30),
    ("Scene", 25),
    ("SubScene", 10),
    ("Look", 6),
    ("Clothing", 5),
    ("Hair", 4),
    ("Morph", 3),
    ("Pose", 3),
];

/// V4 calibration pilot: family-level, OOS-heavy distribution to validate
/// multi-dimensional namespaced tagging on content kinds we haven't tested
/// (looks, clothing, hair, morphs, poses, prop assets, act scenes, etc.).
/// Small Plugin slice included for namespace-migration validation
/// (function:* should replace flat utility tags cleanly).
const PILOT_V4_BUCKETS: &[(&str, usize)] = &[
    ("Look", 25),
    ("Clothing", 25),
    ("Hair", 15),
    ("Morph", 15),
    ("Pose", 15),
    ("Asset", 25),
    ("Texture", 15),
    ("Scene", 20),
    ("SubScene", 10),
    ("Mixed", 15),
    ("Sound", 5),
    ("Plugin", 10),
];

#[derive(Debug, Serialize)]
struct SampleRecord {
    id: i64,
    var_filename: String,
    creator: String,
    package_name: String,
    version: String,
    package_type: String,
    description: Option<String>,
    instructions: Option<String>,
    content_summary: ContentSummary,
}

#[derive(Debug, Serialize)]
struct ContentSummary {
    total_files: usize,
    by_prefix: Vec<PrefixBucket>,
    notable_files: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PrefixBucket {
    prefix: String,
    count: usize,
    sample_files: Vec<String>,
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let mut out_path: Option<PathBuf> = None;
    let mut db_path: Option<PathBuf> = None;
    let mut pilot = false;
    let mut pilot_v4 = false;
    let mut include_jsonl: Option<PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| anyhow!("--out needs a path"))?;
                out_path = Some(PathBuf::from(v));
            }
            "--db" => {
                i += 1;
                let v = args.get(i).ok_or_else(|| anyhow!("--db needs a path"))?;
                db_path = Some(PathBuf::from(v));
            }
            "--pilot" => {
                pilot = true;
            }
            "--pilot-v4" => {
                pilot_v4 = true;
            }
            "--include-jsonl" => {
                i += 1;
                let v = args
                    .get(i)
                    .ok_or_else(|| anyhow!("--include-jsonl needs a path"))?;
                include_jsonl = Some(PathBuf::from(v));
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }

    if pilot && pilot_v4 {
        return Err(anyhow!("--pilot and --pilot-v4 are mutually exclusive"));
    }

    let buckets: &[(&str, usize)] = if pilot_v4 {
        PILOT_V4_BUCKETS
    } else if pilot {
        PILOT_BUCKETS
    } else {
        CALIBRATION_BUCKETS
    };
    let mode_label = if pilot_v4 {
        "pilot-v4 (family-level)"
    } else if pilot {
        "pilot"
    } else {
        "calibration"
    };

    let db_path = db_path.unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!(
            "index db not found at {}\n\
             (run a scan from the GUI first, or pass --db <path>)",
            db_path.display()
        ));
    }
    let conn = index::open_and_migrate(&db_path)
        .with_context(|| format!("open index at {}", db_path.display()))?;

    let force_include_ids: Vec<i64> = match include_jsonl.as_ref() {
        Some(p) => read_ids_from_jsonl(p)
            .with_context(|| format!("read force-include ids from {}", p.display()))?,
        None => Vec::new(),
    };

    let mut out: Box<dyn Write> = match &out_path {
        Some(p) => Box::new(
            OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(p)
                .with_context(|| format!("open output {}", p.display()))?,
        ),
        None => Box::new(io::stdout().lock()),
    };

    let mut total_written = 0usize;
    let mut total_skipped = 0usize;

    // Force-include pass: write the explicitly-listed ids first. These bypass
    // the bucket query and are excluded from subsequent bucket fills so they
    // can't appear twice.
    if !force_include_ids.is_empty() {
        eprintln!(
            "force-include: {} ids from {}",
            force_include_ids.len(),
            include_jsonl.as_ref().unwrap().display()
        );
        let mut wrote = 0usize;
        for id in &force_include_ids {
            match lookup_var_path(&conn, *id).and_then(|p| build_record(*id, &p)) {
                Ok(rec) => {
                    serde_json::to_writer(&mut out, &rec)?;
                    out.write_all(b"\n")?;
                    wrote += 1;
                    total_written += 1;
                }
                Err(e) => {
                    eprintln!("  skip force-include id {id}: {e:#}");
                    total_skipped += 1;
                }
            }
        }
        eprintln!(
            "{:>10}: {wrote:>3}/{} (force-included)",
            "include",
            force_include_ids.len()
        );
    }

    for (ty, n) in buckets {
        let rows = if pilot_v4 {
            select_family_bucket(&conn, ty, *n, &force_include_ids)
                .with_context(|| format!("select family bucket {ty}"))?
        } else {
            select_bucket(&conn, ty, *n, &force_include_ids)
                .with_context(|| format!("select bucket {ty}"))?
        };
        let found = rows.len();
        let mut wrote = 0usize;
        for (id, var_path) in rows {
            match build_record(id, &var_path) {
                Ok(rec) => {
                    serde_json::to_writer(&mut out, &rec)?;
                    out.write_all(b"\n")?;
                    wrote += 1;
                    total_written += 1;
                }
                Err(e) => {
                    eprintln!("  skip id {id} ({var_path}): {e:#}");
                    total_skipped += 1;
                }
            }
        }
        eprintln!("{ty:>10}: {wrote:>3}/{n} (db had {found})");
    }
    out.flush()?;
    eprintln!("---");
    eprintln!("mode: {mode_label}, total written: {total_written}, skipped: {total_skipped}");
    if let Some(p) = out_path {
        eprintln!("output: {}", p.display());
    }
    Ok(())
}

fn default_db_path() -> PathBuf {
    vam_package_browser_lib::paths::default_db_path()
}

/// Pick up to `n` packages of `ty`, spreading across creators via ROW_NUMBER()
/// partitioned by creator. First pass picks 1 package per creator, so a
/// 15-row plugin sample spans up to 15 distinct authors instead of dumping
/// many from one prolific creator. `exclude_ids` lets the pilot mode skip
/// packages already written via --include-jsonl so they don't appear twice.
fn select_bucket(
    conn: &rusqlite::Connection,
    ty: &str,
    n: usize,
    exclude_ids: &[i64],
) -> Result<Vec<(i64, String)>> {
    let exclude_clause = if exclude_ids.is_empty() {
        String::new()
    } else {
        let placeholders: Vec<&str> = (0..exclude_ids.len()).map(|_| "?").collect();
        format!(" AND id NOT IN ({})", placeholders.join(","))
    };
    let sql = format!(
        "WITH ranked AS (
            SELECT id, var_path, creator,
                   ROW_NUMBER() OVER (PARTITION BY creator ORDER BY id) AS rn
              FROM packages
             WHERE package_type = ?
               AND error IS NULL
               AND is_hidden = 0
               AND creator <> ''
               {exclude_clause}
         )
         SELECT id, var_path
           FROM ranked
          ORDER BY rn, creator
          LIMIT ?"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<rusqlite::types::Value> = Vec::with_capacity(exclude_ids.len() + 2);
    binds.push(rusqlite::types::Value::Text(ty.to_string()));
    for id in exclude_ids {
        binds.push(rusqlite::types::Value::Integer(*id));
    }
    binds.push(rusqlite::types::Value::Integer(n as i64));
    let params_ref: Vec<&dyn rusqlite::ToSql> =
        binds.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    let rows = stmt
        .query_map(params_ref.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Pick up to `n` package families whose LATEST version has package_type = ty.
/// Returns (family_id, var_path of latest) — content for the JSONL record
/// comes from the latest package, but the record's `id` field carries the
/// family_id so downstream operations (tag writes, embedding) attach to the
/// family.
///
/// Spreads across creators via ROW_NUMBER PARTITION BY pf.creator just like
/// select_bucket, so a 25-row Look sample spans up to 25 distinct creators.
fn select_family_bucket(
    conn: &rusqlite::Connection,
    ty: &str,
    n: usize,
    exclude_ids: &[i64],
) -> Result<Vec<(i64, String)>> {
    let exclude_clause = if exclude_ids.is_empty() {
        String::new()
    } else {
        let placeholders: Vec<&str> = (0..exclude_ids.len()).map(|_| "?").collect();
        format!(" AND pf.id NOT IN ({})", placeholders.join(","))
    };
    let sql = format!(
        "WITH ranked AS (
            SELECT pf.id, p.var_path, pf.creator,
                   ROW_NUMBER() OVER (PARTITION BY pf.creator ORDER BY pf.id) AS rn
              FROM package_family pf
              JOIN packages p ON p.id = pf.latest_package_id
             WHERE p.package_type = ?
               AND p.error IS NULL
               AND p.is_hidden = 0
               AND pf.creator <> ''
               {exclude_clause}
         )
         SELECT id, var_path
           FROM ranked
          ORDER BY rn, creator
          LIMIT ?"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut binds: Vec<rusqlite::types::Value> = Vec::with_capacity(exclude_ids.len() + 2);
    binds.push(rusqlite::types::Value::Text(ty.to_string()));
    for id in exclude_ids {
        binds.push(rusqlite::types::Value::Integer(*id));
    }
    binds.push(rusqlite::types::Value::Integer(n as i64));
    let params_ref: Vec<&dyn rusqlite::ToSql> =
        binds.iter().map(|v| v as &dyn rusqlite::ToSql).collect();

    let rows = stmt
        .query_map(params_ref.as_slice(), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Read package ids from a JSONL file (one JSON object per line, each with an
/// `id` integer field). Used by --include-jsonl to force-include the
/// calibration sample inside the pilot pass.
fn read_ids_from_jsonl(path: &Path) -> Result<Vec<i64>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let mut ids = Vec::new();
    for (line_no, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)
            .with_context(|| format!("parse {} line {}", path.display(), line_no + 1))?;
        let id = v
            .get("id")
            .and_then(|x| x.as_i64())
            .ok_or_else(|| anyhow!("missing id in {} line {}", path.display(), line_no + 1))?;
        ids.push(id);
    }
    Ok(ids)
}

fn lookup_var_path(conn: &rusqlite::Connection, id: i64) -> Result<String> {
    let path: String = conn
        .query_row(
            "SELECT var_path FROM packages WHERE id = ?1",
            params![id],
            |r| r.get(0),
        )
        .with_context(|| format!("lookup id {id} in packages"))?;
    Ok(path)
}

fn build_record(id: i64, var_path: &str) -> Result<SampleRecord> {
    let path = Path::new(var_path);
    let var_filename = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string();

    let (meta_data, content_list) = read_var(var_path)?;
    let summary = summarize_content(&content_list);
    let version = parse_version_from_filename(&var_filename).unwrap_or_default();
    let package_type = meta::classify(&content_list).as_str().to_string();

    Ok(SampleRecord {
        id,
        var_filename,
        creator: meta_data.creator_name,
        package_name: meta_data.package_name,
        version,
        package_type,
        description: meta_data.description.filter(|s| !s.trim().is_empty()),
        instructions: meta_data.instructions.filter(|s| !s.trim().is_empty()),
        content_summary: summary,
    })
}

fn read_var(var_path: &str) -> Result<(meta::PackageMeta, Vec<String>)> {
    let file = OpenOptions::new()
        .read(true)
        .write(false)
        .open(var_path)
        .with_context(|| format!("open .var read-only: {var_path}"))?;
    let mut zip = ZipArchive::new(file)
        .with_context(|| format!("read zip central dir: {var_path}"))?;

    let mut entries = Vec::with_capacity(zip.len());
    let mut meta_idx: Option<usize> = None;
    for i in 0..zip.len() {
        let e = zip.by_index_raw(i)?;
        let name = e.name().to_string();
        if meta_idx.is_none() && name.eq_ignore_ascii_case("meta.json") {
            meta_idx = Some(i);
        }
        entries.push(name);
    }
    let i = meta_idx.ok_or_else(|| anyhow!("no meta.json"))?;
    let mut entry = zip.by_index(i)?;

    const MAX: u64 = 4 * 1024 * 1024;
    if entry.size() > MAX {
        return Err(anyhow!("meta.json too large: {} bytes", entry.size()));
    }
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    let trimmed = if buf.len() >= 3 && &buf[..3] == [0xEF, 0xBB, 0xBF] {
        &buf[3..]
    } else {
        &buf[..]
    };
    let meta_data = meta::parse_meta(trimmed)?;
    Ok((meta_data, entries))
}

/// Summarize the contentList into a Grok-digestible shape: counts by
/// directory prefix (top 8) plus up to 20 distinctive filenames. Goal is
/// enough texture for the model to understand the package without dumping
/// 5000 paths from a giant asset pack.
fn summarize_content(content_list: &[String]) -> ContentSummary {
    // Pre-filter: zip central directories include explicit directory entries
    // (paths ending in '/'). Drop them — Grok wants files, and counting them
    // both inflates total_files and pollutes sample_files with directory names
    // ("AcidBubbles", "Morphs") instead of actual filenames.
    let content_list: Vec<String> = content_list
        .iter()
        .filter(|p| !p.replace('\\', "/").ends_with('/'))
        .cloned()
        .collect();
    let total_files = content_list.len();

    // Prefix groups: skip meta.json and root-level files (Preview.jpg, etc.) —
    // those are surfaced via notable_files. For everything else take up to 3
    // path segments but always leave at least one segment "below" so the
    // prefix is a directory, never the file itself.
    let mut groups: HashMap<String, (usize, Vec<String>)> = HashMap::new();
    for p in &content_list {
        let normalized = p.replace('\\', "/");
        if normalized.eq_ignore_ascii_case("meta.json") {
            continue;
        }
        let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() < 2 {
            continue;
        }
        let take_count = (segments.len() - 1).min(3);
        let prefix = segments[..take_count].join("/");
        let entry = groups.entry(prefix).or_insert_with(|| (0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < 2 {
            let basename = segments.last().copied().unwrap_or("").to_string();
            if !basename.is_empty() {
                entry.1.push(basename);
            }
        }
    }

    let mut by_prefix: Vec<PrefixBucket> = groups
        .into_iter()
        .map(|(prefix, (count, sample_files))| PrefixBucket {
            prefix,
            count,
            sample_files,
        })
        .collect();
    by_prefix.sort_by(|a, b| b.count.cmp(&a.count));
    by_prefix.truncate(8);

    // Notable filenames in two priority tiers: primary content-bearing exts
    // first, then loose .cs files as a fallback so single-file plugins like
    // AcidBubbles.BlendShapes still surface their script name.
    let primary_exts: &[&str] = &[".cslist", ".vap", ".vam", ".assetbundle", ".json"];
    let fallback_exts: &[&str] = &[".cs"];
    let mut notable: Vec<String> = Vec::new();
    for exts in [primary_exts, fallback_exts] {
        for p in &content_list {
            if notable.len() >= 20 {
                break;
            }
            let normalized = p.replace('\\', "/");
            let lower = normalized.to_lowercase();
            if lower == "meta.json" || lower.contains("/morphs/") {
                continue;
            }
            if !exts.iter().any(|e| lower.ends_with(e)) {
                continue;
            }
            let basename = normalized.rsplit('/').next().unwrap_or("").to_string();
            if basename.is_empty() || notable.contains(&basename) {
                continue;
            }
            notable.push(basename);
        }
        if notable.len() >= 20 {
            break;
        }
    }

    ContentSummary {
        total_files,
        by_prefix,
        notable_files: notable,
    }
}

fn parse_version_from_filename(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".var")?;
    let last_dot = stem.rfind('.')?;
    Some(stem[last_dot + 1..].to_string())
}

fn print_help() {
    eprintln!("Usage: export_sample [--pilot] [--include-jsonl <path>] [--out <path>] [--db <path>]");
    eprintln!();
    eprintln!("Writes a JSONL sample of packages to stdout (or --out file).");
    eprintln!("Picks N packages per PackageType bucket, spread across creators via");
    eprintln!("ROW_NUMBER PARTITION BY creator.");
    eprintln!();
    eprintln!("  --pilot               Use the larger ~200-record bucket distribution");
    eprintln!("                        (default: ~44-record calibration distribution)");
    eprintln!("  --pilot-v4            Family-level pilot for v4 namespaced taxonomy:");
    eprintln!("                        ~200 records biased toward currently-OOS content");
    eprintln!("                        kinds (looks, clothing, hair, morphs, props, etc.).");
    eprintln!("                        IDs in output are family_ids, not package_ids.");
    eprintln!("  --include-jsonl PATH  Force-include packages whose ids appear in this");
    eprintln!("                        JSONL file before the bucket pick runs. Bucket");
    eprintln!("                        queries then exclude these ids to prevent dupes.");
    eprintln!("  --out PATH            Write JSONL to file (default: stdout)");
    eprintln!("  --db PATH             Path to index.sqlite");
    eprintln!("                        (default: %APPDATA%/com.github.kylinblue.vam-package-browser/index.sqlite)");
}
