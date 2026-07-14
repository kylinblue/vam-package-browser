//! Phase 0.5: predict `hub_category` for packages with no hub match.
//!
//! Trains a per-tag voting model on the labeled slice (rows where hub_category
//! is set), then writes `predicted_hub_category`/`predicted_method`/
//! `predicted_confidence` for unlabeled rows (rows where hub_category IS NULL).
//!
//! Model: for each `kind:*` tag attached to a row's family, look up the
//! conditional P(hub_category | kind) learned from labeled rows; sum
//! distributions across the row's kind set; argmax wins. Confidence is the
//! normalized score of the winning category (winning_score / total_score).
//!
//! Honors the multi-session DB write-lock at
//! `%APPDATA%/com.github.kylinblue.vam-package-browser/.session-active.lock`.
//!
//! Usage: `predict_categories [--db PATH] [--dry-run]`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::params;
use vam_package_browser_lib::{holdout, index};

const PREDICTED_METHOD: &str = "kind-vote";

#[derive(Debug, Default)]
struct Args {
    db: Option<PathBuf>,
    dry_run: bool,
    holdout_test: bool,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let db_path = args.db.clone().unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!("index db not found at {}", db_path.display()));
    }

    let db_dir = db_path
        .parent()
        .ok_or_else(|| anyhow!("db path has no parent dir"))?
        .to_path_buf();
    let op = if args.holdout_test {
        "predict_categories (--holdout-test, read-only eval)"
    } else {
        "predict_categories (Phase 0.5 kind-vote)"
    };
    let _lock = SessionLock::acquire(&db_dir, op)?;

    let mut conn = index::open_and_migrate(&db_path)
        .with_context(|| format!("open index at {}", db_path.display()))?;

    // 1. Load (package_id, family_id, kinds, hub_category) for every row that
    //    has a family with at least one kind:* tag. Group concat the tags into
    //    a |-list so we do one query instead of N. family_id is required by
    //    --holdout-test mode (split is by family, not by package).
    #[derive(Clone)]
    struct Row {
        id: i64,
        family_id: Option<i64>,
        kinds: Vec<String>,
        hub: Option<String>,
    }
    let mut rows: Vec<Row> = Vec::new();
    {
        let mut stmt = conn.prepare(
            "SELECT p.id, p.family_id, p.hub_category,
                    (SELECT GROUP_CONCAT(ft.tag, '|') FROM family_tags ft
                     WHERE ft.family_id = p.family_id AND ft.tag LIKE 'kind:%') AS kinds
             FROM packages p",
        )?;
        let it = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let family_id: Option<i64> = row.get(1)?;
            let hub: Option<String> = row.get(2)?;
            let kinds_str: Option<String> = row.get(3)?;
            let kinds: Vec<String> = kinds_str
                .map(|s| s.split('|').map(|x| x.to_string()).collect())
                .unwrap_or_default();
            Ok(Row { id, family_id, kinds, hub })
        })?;
        for r in it {
            rows.push(r?);
        }
    }

    if args.holdout_test {
        let tuples: Vec<(Option<i64>, Option<String>, Vec<String>)> = rows
            .iter()
            .map(|r| (r.family_id, r.hub.clone(), r.kinds.clone()))
            .collect();
        return run_holdout_test(&tuples);
    }

    let labeled: Vec<(i64, Vec<String>, String)> = rows
        .iter()
        .filter(|r| r.hub.is_some())
        .map(|r| (r.id, r.kinds.clone(), r.hub.clone().unwrap()))
        .collect();
    let unlabeled: Vec<(i64, Vec<String>)> = rows
        .iter()
        .filter(|r| r.hub.is_none())
        .map(|r| (r.id, r.kinds.clone()))
        .collect();

    eprintln!(
        "loaded {} rows: {} labeled, {} unlabeled",
        rows.len(),
        labeled.len(),
        unlabeled.len()
    );

    // 2. Train: joint[kind][hub] = count, kind_total[kind] = count
    let mut joint: HashMap<String, HashMap<String, u32>> = HashMap::new();
    let mut kind_total: HashMap<String, u32> = HashMap::new();
    for (_, kinds, hub) in &labeled {
        for k in kinds {
            *joint
                .entry(k.clone())
                .or_default()
                .entry(hub.clone())
                .or_insert(0) += 1;
            *kind_total.entry(k.clone()).or_insert(0) += 1;
        }
    }

    // 3. Predict for unlabeled. Score each candidate hub by summing
    //    P(hub | kind_i) over the row's kinds; confidence = winning_score /
    //    sum_of_scores.
    struct Prediction {
        package_id: i64,
        predicted: String,
        confidence: f64,
    }
    let mut predictions: Vec<Prediction> = Vec::new();
    let mut no_kind = 0usize;
    let mut empty_score = 0usize;
    let mut dist: HashMap<String, u32> = HashMap::new();

    for (pid, kinds) in &unlabeled {
        if kinds.is_empty() {
            no_kind += 1;
            continue;
        }
        let mut score: HashMap<String, f64> = HashMap::new();
        for k in kinds {
            let n = match kind_total.get(k) {
                Some(&n) if n > 0 => n as f64,
                _ => continue,
            };
            if let Some(hubs) = joint.get(k) {
                for (hub, &c) in hubs {
                    *score.entry(hub.clone()).or_insert(0.0) += c as f64 / n;
                }
            }
        }
        if score.is_empty() {
            empty_score += 1;
            continue;
        }
        let total: f64 = score.values().sum();
        let (best_hub, best_score) = score
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        let confidence = best_score / total;
        *dist.entry(best_hub.clone()).or_insert(0) += 1;
        predictions.push(Prediction {
            package_id: *pid,
            predicted: best_hub.clone(),
            confidence,
        });
    }

    eprintln!(
        "predictions: {} ({} no-kind families, {} kinds-with-no-training-signal)",
        predictions.len(),
        no_kind,
        empty_score
    );
    eprintln!("predicted distribution:");
    let mut dist_vec: Vec<_> = dist.iter().collect();
    dist_vec.sort_by(|a, b| b.1.cmp(a.1));
    for (h, c) in &dist_vec {
        eprintln!("  {:<25} {}", h, c);
    }

    // 4. Write to DB. Single transaction, single UPDATE statement.
    if args.dry_run {
        eprintln!("(dry-run — no writes)");
        return Ok(());
    }
    let tx = conn.transaction()?;
    {
        let mut upd = tx.prepare_cached(
            "UPDATE packages
             SET predicted_hub_category = ?1,
                 predicted_method       = ?2,
                 predicted_confidence   = ?3
             WHERE id = ?4 AND hub_category IS NULL",
        )?;
        for p in &predictions {
            upd.execute(params![
                p.predicted,
                PREDICTED_METHOD,
                p.confidence,
                p.package_id,
            ])?;
        }
    }
    tx.commit()?;
    eprintln!("wrote {} predictions", predictions.len());
    Ok(())
}

/// Honest evaluation on a held-out 20% family split. Trains the kind-vote
/// model on the 80% train families' labels, predicts only the 20% test
/// families' labels, reports per-class accuracy. Does not write.
///
/// Same seed across all three predictor binaries (see `holdout.rs`) so the
/// numbers from each are directly comparable on identical test data.
fn run_holdout_test(
    rows: &[(Option<i64>, Option<String>, Vec<String>)],
) -> Result<()> {
    let labeled_families: Vec<i64> = rows
        .iter()
        .filter(|(_, hub, _)| hub.is_some())
        .filter_map(|(fid, _, _)| *fid)
        .collect();
    let (train_families, test_families) = holdout::split(&labeled_families);
    eprintln!(
        "holdout split: {} train families, {} test families (seed={:#x})",
        train_families.len(),
        test_families.len(),
        holdout::HOLDOUT_SEED
    );

    // Train: only labeled rows whose family is in train.
    let mut joint: HashMap<String, HashMap<String, u32>> = HashMap::new();
    let mut kind_total: HashMap<String, u32> = HashMap::new();
    let mut train_rows = 0usize;
    for (fid, hub, kinds) in rows {
        let (Some(fid), Some(hub)) = (fid, hub.as_ref()) else {
            continue;
        };
        if !train_families.contains(fid) {
            continue;
        }
        train_rows += 1;
        for k in kinds {
            *joint.entry(k.clone()).or_default().entry(hub.clone()).or_insert(0) += 1;
            *kind_total.entry(k.clone()).or_insert(0) += 1;
        }
    }
    eprintln!("trained on {} rows ({} distinct kinds)", train_rows, kind_total.len());

    // Predict: each test-family labeled row.
    let mut total = 0u32;
    let mut correct = 0u32;
    let mut no_signal = 0u32;
    let mut per_class_total: HashMap<String, u32> = HashMap::new();
    let mut per_class_correct: HashMap<String, u32> = HashMap::new();
    let mut confusion: HashMap<(String, String), u32> = HashMap::new();

    for (fid, hub, kinds) in rows {
        let (Some(fid), Some(truth)) = (fid, hub.as_ref()) else {
            continue;
        };
        if !test_families.contains(fid) {
            continue;
        }
        total += 1;
        *per_class_total.entry(truth.clone()).or_insert(0) += 1;

        let mut score: HashMap<String, f64> = HashMap::new();
        for k in kinds {
            let n = match kind_total.get(k) {
                Some(&n) if n > 0 => n as f64,
                _ => continue,
            };
            if let Some(hubs) = joint.get(k) {
                for (hub, &c) in hubs {
                    *score.entry(hub.clone()).or_insert(0.0) += c as f64 / n;
                }
            }
        }
        if score.is_empty() {
            no_signal += 1;
            *confusion
                .entry((truth.clone(), "<no-signal>".to_string()))
                .or_insert(0) += 1;
            continue;
        }
        let (best, _) = score
            .iter()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap();
        if best == truth {
            correct += 1;
            *per_class_correct.entry(truth.clone()).or_insert(0) += 1;
        } else {
            *confusion
                .entry((truth.clone(), best.clone()))
                .or_insert(0) += 1;
        }
    }

    eprintln!();
    eprintln!("=== holdout-test on kind-vote ===");
    eprintln!(
        "overall: {}/{} = {:.1}% ({} with no kind:* signal)",
        correct,
        total,
        100.0 * correct as f64 / total as f64,
        no_signal
    );
    eprintln!("per-class accuracy:");
    let mut classes: Vec<_> = per_class_total.iter().collect();
    classes.sort_by(|a, b| b.1.cmp(a.1));
    for (cls, &tot) in &classes {
        let cor = per_class_correct.get(*cls).copied().unwrap_or(0);
        eprintln!(
            "  {:<25} {:>4}/{:<4} = {:>5.1}%",
            cls,
            cor,
            tot,
            100.0 * cor as f64 / tot as f64
        );
    }
    eprintln!("top confusions (truth → predicted):");
    let mut conf_vec: Vec<_> = confusion.iter().filter(|((t, p), _)| t != p).collect();
    conf_vec.sort_by(|a, b| b.1.cmp(a.1));
    for ((truth, pred), c) in conf_vec.iter().take(10) {
        eprintln!("  {:<22} → {:<22} {}", truth, pred, c);
    }
    Ok(())
}

/// RAII semaphore for the shared `%APPDATA%/.../.session-active.lock` file.
/// Refuses to acquire if the lock is already present (surfaces contents so the
/// user can decide whether to override). Releases on Drop, regardless of how
/// the program exits.
struct SessionLock {
    path: PathBuf,
}

impl SessionLock {
    fn acquire(db_dir: &Path, operation: &str) -> Result<Self> {
        let lock_path = db_dir.join(".session-active.lock");
        if lock_path.exists() {
            let contents =
                std::fs::read_to_string(&lock_path).unwrap_or_else(|_| "(unreadable)".to_string());
            return Err(anyhow!(
                "DB write-lock already held — bailing out. existing lock:\n{contents}"
            ));
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cwd = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "(unknown)".to_string());
        let contents = format!(
            "acquired_unix: {now}\nworktree:      {cwd}\npid:           {}\noperation:     {operation}\n",
            std::process::id()
        );
        std::fs::write(&lock_path, contents)
            .with_context(|| format!("write lock {}", lock_path.display()))?;
        Ok(SessionLock { path: lock_path })
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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
            "--dry-run" => args.dry_run = true,
            "--holdout-test" => args.holdout_test = true,
            "-h" | "--help" => {
                eprintln!("usage: predict_categories [--db PATH] [--dry-run] [--holdout-test]");
                std::process::exit(0);
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
        i += 1;
    }
    Ok(args)
}

fn default_db_path() -> PathBuf {
    vam_package_browser_lib::paths::default_db_path()
}
