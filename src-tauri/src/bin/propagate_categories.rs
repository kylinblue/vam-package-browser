//! Phase 2a: dep-graph feature-kNN for `hub_category`.
//!
//! Picks up where Phase 0.5 (`predict_categories`) leaves off. For each row
//! with `hub_category IS NULL`, compute a structural feature vector from the
//! dep graph, find the K most similar labeled rows by cosine, majority-vote
//! their `hub_category` weighted by similarity.
//!
//! Why feature-kNN, not direct neighbor voting:
//!   An earlier draft used "neighbors vote with their own category" + label
//!   propagation. CV scored 2.8%. The `--audit` mode revealed why: this graph
//!   is *anti-assortative* — a Scene's deps are Plugins/Hair/Clothing/Morph,
//!   so a vote-based propagator labels Scenes as "Plugins". The distributions
//!   themselves are highly distinctive per-category, but they're features for
//!   a classifier, not labels to copy from neighbors.
//!
//! Feature vector per node (dim = 2 × |hub_categories| + 2):
//!   - `fwd_frac[C]` = fraction of forward-edges-to-known-category going to C
//!   - `rev_frac[C]` = fraction of reverse-edges-from-known-category coming from C
//!   - `outdeg_norm`, `indeg_norm` = saturating log-degree, scaled to ~[0,1]
//!
//!   Neighbor labels: ground-truth `hub_category` (weight 1.0) where present,
//!   else `predicted_hub_category` (weight = `predicted_confidence`). Using
//!   kind-vote predictions widens neighbor coverage from 58% to ~95% — empty
//!   features were the dominant failure mode in CV-without-predictions.
//!
//! Write policy (production mode):
//!   - row has no prediction yet                  → write graph-prop result
//!   - row has kind-vote with confidence < 0.6    → overwrite with graph-prop
//!   - row has kind-vote with confidence ≥ 0.6    → keep kind-vote
//!
//! The threshold (0.6) matches the TODO's "low-confidence" boundary and the
//! morph-pack ambiguity Phase 2a is meant to break.
//!
//! `--cv` runs 5-fold cross-validation: hide a fold's labels (both from voting
//! candidates and from feature-set contributions), predict, score per class.
//! `--audit` dumps the empirical neighbor-distribution per labeled category.
//!
//! Honors the multi-session DB write-lock at
//! `%APPDATA%/com.github.kylinblue.vam-package-browser/.session-active.lock`.
//!
//! Usage: `propagate_categories [--db PATH] [--dry-run] [--cv] [--audit] [-k N]`

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use rusqlite::params;
use vam_package_browser_lib::{holdout, index};

const PREDICTED_METHOD: &str = "graph-prop";
const LOW_CONF_THRESHOLD: f64 = 0.6;
const CV_FOLDS: usize = 5;
const CV_SEED: u64 = 0xC0FFEE_C0FFEE;
const DEFAULT_K: usize = 10;

#[derive(Debug, Default)]
struct Args {
    db: Option<PathBuf>,
    dry_run: bool,
    cv: bool,
    audit: bool,
    holdout_test: bool,
    k: Option<usize>,
}

#[derive(Clone)]
struct Node {
    id: i64,
    family_id: Option<i64>,
    hub: Option<String>,
    pred: Option<String>,
    pred_conf: Option<f64>,
}

fn main() -> Result<()> {
    let args = parse_args()?;
    let k = args.k.unwrap_or(DEFAULT_K);
    let db_path = args.db.clone().unwrap_or_else(default_db_path);
    if !db_path.exists() {
        return Err(anyhow!("index db not found at {}", db_path.display()));
    }

    let db_dir = db_path
        .parent()
        .ok_or_else(|| anyhow!("db path has no parent dir"))?
        .to_path_buf();
    let _lock = SessionLock::acquire(&db_dir, "propagate_categories (Phase 2a feature-kNN)")?;

    let mut conn = index::open_and_migrate(&db_path)
        .with_context(|| format!("open index at {}", db_path.display()))?;

    let nodes = load_nodes(&conn)?;
    let (forward, reverse, edge_count) = load_edges(&conn)?;
    let by_id: HashMap<i64, &Node> = nodes.iter().map(|n| (n.id, n)).collect();

    // Discover the labeled category set + assign a stable index per category.
    let mut cats: Vec<String> = nodes
        .iter()
        .filter_map(|n| n.hub.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    cats.sort();
    let cat_idx: HashMap<String, usize> = cats
        .iter()
        .enumerate()
        .map(|(i, c)| (c.clone(), i))
        .collect();

    let labeled_count = nodes.iter().filter(|n| n.hub.is_some()).count();
    let predicted_count = nodes
        .iter()
        .filter(|n| n.hub.is_none() && n.pred.is_some())
        .count();
    let unpredicted_count = nodes
        .iter()
        .filter(|n| n.hub.is_none() && n.pred.is_none())
        .count();
    let low_conf = nodes
        .iter()
        .filter(|n| {
            n.hub.is_none() && n.pred_conf.map(|c| c < LOW_CONF_THRESHOLD).unwrap_or(false)
        })
        .count();
    eprintln!("loaded {} nodes, {} edges, {} hub categories", nodes.len(), edge_count, cats.len());
    eprintln!("  labeled (hub truth):     {}", labeled_count);
    eprintln!(
        "  kind-vote predicted:     {} ({} low-conf < {})",
        predicted_count, low_conf, LOW_CONF_THRESHOLD
    );
    eprintln!("  no prediction yet:       {}", unpredicted_count);
    eprintln!("  k (kNN neighbors):       {}", k);

    if args.audit {
        run_audit(&nodes, &forward, &reverse, &by_id);
        return Ok(());
    }

    if args.cv {
        run_cv(&nodes, &forward, &reverse, &by_id, &cat_idx, k);
        return Ok(());
    }

    if args.holdout_test {
        run_holdout_test(&nodes, &forward, &reverse, &by_id, &cat_idx, k);
        return Ok(());
    }

    // Production pass. Build features once (no hidden set) using ground-truth
    // labels only. Predict every row with hub_category IS NULL, then apply the
    // write policy.
    let training: Vec<(Vec<f64>, String)> = nodes
        .iter()
        .filter_map(|n| {
            let hub = n.hub.as_ref()?;
            let f = build_features(n.id, &forward, &reverse, &by_id, &cat_idx, None);
            if f.is_empty() {
                return None;
            }
            Some((f.flatten(), hub.clone()))
        })
        .collect();
    eprintln!(
        "training set: {} labeled rows with non-empty features",
        training.len()
    );

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
    let mut empty_features = 0usize;
    let mut kept_high_conf = 0usize;
    let mut dist: HashMap<String, u32> = HashMap::new();

    for n in &nodes {
        if n.hub.is_some() {
            continue;
        }
        let high_conf = n
            .pred_conf
            .map(|c| c >= LOW_CONF_THRESHOLD)
            .unwrap_or(false);
        if high_conf {
            kept_high_conf += 1;
            continue;
        }
        let f = build_features(n.id, &forward, &reverse, &by_id, &cat_idx, None);
        if f.is_empty() {
            empty_features += 1;
            continue;
        }
        match predict_knn(&f.flatten(), &training, k) {
            None => {
                empty_features += 1;
            }
            Some((label, conf)) => {
                let replaces = if n.pred.is_none() {
                    ReplaceKind::Fresh
                } else {
                    ReplaceKind::LowConf
                };
                *dist.entry(label.clone()).or_insert(0) += 1;
                plans.push(Plan {
                    package_id: n.id,
                    predicted: label,
                    confidence: conf,
                    replaces,
                });
            }
        }
    }

    let fresh = plans
        .iter()
        .filter(|p| matches!(p.replaces, ReplaceKind::Fresh))
        .count();
    let overwrites = plans.len() - fresh;
    eprintln!(
        "graph-prop plan: {} writes ({} fresh, {} overwrite low-conf kind-vote)",
        plans.len(),
        fresh,
        overwrites
    );
    eprintln!("  kept high-conf kind-vote:    {}", kept_high_conf);
    eprintln!("  candidates with empty features: {}", empty_features);
    eprintln!("graph-prop distribution:");
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
    eprintln!("wrote {} graph-prop predictions", plans.len());
    Ok(())
}

fn load_nodes(conn: &rusqlite::Connection) -> Result<Vec<Node>> {
    let mut stmt = conn.prepare(
        "SELECT id, family_id, hub_category, predicted_hub_category, predicted_confidence
         FROM packages",
    )?;
    let it = stmt.query_map([], |row| {
        Ok(Node {
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

fn load_edges(
    conn: &rusqlite::Connection,
) -> Result<(HashMap<i64, Vec<i64>>, HashMap<i64, Vec<i64>>, usize)> {
    let mut stmt = conn.prepare(
        "SELECT src_package_id, dst_package_id FROM package_dep_links
         WHERE dst_package_id IS NOT NULL",
    )?;
    let it = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut forward: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut reverse: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut count = 0usize;
    for row in it {
        let (s, d) = row?;
        forward.entry(s).or_default().push(d);
        reverse.entry(d).or_default().push(s);
        count += 1;
    }
    Ok((forward, reverse, count))
}

struct Features {
    fwd: Vec<f64>, // length = |categories|, normalized to sum to 1 (or all 0)
    rev: Vec<f64>,
    outdeg_norm: f64, // log(1 + count_fwd) / log(1 + DEG_CAP), clamped to [0,1]
    indeg_norm: f64,
}

/// Cap log-degree growth so a single mega-popular package doesn't dominate the
/// feature scale. 50 forward-edges is already extreme in this dataset.
const DEG_CAP: f64 = 50.0;

impl Features {
    fn is_empty(&self) -> bool {
        // Treat as empty only when there's no signal at all — neither labeled
        // neighbors nor any edges. A node with zero labeled neighbors but
        // non-zero degree still carries a (degree-only) signal worth using.
        self.fwd.iter().all(|&x| x == 0.0)
            && self.rev.iter().all(|&x| x == 0.0)
            && self.outdeg_norm == 0.0
            && self.indeg_norm == 0.0
    }
    fn flatten(&self) -> Vec<f64> {
        let mut v = Vec::with_capacity(self.fwd.len() + self.rev.len() + 2);
        v.extend_from_slice(&self.fwd);
        v.extend_from_slice(&self.rev);
        v.push(self.outdeg_norm);
        v.push(self.indeg_norm);
        v
    }
}

fn deg_norm(count: usize) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let n = count as f64;
    ((1.0 + n).ln() / (1.0 + DEG_CAP).ln()).min(1.0)
}

/// Build the structural feature vector for a node from its dep neighborhood.
/// Neighbor label = ground-truth `hub_category` (weight 1.0) where present,
/// else `predicted_hub_category` (weight = `predicted_confidence`). Widening
/// to predictions raises neighbor coverage from 58% labeled-only to ~95% and
/// drops the "empty features" failure rate substantially.
///
/// Degree features are computed against the *full* neighborhood (labeled,
/// predicted, and unknown) so even a node with zero category-known neighbors
/// still carries a degree-only signal.
///
/// `hidden` removes the listed node IDs from contributing labels — used by CV
/// to prevent the fold being scored from leaking into its own features.
/// Hidden nodes still count toward degree (the edge exists; only the label is
/// withheld), which matches what we'd see in production if those nodes were
/// genuinely unlabeled.
fn build_features(
    pid: i64,
    forward: &HashMap<i64, Vec<i64>>,
    reverse: &HashMap<i64, Vec<i64>>,
    by_id: &HashMap<i64, &Node>,
    cat_idx: &HashMap<String, usize>,
    hidden: Option<&HashSet<i64>>,
) -> Features {
    let n_cat = cat_idx.len();
    let mut fwd = vec![0.0; n_cat];
    let mut rev = vec![0.0; n_cat];
    let mut fwd_total = 0.0;
    let mut rev_total = 0.0;
    let mut fwd_count = 0usize;
    let mut rev_count = 0usize;

    let collect = |neighbors: &[i64],
                   into: &mut [f64],
                   total: &mut f64,
                   deg: &mut usize| {
        // Dedup — multiple raw_dep_keys can resolve to the same dst.
        let mut seen: HashSet<i64> = HashSet::new();
        for &nid in neighbors {
            if nid == pid {
                continue;
            }
            if !seen.insert(nid) {
                continue;
            }
            *deg += 1;
            let Some(n) = by_id.get(&nid) else { continue };
            // Hidden nodes contribute no label but still counted toward degree.
            let hide_label = hidden.map(|h| h.contains(&nid)).unwrap_or(false);
            if hide_label {
                continue;
            }
            let (label, w) = match (&n.hub, &n.pred, n.pred_conf) {
                (Some(h), _, _) => (h.as_str(), 1.0),
                (None, Some(p), Some(c)) if c > 0.0 => (p.as_str(), c),
                _ => continue,
            };
            if let Some(&idx) = cat_idx.get(label) {
                into[idx] += w;
                *total += w;
            }
        }
    };

    if let Some(deps) = forward.get(&pid) {
        collect(deps, &mut fwd, &mut fwd_total, &mut fwd_count);
    }
    if let Some(rdeps) = reverse.get(&pid) {
        collect(rdeps, &mut rev, &mut rev_total, &mut rev_count);
    }
    if fwd_total > 0.0 {
        for x in &mut fwd {
            *x /= fwd_total;
        }
    }
    if rev_total > 0.0 {
        for x in &mut rev {
            *x /= rev_total;
        }
    }
    Features {
        fwd,
        rev,
        outdeg_norm: deg_norm(fwd_count),
        indeg_norm: deg_norm(rev_count),
    }
}

fn cosine(a: &[f64], b: &[f64]) -> f64 {
    debug_assert_eq!(a.len(), b.len());
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Cosine-kNN. Returns `(winning_class, confidence)` where confidence is the
/// share of summed top-K similarity captured by the winning class. None when
/// no training row has any overlap with the query.
fn predict_knn(
    query: &[f64],
    training: &[(Vec<f64>, String)],
    k: usize,
) -> Option<(String, f64)> {
    if query.iter().all(|&x| x == 0.0) {
        return None;
    }
    let mut sims: Vec<(f64, &str)> = training
        .iter()
        .map(|(v, h)| (cosine(query, v), h.as_str()))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    if sims.is_empty() {
        return None;
    }
    sims.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
    let top = if sims.len() > k { &sims[..k] } else { &sims[..] };
    let mut score: HashMap<String, f64> = HashMap::new();
    let mut total = 0.0;
    for (sim, label) in top {
        *score.entry(label.to_string()).or_insert(0.0) += sim;
        total += sim;
    }
    let (best, &best_score) = score
        .iter()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())?;
    Some((best.clone(), best_score / total))
}

fn run_cv(
    nodes: &[Node],
    forward: &HashMap<i64, Vec<i64>>,
    reverse: &HashMap<i64, Vec<i64>>,
    by_id: &HashMap<i64, &Node>,
    cat_idx: &HashMap<String, usize>,
    k: usize,
) {
    let mut labeled: Vec<&Node> = nodes.iter().filter(|n| n.hub.is_some()).collect();
    // Deterministic shuffle (splitmix64-ish).
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

    let n = labeled.len();
    let fold_size = (n + CV_FOLDS - 1) / CV_FOLDS;

    let mut per_class_total: HashMap<String, u32> = HashMap::new();
    let mut per_class_correct: HashMap<String, u32> = HashMap::new();
    let mut total = 0u32;
    let mut correct = 0u32;
    let mut no_features = 0u32;
    let mut confusion: HashMap<(String, String), u32> = HashMap::new();

    for fold in 0..CV_FOLDS {
        let lo = fold * fold_size;
        let hi = ((fold + 1) * fold_size).min(n);
        if lo >= hi {
            break;
        }
        let hidden: HashSet<i64> = labeled[lo..hi].iter().map(|n| n.id).collect();

        // Rebuild training set per fold using only visible labeled rows. Their
        // features also hide the fold to avoid leakage (a non-fold labeled
        // node that has a fold node as neighbor shouldn't see that label).
        let training: Vec<(Vec<f64>, String)> = nodes
            .iter()
            .filter(|n| n.hub.is_some() && !hidden.contains(&n.id))
            .filter_map(|n| {
                let f = build_features(n.id, forward, reverse, by_id, cat_idx, Some(&hidden));
                if f.is_empty() {
                    return None;
                }
                Some((f.flatten(), n.hub.clone().unwrap()))
            })
            .collect();

        for node in &labeled[lo..hi] {
            let truth = node.hub.as_ref().unwrap();
            *per_class_total.entry(truth.clone()).or_insert(0) += 1;
            total += 1;
            let f = build_features(node.id, forward, reverse, by_id, cat_idx, Some(&hidden));
            if f.is_empty() {
                no_features += 1;
                *confusion
                    .entry((truth.clone(), "<no-features>".to_string()))
                    .or_insert(0) += 1;
                continue;
            }
            match predict_knn(&f.flatten(), &training, k) {
                None => {
                    no_features += 1;
                    *confusion
                        .entry((truth.clone(), "<no-features>".to_string()))
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
    }

    eprintln!();
    eprintln!("=== {}-fold CV on graph-prop feature-kNN (k={}) ===", CV_FOLDS, k);
    eprintln!(
        "overall: {}/{} = {:.1}% ({} with empty features)",
        correct,
        total,
        100.0 * correct as f64 / total as f64,
        no_features
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
/// with the other predictors). Trains using ONLY train-family ground-truth
/// labels — no predictions from any other method leak in (matches what we'd
/// have if graph-prop were the only predictor). Predicts only test-family
/// rows, reports per-class accuracy.
fn run_holdout_test(
    nodes: &[Node],
    forward: &HashMap<i64, Vec<i64>>,
    reverse: &HashMap<i64, Vec<i64>>,
    _by_id: &HashMap<i64, &Node>,
    cat_idx: &HashMap<String, usize>,
    k: usize,
) {
    let labeled_families: Vec<i64> = nodes
        .iter()
        .filter(|n| n.hub.is_some())
        .filter_map(|n| n.family_id)
        .collect();
    let (train_families, test_families) = holdout::split(&labeled_families);
    eprintln!(
        "holdout split: {} train families, {} test families (seed={:#x})",
        train_families.len(),
        test_families.len(),
        holdout::HOLDOUT_SEED
    );

    // Build a node map that strips labels from test families AND from all
    // predictions (test-time data must not leak through pred_*, which was
    // computed using all labeled data including the held-out fold).
    // We do this by passing a `hidden` set to build_features that includes
    // test families' nodes, and by zeroing out predictions for everyone.
    let test_ids: std::collections::HashSet<i64> = nodes
        .iter()
        .filter(|n| n.family_id.map(|f| test_families.contains(&f)).unwrap_or(false))
        .map(|n| n.id)
        .collect();

    // Rebuild a node map where pred_* is wiped — features will use ground
    // truth only, ensuring no cross-method leak from kind-vote/embed-knn DB
    // state that was trained on the full labeled set.
    let clean_nodes: Vec<Node> = nodes
        .iter()
        .map(|n| Node {
            id: n.id,
            family_id: n.family_id,
            hub: n.hub.clone(),
            pred: None,
            pred_conf: None,
        })
        .collect();
    let clean_by_id: HashMap<i64, &Node> = clean_nodes.iter().map(|n| (n.id, n)).collect();

    // Training set: features of train-family labeled rows (test families
    // hidden so their label can't propagate even through a training row's
    // own features).
    let training: Vec<(Vec<f64>, String)> = clean_nodes
        .iter()
        .filter(|n| n.family_id.map(|f| train_families.contains(&f)).unwrap_or(false))
        .filter_map(|n| {
            let hub = n.hub.as_ref()?;
            let f = build_features(n.id, forward, reverse, &clean_by_id, cat_idx, Some(&test_ids));
            if f.is_empty() {
                return None;
            }
            Some((f.flatten(), hub.clone()))
        })
        .collect();
    eprintln!("training set: {} train-family rows with non-empty features", training.len());

    let mut total = 0u32;
    let mut correct = 0u32;
    let mut no_features = 0u32;
    let mut per_class_total: HashMap<String, u32> = HashMap::new();
    let mut per_class_correct: HashMap<String, u32> = HashMap::new();
    let mut confusion: HashMap<(String, String), u32> = HashMap::new();

    for n in &clean_nodes {
        let Some(truth) = &n.hub else { continue };
        let Some(fid) = n.family_id else { continue };
        if !test_families.contains(&fid) {
            continue;
        }
        total += 1;
        *per_class_total.entry(truth.clone()).or_insert(0) += 1;
        let f = build_features(n.id, forward, reverse, &clean_by_id, cat_idx, Some(&test_ids));
        if f.is_empty() {
            no_features += 1;
            *confusion
                .entry((truth.clone(), "<no-features>".to_string()))
                .or_insert(0) += 1;
            continue;
        }
        match predict_knn(&f.flatten(), &training, k) {
            None => {
                no_features += 1;
                *confusion
                    .entry((truth.clone(), "<no-features>".to_string()))
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
    eprintln!("=== holdout-test on graph-prop feature-kNN (k={}, no cross-method inputs) ===", k);
    eprintln!(
        "overall: {}/{} = {:.1}% ({} with empty features)",
        correct,
        total,
        100.0 * correct as f64 / total as f64,
        no_features
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

/// Empirical neighbor-label distribution per labeled hub_category. Tests
/// whether the dep graph is assortative (diagonal-heavy) or complementary
/// (off-diagonal-heavy). Splits forward (deps) from reverse (consumers).
fn run_audit(
    nodes: &[Node],
    forward: &HashMap<i64, Vec<i64>>,
    reverse: &HashMap<i64, Vec<i64>>,
    by_id: &HashMap<i64, &Node>,
) {
    let neighbor_truth = |nid: i64| -> Option<&str> {
        by_id.get(&nid).and_then(|n| n.hub.as_deref())
    };

    let mut fwd: HashMap<String, HashMap<String, u32>> = HashMap::new();
    let mut rev: HashMap<String, HashMap<String, u32>> = HashMap::new();
    let mut totals: HashMap<String, u32> = HashMap::new();
    let mut fwd_edges: HashMap<String, u32> = HashMap::new();
    let mut rev_edges: HashMap<String, u32> = HashMap::new();

    for n in nodes {
        let Some(hub) = &n.hub else { continue };
        *totals.entry(hub.clone()).or_insert(0) += 1;
        if let Some(deps) = forward.get(&n.id) {
            for &d in deps {
                if let Some(nhub) = neighbor_truth(d) {
                    *fwd.entry(hub.clone()).or_default().entry(nhub.to_string()).or_insert(0) += 1;
                    *fwd_edges.entry(hub.clone()).or_insert(0) += 1;
                }
            }
        }
        if let Some(rdeps) = reverse.get(&n.id) {
            for &s in rdeps {
                if let Some(nhub) = neighbor_truth(s) {
                    *rev.entry(hub.clone()).or_default().entry(nhub.to_string()).or_insert(0) += 1;
                    *rev_edges.entry(hub.clone()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut classes: Vec<_> = totals.iter().collect();
    classes.sort_by(|a, b| b.1.cmp(a.1));

    eprintln!();
    eprintln!("=== forward audit: for each labeled C, distribution of dep-categories ===");
    eprintln!("(labeled-neighbor edges only; predicted labels excluded)");
    for (cls, &tot) in &classes {
        let edges = fwd_edges.get(*cls).copied().unwrap_or(0);
        let same = fwd
            .get(*cls)
            .and_then(|m| m.get(*cls))
            .copied()
            .unwrap_or(0);
        eprintln!(
            "{:<22} (n={}, fwd-edges-to-labeled={}, P(same)={:.2})",
            cls,
            tot,
            edges,
            if edges == 0 { 0.0 } else { same as f64 / edges as f64 }
        );
        if edges == 0 {
            continue;
        }
        let mut row: Vec<_> = fwd.get(*cls).unwrap().iter().collect();
        row.sort_by(|a, b| b.1.cmp(a.1));
        for (nc, &c) in row.iter().take(5) {
            eprintln!("    → {:<20} {:>5} ({:.1}%)", nc, c, 100.0 * c as f64 / edges as f64);
        }
    }
    eprintln!();
    eprintln!("=== reverse audit: for each labeled C, distribution of consumer-categories ===");
    for (cls, &tot) in &classes {
        let edges = rev_edges.get(*cls).copied().unwrap_or(0);
        let same = rev
            .get(*cls)
            .and_then(|m| m.get(*cls))
            .copied()
            .unwrap_or(0);
        eprintln!(
            "{:<22} (n={}, rev-edges-from-labeled={}, P(same)={:.2})",
            cls,
            tot,
            edges,
            if edges == 0 { 0.0 } else { same as f64 / edges as f64 }
        );
        if edges == 0 {
            continue;
        }
        let mut row: Vec<_> = rev.get(*cls).unwrap().iter().collect();
        row.sort_by(|a, b| b.1.cmp(a.1));
        for (nc, &c) in row.iter().take(5) {
            eprintln!("    ← {:<20} {:>5} ({:.1}%)", nc, c, 100.0 * c as f64 / edges as f64);
        }
    }
}

/// RAII semaphore for the shared `%APPDATA%/.../.session-active.lock` file.
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
            "--audit" => args.audit = true,
            "--holdout-test" => args.holdout_test = true,
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
                    "usage: propagate_categories [--db PATH] [--dry-run] [--cv]\n\
                     \t[--audit] [--holdout-test] [-k N]"
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
    vam_package_browser_lib::paths::default_db_path()
}
