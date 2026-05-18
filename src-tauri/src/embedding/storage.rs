//! BLOB <-> Vec<f32> serde and DB I/O for the v13 `family_embeddings`
//! table. Vectors are persisted as little-endian f32 bytes — portable
//! across hosts and trivially mmappable, at the cost of needing one
//! pass to copy in/out (vs zero-copy raw transmute, which would only
//! work on LE hosts with the right alignment).

use anyhow::{anyhow, Result};
use rusqlite::{params, Connection};

pub fn f32s_to_blob(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for &x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

pub fn blob_to_f32s(b: &[u8]) -> Result<Vec<f32>> {
    if !b.len().is_multiple_of(4) {
        return Err(anyhow!(
            "embedding BLOB length {} is not a multiple of 4",
            b.len()
        ));
    }
    let mut out = Vec::with_capacity(b.len() / 4);
    for chunk in b.chunks_exact(4) {
        out.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    Ok(out)
}

pub fn upsert(
    conn: &Connection,
    family_id: i64,
    model: &str,
    input_kind: &str,
    embedding: &[f32],
    embedded_at: i64,
) -> Result<()> {
    let blob = f32s_to_blob(embedding);
    conn.execute(
        "INSERT INTO family_embeddings(family_id, model, input_kind, embedding, dim, embedded_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(family_id, model, input_kind) DO UPDATE SET
             embedding   = excluded.embedding,
             dim         = excluded.dim,
             embedded_at = excluded.embedded_at",
        params![family_id, model, input_kind, blob, embedding.len() as i64, embedded_at],
    )?;
    Ok(())
}

pub fn load_one(
    conn: &Connection,
    family_id: i64,
    model: &str,
    input_kind: &str,
) -> Result<Option<Vec<f32>>> {
    let mut stmt = conn.prepare_cached(
        "SELECT embedding FROM family_embeddings
         WHERE family_id = ?1 AND model = ?2 AND input_kind = ?3",
    )?;
    let mut rows = stmt.query(params![family_id, model, input_kind])?;
    if let Some(row) = rows.next()? {
        let blob: Vec<u8> = row.get(0)?;
        Ok(Some(blob_to_f32s(&blob)?))
    } else {
        Ok(None)
    }
}

/// Load every (family_id, embedding) row for a given variant. Used by
/// brute-force cosine search — at 3.7k families this is ~5 MB of f32
/// data, fits comfortably in memory for the duration of a query.
pub fn load_all(
    conn: &Connection,
    model: &str,
    input_kind: &str,
) -> Result<Vec<(i64, Vec<f32>)>> {
    let mut stmt = conn.prepare(
        "SELECT family_id, embedding FROM family_embeddings
         WHERE model = ?1 AND input_kind = ?2",
    )?;
    let mut rows = stmt.query(params![model, input_kind])?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let blob: Vec<u8> = row.get(1)?;
        out.push((id, blob_to_f32s(&blob)?));
    }
    Ok(out)
}

pub fn count(conn: &Connection, model: &str, input_kind: &str) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM family_embeddings WHERE model = ?1 AND input_kind = ?2",
        params![model, input_kind],
        |r| r.get(0),
    )?;
    Ok(n)
}

/// Clear embeddings. With both `model` and `input_kind` = None, wipes
/// the entire table — used by `--re-embed`. Pass either to scope it.
pub fn clear(
    conn: &Connection,
    model: Option<&str>,
    input_kind: Option<&str>,
) -> Result<usize> {
    let n = match (model, input_kind) {
        (Some(m), Some(k)) => conn.execute(
            "DELETE FROM family_embeddings WHERE model = ?1 AND input_kind = ?2",
            params![m, k],
        )?,
        (Some(m), None) => conn.execute(
            "DELETE FROM family_embeddings WHERE model = ?1",
            params![m],
        )?,
        (None, Some(k)) => conn.execute(
            "DELETE FROM family_embeddings WHERE input_kind = ?1",
            params![k],
        )?,
        (None, None) => conn.execute("DELETE FROM family_embeddings", [])?,
    };
    Ok(n)
}
