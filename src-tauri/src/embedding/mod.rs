//! Local embedding pipeline. Encodes each `package_family` row's
//! purpose text (and optionally its tag list) into a vector embedding,
//! stored in the v13 `family_embeddings` table. Powers "find similar to
//! this package" and natural-language search via brute-force cosine.
//!
//! Companion to the `tagging` module — tagging gives discrete filters,
//! embeddings give fuzzy semantic match.

pub mod model;
pub mod runner;
pub mod search;
pub mod storage;

pub use model::ModelChoice;
pub use runner::InputKind;
