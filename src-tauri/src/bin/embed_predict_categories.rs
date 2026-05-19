//! Phase 2b: text-embedding kNN for `hub_category`.
//!
//! Orthogonal signal to Phase 0.5 (kind:* tag vote) and Phase 2a (dep-graph
//! feature kNN). Uses the existing `family_embeddings` table — BGE or Nomic
//! over `purpose` or `purpose-with-tags`. For each unlabeled package, look up
//! its family's embedding, find the K nearest labeled families by cosine,
//! majority-vote weighted by similarity.
//!
//! Granularity: embeddings are per-family but writes are per-package. We
//! predict at family granularity (same family → same embedding → same
//! prediction, and almost always the same hub_category in the labeled set)
//! and apply the per-package write policy.
//!
//! CV is fold-by-family — same-family packages share an embedding, so
//! fold-by-package would leak (cosine = 1.0 against own family at training).
//!
//! Write policy mirrors Phase 2a:
//!   - row has no prediction yet              → write embed-knn
//!   - row has any prediction with conf < 0.6 → overwrite with embed-knn
//!   - row has any prediction with conf ≥ 0.6 → keep
//!
//! Honors the multi-session DB write-lock at
//! `%APPDATA%/com.github.kylinblue.vam-package-browser/.session-active.lock`.
//!
//! Usage:
//!   embed_predict_categories [--db PATH] [--dry-run] [--cv]
//!                            [--model bge|nomic] [--input purpose|tags]
//!                            [-k N]

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::params;
use vam_package_browser_lib::embedding::{storage, InputKind, ModelChoice};
use vam_package_browser_lib::{holdout, index};

const PREDICTED_METHOD: &str = "embed-knn";
const LOW_CONF_THRESHOLD: f64 = 0.6;
const CV_FOLDS: usize = 5;
const CV_SEED: u64 = 0xC0FFEE_C0FFEE;
const DEFAULT_K: usize = 10;

#[derive(Debug, Default)]
struct Args {
    db: Option<PathBuf>,
    dry_run: bool,
    cv: bool,
    holdout_test: bool,
    model: Option<ModelChoice>,
    input: Option<InputKind>,
    k: Option<usize>,
}

#[derive(Clone)]
struct Pkg {
    id: i64,
    family_id: Option<i64>,
    hub: Option<String>,
    pred: Option<String>,
    pred_conf: Option<f64>,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let k = args.k.unwrap_or(DEFAULT_K);
    let model = args.model.unwrap_or(ModelChoice::BgeSmallEnV15);
    let input = args.input.unwrap_or(InputKind::PurposeWithTags);
    let db_path = args.db.clone().unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!("index db not found at {}", db_path.display()));
    }

    let db_dir = db_path
        .parent()
        .ok_or_else(|| anyhow!("db path has no parent dir"))?
        .to_path_buf();
    let _lock = SessionLock::acquire(
        &db_dir,
        &format!(
            "embed_predict_categories (Phase 2b, {} / {})",
            model.name(),
            input.name()
        ),
    )?;

    let mut conn = index::open_and_migrate(&db_path)
        .with_context(|| format!("open index at {}", db_path.display()))?;

    let pkgs = load_packages(&conn)?;
    let embeddings = storage::load_all(&conn, model.name(), input.name())
        .with_context(|| format!("load embeddings {} / {}", model.name(), input.name()))?;
    if embeddings.is_empty() {
        return Err(anyhow!(
            "no embeddings stored for {} / {} — run `embed_library --embed-all` first",
            model.name(),
            input.name()
        ));
    }
    let by_family_embed: HashMap<i64, Vec<f32>> = embeddings.into_iter().collect();

    // Aggregate packages to families: pick the first non-null hub_category as
    // the family's label. (In practice all packages of a family share the same
    // hub_category — they're versions of the same Author.Package — but if any
    // are missing a match we still extract a label from the labeled siblings.)
    let mut family_hub: HashMap<i64, String> = HashMap::new();
    let mut family_pkgs: HashMap<i64, Vec<Pkg>> = HashMap::new();
    let mut no_family = 0usize;
    for p in &pkgs {
        let Some(fid) = p.family_id else {
            no_family += 1;
            continue;
        };
        family_pkgs.entry(fid).or_default().push(p.clone());
        if let Some(h) = &p.hub {
            family_hub.entry(fid).or_insert_with(|| h.clone());
        }
    }

    let total_families = family_pkgs.len();
    let labeled_families = family_hub.len();
    let unlabeled_families = total_families - labeled_families;
    let families_with_embed = family_pkgs
        .keys()
        .filter(|fid| by_family_embed.contains_key(fid))
        .count();
    eprintln!("loaded {} packages, {} families ({} with embeddings)",
        pkgs.len(), total_families, families_with_embed);
    eprintln!("  labeled families:        {}", labeled_families);
    eprintln!("  unlabeled families:      {}", unlabeled_families);
    eprintln!("  packages w/o family:     {}", no_family);
    eprintln!("  model / input / k:       {} / {} / {}", model.name(), input.name(), k);

    if args.cv {
        run_cv(&family_hub, &by_family_embed, k);
        return Ok(());
    }

    if args.holdout_test {
        run_holdout_test(&family_hub, &by_family_embed, k);
        return Ok(());
    }

    // Build training set: labeled families with embeddings.
    let training: Vec<(Vec<f32>, i64, String)> = family_hub
        .iter()
        .filter_map(|(fid, hub)| {
            by_family_embed
                .get(fid)
                .map(|v| (v.clone(), *fid, hub.clone()))
        })
        .collect();
    eprintln!("training set: {} labeled families with embeddings", training.len());

    // Predict for each unlabeled family that has an embedding.
    let mut family_predictions: HashMap<i64, (String, f64)> = HashMap::new();
    let mut no_embed = 0usize;
    for fid in family_pkgs.keys() {
        if family_hub.contains_key(fid) {
            continue;
        }
        let Some(query) = by_family_embed.get(fid) else {
            no_embed += 1;
            continue;
        };
        // Exclude self (always — own family would be similarity ~1.0).
        if let Some((label, conf)) = predict_knn(query, &training, Some(*fid), k) {
            family_predictions.insert(*fid, (label, conf));
        }
    }

    // Build per-package write plan, applying the threshold policy.
    #[derive(Copy, Clone)]
    enum ReplaceKind {
        Fresh,
        LowConf,
    }
    struct Plan {
        package_id: i64,
        predicted: String,
        confidence: f64,
        replaces: ReplaceKind,
    }
    let mut plans: Vec<Plan> = Vec::new();
    let mut kept_high_conf = 0usize;
    let mut family_unpredicted = 0usize;
    let mut dist: HashMap<String, u32> = HashMap::new();

    for (fid, pkgs_in_family) in &family_pkgs {
        if family_hub.contains_key(fid) {
            continue;
        }
        let Some((label, conf)) = family_predictions.get(fid) else {
            family_unpredicted += pkgs_in_family
                .iter()
                .filter(|p| p.hub.is_none())
                .count();
            continue;
        };
        for p in pkgs_in_family {
            if p.hub.is_some() {
                continue;
            }
            let high_conf = p
                .pred_conf
                .map(|c| c >= LOW_CONF_THRESHOLD)
                .unwrap_or(false);
            if high_conf {
                kept_high_conf += 1;
                continue;
            }
            let replaces = if p.pred.is_none() {
                ReplaceKind::Fresh
            } else {
                ReplaceKind::LowConf
            };
            *dist.entry(label.clone()).or_insert(0) += 1;
            plans.push(Plan {
                package_id: p.id,
                predicted: label.clone(),
                confidence: *conf,
                replaces,
            });
        }
    }

    let fresh = plans
        .iter()
        .filter(|p| matches!(p.replaces, ReplaceKind::Fresh))
        .count();
    let overwrites = plans.len() - fresh;
    eprintln!(
        "embed-knn plan: {} writes ({} fresh, {} overwrite existing predictions)",
        plans.len(),
        fresh,
        overwrites
    );
    eprintln!("  kept high-conf prior:        {}", kept_high_conf);
    eprintln!("  packages w/ no family embed: {}", family_unpredicted);
    eprintln!("  packages w/ no embed model:  {}", no_embed);
    eprintln!("embed-knn distribution:");
    let mut dist_vec: Vec<_> = dist.iter().collect();
    dist_vec.sort_by(|a, b| b.1.cmp(a.1));
    for (h, c) in &dist_vec {
        eprintln!("  {:<25} {}", h, c);
    }

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
        for p in &plans {
            upd.execute(params![
                p.predicted,
                PREDICTED_METHOD,
                p.confidence,
                p.package_id,
            ])?;
        }
    }
    tx.commit()?;
    eprintln!("wrote {} embed-knn predictions", plans.len());
    Ok(())
}

fn load_packages(conn: &rusqlite::Connection) -> Result<Vec<Pkg>> {
    let mut stmt = conn.prepare(
        "SELECT id, family_id, hub_category, predicted_hub_category, predicted_confidence
         FROM packages",
    )?;
    let it = stmt.query_map([], |row| {
        Ok(Pkg {
            id: row.get(0)?,
            family_id: row.get(1)?,
            hub: row.get(2)?,
            pred: row.get(3)?,
            pred_conf: row.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for n in it {
        out.push(n?);
    }
    Ok(out)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// kNN over the training set (labeled families + their embeddings).
/// `exclude_family` removes a single family — used in production to skip the
/// query's own family (cosine = 1.0). `hidden` does the same for a CV fold.
fn predict_knn(
    query: &[f32],
    training: &[(Vec<f32>, i64, String)],
    exclude_family: Option<i64>,
    k: usize,
) -> Option<(String, f64)> {
    let hidden: HashSet<i64> = exclude_family.into_iter().collect();
    predict_knn_hidden(query, training, &hidden, k)
}

fn predict_knn_hidden(
    query: &[f32],
    training: &[(Vec<f32>, i64, String)],
    hidden: &HashSet<i64>,
    k: usize,
) -> Option<(String, f64)> {
    let mut sims: Vec<(f32, &str)> = training
        .iter()
        .filter(|(v, fid, _)| !hidden.contains(fid) && v.len() == query.len())
        .map(|(v, _, h)| (dot(query, v), h.as_str()))
        .collect();
    if sims.is_empty() {
        return None;
    }
    // Discard any negative similarities — for L2-normalized vectors these are
    // strongly dissimilar pairs and shouldn't vote.
    sims.retain(|(s, _)| *s > 0.0);
    if sims.is_empty() {
        return None;
    }
    sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let top = if sims.len() > k { &sims[..k] } else { &sims[..] };
    let mut score: HashMap<String, f64> = HashMap::new();
    let mut total = 0.0f64;
    for (sim, label) in top {
        *score.entry(label.to_string()).or_insert(0.0) += *sim as f64;
        total += *sim as f64;
    }
    let (best, &best_score) = score
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())?;
    Some((best.clone(), best_score / total))
}

fn run_cv(
    family_hub: &HashMap<i64, String>,
    by_family_embed: &HashMap<i64, Vec<f32>>,
    k: usize,
) {
    // CV is on the labeled families only — we measure how often kNN recovers
    // a family's hub_category when it's hidden along with same-family siblings.
    // (Same-family siblings don't matter here because we're family-level CV,
    // but per-family folds are also the cleanest setup for fusion.)
    let mut labeled: Vec<(i64, &str)> = family_hub
        .iter()
        .filter(|(fid, _)| by_family_embed.contains_key(fid))
        .map(|(fid, h)| (*fid, h.as_str()))
        .collect();
    let n_skipped = family_hub.len() - labeled.len();

    // Deterministic shuffle.
    let mut state = CV_SEED;
    let mut next = || {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    };
    for i in (1..labeled.len()).rev() {
        let j = (next() as usize) % (i + 1);
        labeled.swap(i, j);
    }

    let training_full: Vec<(Vec<f32>, i64, String)> = labeled
        .iter()
        .map(|(fid, h)| (by_family_embed[fid].clone(), *fid, h.to_string()))
        .collect();

    let n = labeled.len();
    let fold_size = (n + CV_FOLDS - 1) / CV_FOLDS;
    let mut per_class_total: HashMap<String, u32> = HashMap::new();
    let mut per_class_correct: HashMap<String, u32> = HashMap::new();
    let mut total = 0u32;
    let mut correct = 0u32;
    let mut no_pred = 0u32;
    let mut confusion: HashMap<(String, String), u32> = HashMap::new();

    for fold in 0..CV_FOLDS {
        let lo = fold * fold_size;
        let hi = ((fold + 1) * fold_size).min(n);
        if lo >= hi {
            break;
        }
        let hidden: HashSet<i64> = labeled[lo..hi].iter().map(|(fid, _)| *fid).collect();
        for &(fid, truth) in &labeled[lo..hi] {
            *per_class_total.entry(truth.to_string()).or_insert(0) += 1;
            total += 1;
            let Some(query) = by_family_embed.get(&fid) else {
                no_pred += 1;
                continue;
            };
            match predict_knn_hidden(query, &training_full, &hidden, k) {
                None => {
                    no_pred += 1;
                    *confusion
                        .entry((truth.to_string(), "<no-prediction>".to_string()))
                        .or_insert(0) += 1;
                }
                Some((pred, _conf)) => {
                    if pred == truth {
                        correct += 1;
                        *per_class_correct.entry(truth.to_string()).or_insert(0) += 1;
                    } else {
                        *confusion
                            .entry((truth.to_string(), pred))
                            .or_insert(0) += 1;
                    }
                }
            }
        }
    }

    eprintln!();
    eprintln!("=== {}-fold CV on embed-knn (k={}) ===", CV_FOLDS, k);
    if n_skipped > 0 {
        eprintln!("(skipped {} labeled families that had no embedding)", n_skipped);
    }
    eprintln!(
        "overall: {}/{} = {:.1}% ({} with no prediction)",
        correct,
        total,
        100.0 * correct as f64 / total as f64,
        no_pred
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
}

/// Honest holdout-test mode: deterministic 80/20 family split (shared seed
/// with the other predictors). Trains kNN against ONLY train-family
/// embeddings + labels, predicts only test-family rows, reports per-class
/// accuracy. Embeddings encode text not labels, so there's no extra leak
/// surface beyond the train/test family separation itself.
fn run_holdout_test(
    family_hub: &HashMap<i64, String>,
    by_family_embed: &HashMap<i64, Vec<f32>>,
    k: usize,
) {
    let labeled_families: Vec<i64> = family_hub
        .iter()
        .filter(|(fid, _)| by_family_embed.contains_key(fid))
        .map(|(fid, _)| *fid)
        .collect();
    let (train_families, test_families) = holdout::split(&labeled_families);
    eprintln!(
        "holdout split: {} train families, {} test families (seed={:#x})",
        train_families.len(),
        test_families.len(),
        holdout::HOLDOUT_SEED
    );

    let training: Vec<(Vec<f32>, i64, String)> = family_hub
        .iter()
        .filter(|(fid, _)| train_families.contains(fid))
        .filter_map(|(fid, hub)| {
            by_family_embed
                .get(fid)
                .map(|v| (v.clone(), *fid, hub.clone()))
        })
        .collect();
    eprintln!("training set: {} train families with embeddings", training.len());

    let mut total = 0u32;
    let mut correct = 0u32;
    let mut no_pred = 0u32;
    let mut per_class_total: HashMap<String, u32> = HashMap::new();
    let mut per_class_correct: HashMap<String, u32> = HashMap::new();
    let mut confusion: HashMap<(String, String), u32> = HashMap::new();

    for (&fid, truth) in family_hub.iter() {
        if !test_families.contains(&fid) {
            continue;
        }
        let Some(query) = by_family_embed.get(&fid) else {
            no_pred += 1;
            continue;
        };
        total += 1;
        *per_class_total.entry(truth.clone()).or_insert(0) += 1;
        let hidden: HashSet<i64> = std::iter::once(fid).collect();
        match predict_knn_hidden(query, &training, &hidden, k) {
            None => {
                no_pred += 1;
                *confusion
                    .entry((truth.clone(), "<no-prediction>".to_string()))
                    .or_insert(0) += 1;
            }
            Some((pred, _conf)) => {
                if pred == *truth {
                    correct += 1;
                    *per_class_correct.entry(truth.clone()).or_insert(0) += 1;
                } else {
                    *confusion.entry((truth.clone(), pred)).or_insert(0) += 1;
                }
            }
        }
    }

    eprintln!();
    eprintln!("=== holdout-test on embed-knn (k={}) ===", k);
    eprintln!(
        "overall: {}/{} = {:.1}% ({} with no prediction)",
        correct,
        total,
        100.0 * correct as f64 / total as f64,
        no_pred
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
}

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
            "--cv" => args.cv = true,
            "--holdout-test" => args.holdout_test = true,
            "--model" => {
                i += 1;
                args.model = Some(ModelChoice::parse(
                    raw.get(i).ok_or_else(|| anyhow!("--model needs a value"))?,
                )?);
            }
            "--input" => {
                i += 1;
                args.input = Some(InputKind::parse(
                    raw.get(i).ok_or_else(|| anyhow!("--input needs a value"))?,
                )?);
            }
            "-k" => {
                i += 1;
                args.k = Some(
                    raw.get(i)
                        .ok_or_else(|| anyhow!("-k needs a value"))?
                        .parse()
                        .context("-k value must be a positive integer")?,
                );
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: embed_predict_categories [--db PATH] [--dry-run] [--cv]\n\
                     \t[--holdout-test] [--model bge|nomic] [--input purpose|tags] [-k N]"
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
