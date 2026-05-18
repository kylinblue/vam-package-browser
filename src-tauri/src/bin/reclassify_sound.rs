//! One-shot tool: re-runs `meta::classify` on packages whose stored
//! `package_type='Sound'` AND `scene_count>0`. This is the slice the
//! Scene-vs-Sound classifier patch changes; we touch only it so we don't have
//! to walk the full 4000+ .var library.
//!
//! Usage: `reclassify_sound [--db PATH] [--dry-run]`

use std::fs::OpenOptions;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use vam_package_browser_lib::{index, meta};
use zip::ZipArchive;

#[derive(Debug, Default)]
struct Args {
    db: Option<PathBuf>,
    dry_run: bool,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let db_path = args.db.clone().unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!("index db not found at {}", db_path.display()));
    }
    let mut conn = index::open_and_migrate(&db_path)
        .with_context(|| format!("open index at {}", db_path.display()))?;

    let candidates: Vec<(i64, String)> = conn
        .prepare(
            "SELECT id, var_path FROM packages
             WHERE package_type = 'Sound' AND scene_count > 0
             ORDER BY id",
        )?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;

    eprintln!("affected slice: {} packages", candidates.len());

    let mut changed = 0usize;
    let mut errors = 0usize;
    let mut breakdown: std::collections::BTreeMap<String, usize> = Default::default();

    let tx = conn.transaction()?;
    {
        let mut upd = tx.prepare_cached(
            "UPDATE packages SET package_type = ?1 WHERE id = ?2",
        )?;
        for (id, var_path) in &candidates {
            match reclassify(Path::new(var_path)) {
                Ok(new_type) => {
                    let new_str = new_type.as_str().to_string();
                    *breakdown.entry(new_str.clone()).or_default() += 1;
                    if new_str != "Sound" {
                        changed += 1;
                        if !args.dry_run {
                            upd.execute(rusqlite::params![new_str, id])?;
                        }
                    }
                }
                Err(e) => {
                    errors += 1;
                    eprintln!("  err id={id} {var_path}: {e:#}");
                }
            }
        }
    }
    if args.dry_run {
        eprintln!("(dry-run — no writes)");
    } else {
        tx.commit()?;
    }

    eprintln!(
        "\nreclassified {}/{} (errors: {})",
        changed,
        candidates.len(),
        errors
    );
    eprintln!("new package_type distribution within the slice:");
    for (ty, n) in &breakdown {
        eprintln!("  {ty:<10} {n}");
    }
    Ok(())
}

fn reclassify(var_path: &Path) -> Result<meta::PackageType> {
    let file = OpenOptions::new()
        .read(true)
        .write(false)
        .open(var_path)
        .with_context(|| format!("open {}", var_path.display()))?;
    let mut zip = ZipArchive::new(file).context("read zip")?;
    let mut entry = zip
        .by_name("meta.json")
        .context("meta.json not found in archive")?;
    let mut bytes = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut bytes)?;
    let parsed = meta::parse_meta(&bytes).context("parse meta.json")?;
    Ok(meta::classify(&parsed.content_list))
}

fn parse_args() -> Result<Args> {
    let mut args = Args::default();
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < raw.len() {
        match raw[i].as_str() {
            "--db" => {
                i += 1;
                args.db = Some(PathBuf::from(
                    raw.get(i).ok_or_else(|| anyhow!("--db needs a value"))?,
                ));
            }
            "--dry-run" => args.dry_run = true,
            "-h" | "--help" => {
                eprintln!("usage: reclassify_sound [--db PATH] [--dry-run]");
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }
    Ok(args)
}

fn default_db_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_default();
    PathBuf::from(appdata)
        .join("com.github.kylinblue.vam-package-browser")
        .join("index.sqlite")
}
