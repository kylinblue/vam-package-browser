//! Brute-force cosine top-N. Loads every stored vector for the chosen
//! (model, input_kind) into memory and scores against the query vector.
//! At ~3.7k families × 384/768 dims this is sub-millisecond and adds no
//! ANN-index dependencies; revisit if the library grows past ~50k.
//!
//! BGE-small-en-v1.5 and nomic-embed-text-v1.5 both emit L2-normalized
//! vectors, so cosine reduces to dot product. We still normalize the
//! query defensively in case a future model doesn't pre-normalize.

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};

use crate::embedding::{
    model::encode_batch,
    runner::InputKind,
    storage::{self, blob_to_f32s},
    ModelChoice,
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub family_id: i64,
    pub creator: String,
    pub package_name: String,
    pub purpose: Option<String>,
    pub score: f32,
}

fn l2_normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

fn rank_top_n(query: &[f32], rows: &[(i64, Vec<f32>)], top_n: usize) -> Vec<(i64, f32)> {
    let mut scored: Vec<(i64, f32)> = rows
        .iter()
        .filter(|(_, v)| v.len() == query.len())
        .map(|(id, v)| (*id, dot(query, v)))
        .collect();
    // Sort descending by score. f32 ordering needs partial_cmp.
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_n);
    scored
}

fn enrich(conn: &Connection, ranked: Vec<(i64, f32)>) -> Result<Vec<SearchHit>> {
    if ranked.is_empty() {
        return Ok(Vec::new());
    }
    let ids_csv = ranked
        .iter()
        .map(|(id, _)| id.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, creator, package_name, purpose FROM package_family
         WHERE id IN ({ids_csv})"
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let mut by_id: std::collections::HashMap<i64, (String, String, Option<String>)> =
        std::collections::HashMap::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let creator: String = row.get(1)?;
        let package_name: String = row.get(2)?;
        let purpose: Option<String> = row.get(3)?;
        by_id.insert(id, (creator, package_name, purpose));
    }
    let mut hits = Vec::with_capacity(ranked.len());
    for (id, score) in ranked {
        if let Some((creator, package_name, purpose)) = by_id.remove(&id) {
            hits.push(SearchHit {
                family_id: id,
                creator,
                package_name,
                purpose,
                score,
            });
        }
    }
    Ok(hits)
}

pub fn search_text(
    conn: &Connection,
    model: ModelChoice,
    input_kind: InputKind,
    query: &str,
    top_n: usize,
) -> Result<Vec<SearchHit>> {
    let query = query.trim();
    if query.is_empty() {
        return Err(anyhow!("empty search query"));
    }
    let mut vectors = encode_batch(model, &[query.to_string()], None)?;
    let mut qvec = vectors
        .pop()
        .ok_or_else(|| anyhow!("encoder returned no vectors for query"))?;
    l2_normalize(&mut qvec);

    let all = storage::load_all(conn, model.name(), input_kind.name())?;
    if all.is_empty() {
        return Err(anyhow!(
            "no embeddings stored for model={} input_kind={} (run --embed-all first)",
            model.name(),
            input_kind.name()
        ));
    }
    let ranked = rank_top_n(&qvec, &all, top_n);
    enrich(conn, ranked)
}

pub fn search_similar_to_family(
    conn: &Connection,
    model: ModelChoice,
    input_kind: InputKind,
    family_id: i64,
    top_n: usize,
) -> Result<Vec<SearchHit>> {
    let blob: Vec<u8> = conn
        .query_row(
            "SELECT embedding FROM family_embeddings
             WHERE family_id = ?1 AND model = ?2 AND input_kind = ?3",
            params![family_id, model.name(), input_kind.name()],
            |r| r.get(0),
        )
        .map_err(|e| {
            anyhow!(
                "no embedding for family_id={family_id} model={} input_kind={}: {e}",
                model.name(),
                input_kind.name()
            )
        })?;
    let mut qvec = blob_to_f32s(&blob)?;
    l2_normalize(&mut qvec);

    let all = storage::load_all(conn, model.name(), input_kind.name())?;
    // +1 because the family will rank #1 against itself; drop it before truncating.
    let mut ranked = rank_top_n(&qvec, &all, top_n + 1);
    ranked.retain(|(id, _)| *id != family_id);
    ranked.truncate(top_n);
    enrich(conn, ranked)
}
