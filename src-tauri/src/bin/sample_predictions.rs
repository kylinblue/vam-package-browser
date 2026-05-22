//! One-shot sampler for manual review of classifier predictions.
//!
//! Selects N packages where `hub_category IS NULL` (i.e. the prediction
//! came from one of our predictors, not from the hub), filters to those
//! with a non-null `predicted_hub_category`, samples them deterministically,
//! sorts by creator, and writes a Markdown file with one section per package.
//!
//! Each section shows: package identity + predicted category + method +
//! confidence + the scanner's package_type guess + family purpose +
//! kind:* tags. Enough to eyeball whether the predicted category is right
//! without having to open the .var.
//!
//! Read-only — no session lock needed (per the multi-session DB protocol,
//! readers don't lock; only writers do).
//!
//! Usage:
//!   sample_predictions [--db PATH] [-n COUNT] [--seed N] [--out PATH]

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OpenFlags};

const DEFAULT_N: usize = 30;
const DEFAULT_SEED: u64 = 0x5A_C0FFEE_BEEF;
const DEFAULT_OUT: &str = "classifier-review-sample.md";

#[derive(Debug, Default)]
struct Args {
    db: Option<PathBuf>,
    n: Option<usize>,
    seed: Option<u64>,
    out: Option<PathBuf>,
}

struct Pkg {
    id: i64,
    creator: String,
    package_name: String,
    version: String,
    description: Option<String>,
    package_type: String,
    family_id: Option<i64>,
    predicted: String,
    method: String,
    confidence: f64,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let n = args.n.unwrap_or(DEFAULT_N);
    let seed = args.seed.unwrap_or(DEFAULT_SEED);
    let out_path = args.out.clone().unwrap_or_else(|| PathBuf::from(DEFAULT_OUT));
    let db_path = args.db.clone().unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!("index db not found at {}", db_path.display()));
    }

    // Read-only handle — no migrations, no pragma writes. We don't need the
    // session lock for reads (per CLAUDE.md: SQLite WAL allows concurrent
    // readers cleanly; only writers must coordinate).
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("open read-only sqlite at {}", db_path.display()))?;

    // 1. Pull every predicted-but-unmatched candidate.
    let mut candidates: Vec<Pkg> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT id, creator, package_name, version, description, package_type,
                    family_id, predicted_hub_category, predicted_method, predicted_confidence
             FROM packages
             WHERE hub_category IS NULL
               AND predicted_hub_category IS NOT NULL",
        )?;
        let it = stmt.query_map([], |row| {
            Ok(Pkg {
                id: row.get(0)?,
                creator: row.get(1)?,
                package_name: row.get(2)?,
                version: row.get(3)?,
                description: row.get(4)?,
                package_type: row.get(5)?,
                family_id: row.get(6)?,
                predicted: row.get(7)?,
                method: row.get::<_, Option<String>>(8)?.unwrap_or_else(|| "?".to_string()),
                confidence: row.get::<_, Option<f64>>(9)?.unwrap_or(0.0),
            })
        })?;
        for r in it {
            candidates.push(r?);
        }
    }

    let total_candidates = candidates.len();
    if total_candidates == 0 {
        return Err(anyhow!("no predicted-but-unmatched packages found"));
    }
    if total_candidates < n {
        eprintln!(
            "warning: requested {} but only {} candidates exist — using all",
            n, total_candidates,
        );
    }
    let want = n.min(total_candidates);

    // 2. Deterministic shuffle (splitmix64-ish) so re-running with the same
    //    seed yields the same sample.
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    };
    for i in (1..candidates.len()).rev() {
        let j = (next() as usize) % (i + 1);
        candidates.swap(i, j);
    }
    candidates.truncate(want);

    // 3. Sort by creator (case-insensitive), then package_name.
    candidates.sort_by(|a, b| {
        a.creator
            .to_lowercase()
            .cmp(&b.creator.to_lowercase())
            .then_with(|| a.package_name.to_lowercase().cmp(&b.package_name.to_lowercase()))
    });

    // 4. Pull purpose + kind tags for each sampled family (one query each, n is
    //    tiny so we don't bother batching).
    let mut purposes: HashMap<i64, Option<String>> = HashMap::new();
    let mut kind_tags: HashMap<i64, Vec<String>> = HashMap::new();
    {
        let mut p_stmt = conn.prepare(
            "SELECT purpose FROM package_family WHERE id = ?1",
        )?;
        let mut t_stmt = conn.prepare(
            "SELECT tag FROM family_tags WHERE family_id = ?1 AND tag LIKE 'kind:%' ORDER BY tag",
        )?;
        for p in &candidates {
            let Some(fid) = p.family_id else {
                purposes.insert(0, None);
                continue;
            };
            if let std::collections::hash_map::Entry::Vacant(e) = purposes.entry(fid) {
                let purpose: Option<String> = p_stmt
                    .query_row(params![fid], |r| r.get(0))
                    .unwrap_or(None);
                e.insert(purpose);
            }
            if !kind_tags.contains_key(&fid) {
                let mut tags = Vec::new();
                let mut rows = t_stmt.query(params![fid])?;
                while let Some(row) = rows.next()? {
                    tags.push(row.get::<_, String>(0)?);
                }
                kind_tags.insert(fid, tags);
            }
        }
    }

    // 5. Write the Markdown report.
    let mut f = fs::File::create(&out_path)
        .with_context(|| format!("create {}", out_path.display()))?;
    writeln!(f, "# Classifier review sample")?;
    writeln!(f)?;
    writeln!(
        f,
        "Sampled {} of {} hub-unmatched packages with predictions \
         (seed `{:#x}`, deterministic). Sorted by creator.",
        want, total_candidates, seed
    )?;
    writeln!(f)?;
    writeln!(f, "For each row, mark `[ ]` → `[x]` correct or `[!]` wrong.")?;
    writeln!(f)?;

    // Summary table first — quick scan.
    writeln!(f, "## Summary")?;
    writeln!(f)?;
    writeln!(
        f,
        "| # | creator | package | version | predicted | conf | method | verdict |"
    )?;
    writeln!(
        f,
        "| - | ------- | ------- | ------- | --------- | ---: | ------ | ------- |"
    )?;
    for (i, p) in candidates.iter().enumerate() {
        writeln!(
            f,
            "| {} | `{}` | `{}` | `{}` | **{}** | {:.2} | `{}` | [ ] |",
            i + 1,
            md_escape(&p.creator),
            md_escape(&p.package_name),
            md_escape(&p.version),
            md_escape(&p.predicted),
            p.confidence,
            p.method,
        )?;
    }
    writeln!(f)?;

    // Per-package detail.
    writeln!(f, "## Details")?;
    writeln!(f)?;
    for (i, p) in candidates.iter().enumerate() {
        writeln!(f, "### {}. {}.{}.{}", i + 1, p.creator, p.package_name, p.version)?;
        writeln!(f)?;
        writeln!(f, "- **DB id:** {}", p.id)?;
        writeln!(f, "- **Predicted:** `{}` (conf {:.2}, via `{}`)", p.predicted, p.confidence, p.method)?;
        writeln!(f, "- **Scanner's package_type guess:** `{}`", p.package_type)?;
        let tags = p.family_id.and_then(|fid| kind_tags.get(&fid));
        if let Some(tags) = tags {
            if !tags.is_empty() {
                writeln!(f, "- **kind:* tags:** `{}`", tags.join("`, `"))?;
            } else {
                writeln!(f, "- **kind:* tags:** (none)")?;
            }
        } else {
            writeln!(f, "- **kind:* tags:** (no family)")?;
        }
        let purpose = p.family_id.and_then(|fid| purposes.get(&fid)).and_then(|o| o.clone());
        if let Some(purpose) = purpose {
            if !purpose.trim().is_empty() {
                writeln!(f, "- **Family purpose:** {}", oneline(&purpose))?;
            } else {
                writeln!(f, "- **Family purpose:** (empty)")?;
            }
        } else {
            writeln!(f, "- **Family purpose:** (none)")?;
        }
        if let Some(desc) = &p.description {
            let t = desc.trim();
            if !t.is_empty() {
                writeln!(f, "- **Package description:** {}", oneline(t))?;
            }
        }
        writeln!(f, "- **Verdict:** [ ] correct  [ ] wrong  [ ] unsure   _notes:_")?;
        writeln!(f)?;
    }

    eprintln!("wrote {} to {}", want, out_path.display());
    Ok(())
}

fn md_escape(s: &str) -> String {
    // Backticks are already the delimiter we use for code spans; replace
    // internal backticks so they don't break the table.
    s.replace('`', "'")
}

fn oneline(s: &str) -> String {
    // Markdown bullets eat raw newlines — flatten and trim noisy whitespace.
    let collapsed: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.len() > 400 {
        format!("{}…", &collapsed[..400])
    } else {
        collapsed
    }
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
            "-n" | "--count" => {
                i += 1;
                args.n = Some(
                    raw.get(i)
                        .ok_or_else(|| anyhow!("-n needs a value"))?
                        .parse()
                        .context("-n value must be a positive integer")?,
                );
            }
            "--seed" => {
                i += 1;
                let s = raw.get(i).ok_or_else(|| anyhow!("--seed needs a value"))?;
                args.seed = Some(if let Some(hex) = s.strip_prefix("0x") {
                    u64::from_str_radix(hex, 16).context("invalid hex seed")?
                } else {
                    s.parse().context("invalid decimal seed")?
                });
            }
            "--out" => {
                i += 1;
                args.out = Some(PathBuf::from(
                    raw.get(i).ok_or_else(|| anyhow!("--out needs a value"))?,
                ));
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: sample_predictions [--db PATH] [-n COUNT] [--seed N] [--out PATH]"
                );
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
