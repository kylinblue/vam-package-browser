//! Deterministic 80/20 family-level holdout split for honest classifier eval.
//!
//! Same seed across all three predictor binaries (`predict_categories`,
//! `propagate_categories`, `embed_predict_categories`) so they evaluate on the
//! identical test families and accuracy numbers are directly comparable.
//!
//! Split is by **family**, not by package, because:
//!   - same-family packages share kind:* tags (kind-vote: P(hub|kind) trained
//!     on one sibling leaks the label to another)
//!   - same-family packages share family_embeddings (embed-knn: cosine ~1.0
//!     against own-family training row trivializes the prediction)
//!   - a Family is the actual Author.Package unit the hub matches against
//!
//! Usage in a predictor:
//!   let (train_families, test_families) = holdout::split(&labeled_family_ids);
//!   // train using only rows whose family_id ∈ train_families
//!   // evaluate against rows whose family_id ∈ test_families
//!
//! See TODO-classifier-residual.md for the motivation (CV-tuning leak →
//! one-shot honest evaluation).

use std::collections::HashSet;

/// Seed for the deterministic shuffle. Distinct from CV_SEED so the split is
/// independent of any per-binary CV. Changing this value invalidates all
/// previously-reported holdout numbers — pin it.
pub const HOLDOUT_SEED: u64 = 0xDEADBEEF_CAFEBABE;

/// Fraction of labeled families reserved for the held-out test set. 20% is
/// standard; large enough to give per-class accuracy useful statistical power
/// on the dominant classes, small enough to leave the training set intact.
pub const HOLDOUT_FRACTION: f64 = 0.20;

/// Deterministic 80/20 split of family IDs. The input order does not affect
/// the output (we sort first), so different binaries pulling family IDs in
/// different orders still produce the same split.
pub fn split(family_ids: &[i64]) -> (HashSet<i64>, HashSet<i64>) {
    let mut ids: Vec<i64> = family_ids.to_vec();
    ids.sort_unstable();
    ids.dedup();

    // splitmix64-ish Fisher-Yates shuffle.
    let mut state = HOLDOUT_SEED;
    let mut next = || {
        state = state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    };
    for i in (1..ids.len()).rev() {
        let j = (next() as usize) % (i + 1);
        ids.swap(i, j);
    }

    let test_count = ((ids.len() as f64) * HOLDOUT_FRACTION).round() as usize;
    let train_count = ids.len() - test_count;
    let train: HashSet<i64> = ids[..train_count].iter().copied().collect();
    let test: HashSet<i64> = ids[train_count..].iter().copied().collect();
    (train, test)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_is_deterministic() {
        let ids: Vec<i64> = (1..=1000).collect();
        let (a_train, a_test) = split(&ids);
        let (b_train, b_test) = split(&ids);
        assert_eq!(a_train, b_train);
        assert_eq!(a_test, b_test);
    }

    #[test]
    fn split_is_order_independent() {
        let asc: Vec<i64> = (1..=500).collect();
        let mut desc: Vec<i64> = asc.clone();
        desc.reverse();
        let (a_train, a_test) = split(&asc);
        let (b_train, b_test) = split(&desc);
        assert_eq!(a_train, b_train);
        assert_eq!(a_test, b_test);
    }

    #[test]
    fn split_fractions_are_correct() {
        let ids: Vec<i64> = (1..=1000).collect();
        let (train, test) = split(&ids);
        assert_eq!(train.len() + test.len(), 1000);
        assert_eq!(test.len(), 200);
        // Train and test must be disjoint.
        assert!(train.is_disjoint(&test));
    }

    #[test]
    fn split_handles_small_sets() {
        let ids = vec![1, 2, 3, 4, 5];
        let (train, test) = split(&ids);
        assert_eq!(train.len() + test.len(), 5);
        // 5 * 0.20 = 1 test row
        assert_eq!(test.len(), 1);
    }
}
