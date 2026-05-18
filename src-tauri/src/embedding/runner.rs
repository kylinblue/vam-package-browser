//! Batched encode loop. For a given (model, input_kind) variant, finds
//! every `package_family` row with a non-empty `purpose` that doesn't
//! yet have a row in `family_embeddings`, builds the input text, encodes
//! in batches, and upserts. Re-runnable: skips already-embedded rows.

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection};

use crate::embedding::{model::encode_batch, storage, ModelChoice};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputKind {
    /// `purpose` text only — cleanest signal, narrowest match.
    Purpose,
    /// Tag list prefixed before the purpose text — adds taxonomy signal.
    /// Format: `"{tag1} {tag2} ... — {purpose}"` so a query like
    /// "audio plugin" matches both the tag `function:audio-management`
    /// and the purpose blurb.
    PurposeWithTags,
}

impl InputKind {
    pub fn all() -> &'static [InputKind] {
        &[InputKind::Purpose, InputKind::PurposeWithTags]
    }

    pub fn name(&self) -> &'static str {
        match self {
            InputKind::Purpose => "purpose",
            InputKind::PurposeWithTags => "purpose-with-tags",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "purpose" | "p" => Ok(InputKind::Purpose),
            "purpose-with-tags" | "tags" | "pwt" => Ok(InputKind::PurposeWithTags),
            other => Err(anyhow!(
                "unknown input kind '{other}' (expected: purpose | purpose-with-tags)"
            )),
        }
    }
}

#[derive(Debug, Default)]
pub struct EmbedRunStats {
    pub model: String,
    pub input_kind: String,
    pub candidates: usize,
    pub embedded: usize,
    pub skipped_empty: usize,
    pub elapsed_secs: f64,
}

#[derive(Debug)]
struct FamilyRow {
    id: i64,
    purpose: Option<String>,
    tags: Vec<String>,
}

/// Pull every family that has a non-empty purpose and *no existing
/// embedding* for the given (model, input_kind). One-shot read into
/// memory; the family count is bounded (~3.7k rows here).
fn load_pending(
    conn: &Connection,
    model: &str,
    input_kind: &str,
    needs_tags: bool,
    limit: Option<usize>,
) -> Result<Vec<FamilyRow>> {
    // LEFT JOIN against existing embeddings — rows missing from
    // family_embeddings drop through with NULL, and we filter those.
    let limit_sql = match limit {
        Some(n) => format!(" LIMIT {n}"),
        None => String::new(),
    };
    let sql = format!(
        "SELECT pf.id, pf.purpose
         FROM package_family pf
         LEFT JOIN family_embeddings fe
             ON fe.family_id = pf.id
            AND fe.model = ?1
            AND fe.input_kind = ?2
         WHERE pf.purpose IS NOT NULL
           AND TRIM(pf.purpose) <> ''
           AND fe.family_id IS NULL
         ORDER BY pf.id{limit_sql}"
    );

    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query(params![model, input_kind])?;
    let mut out: Vec<FamilyRow> = Vec::new();
    while let Some(row) = rows.next()? {
        out.push(FamilyRow {
            id: row.get(0)?,
            purpose: row.get(1)?,
            tags: Vec::new(),
        });
    }

    if needs_tags && !out.is_empty() {
        // Single follow-up query for all tags, joined in code rather
        // than firing N queries.
        let ids_csv = out
            .iter()
            .map(|r| r.id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let tag_sql = format!(
            "SELECT family_id, tag FROM family_tags
             WHERE family_id IN ({ids_csv})
             ORDER BY family_id, tag"
        );
        let mut tag_stmt = conn.prepare(&tag_sql)?;
        let mut tag_rows = tag_stmt.query([])?;
        let mut by_id: std::collections::HashMap<i64, Vec<String>> =
            std::collections::HashMap::new();
        while let Some(row) = tag_rows.next()? {
            let id: i64 = row.get(0)?;
            let tag: String = row.get(1)?;
            by_id.entry(id).or_default().push(tag);
        }
        for fam in &mut out {
            if let Some(tags) = by_id.remove(&fam.id) {
                fam.tags = tags;
            }
        }
    }

    Ok(out)
}

fn build_input_text(fam: &FamilyRow, kind: InputKind) -> Option<String> {
    let purpose = fam.purpose.as_deref()?.trim();
    if purpose.is_empty() {
        return None;
    }
    match kind {
        InputKind::Purpose => Some(purpose.to_string()),
        InputKind::PurposeWithTags => {
            if fam.tags.is_empty() {
                Some(purpose.to_string())
            } else {
                Some(format!("{} — {purpose}", fam.tags.join(" ")))
            }
        }
    }
}

/// Encode every pending family for one (model, input_kind) variant.
/// Progress logged every batch. Errors on a single batch abort the run
/// — partial progress is preserved (each batch commits inside its own
/// transaction). Resume by re-running.
pub fn embed_missing(
    conn: &Arc<Mutex<Connection>>,
    model: ModelChoice,
    input_kind: InputKind,
    limit: Option<usize>,
    batch_size: usize,
) -> Result<EmbedRunStats> {
    let start = Instant::now();
    let needs_tags = matches!(input_kind, InputKind::PurposeWithTags);

    let pending = {
        let conn = conn.lock();
        load_pending(&conn, model.name(), input_kind.name(), needs_tags, limit)?
    };
    let candidates = pending.len();

    eprintln!(
        "[embed] {} / {}: {candidates} candidate families",
        model.name(),
        input_kind.name()
    );

    let mut embedded = 0usize;
    let mut skipped_empty = 0usize;

    for (batch_idx, chunk) in pending.chunks(batch_size).enumerate() {
        // Build inputs for this batch; skip any that come up empty.
        let mut ids: Vec<i64> = Vec::with_capacity(chunk.len());
        let mut texts: Vec<String> = Vec::with_capacity(chunk.len());
        for fam in chunk {
            match build_input_text(fam, input_kind) {
                Some(text) => {
                    ids.push(fam.id);
                    texts.push(text);
                }
                None => skipped_empty += 1,
            }
        }
        if texts.is_empty() {
            continue;
        }

        let vectors = encode_batch(model, &texts, Some(batch_size))?;
        if vectors.len() != texts.len() {
            return Err(anyhow!(
                "encoder returned {} vectors for {} inputs (model {}, input {})",
                vectors.len(),
                texts.len(),
                model.name(),
                input_kind.name()
            ));
        }

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        {
            let conn = conn.lock();
            let tx = conn.unchecked_transaction()?;
            for (id, vec) in ids.iter().zip(vectors.iter()) {
                storage::upsert(&tx, *id, model.name(), input_kind.name(), vec, now)?;
            }
            tx.commit()?;
        }
        embedded += ids.len();

        eprintln!(
            "[embed] {} / {}: batch {} done — {embedded}/{candidates} ({:.1}/s)",
            model.name(),
            input_kind.name(),
            batch_idx + 1,
            embedded as f64 / start.elapsed().as_secs_f64().max(0.001),
        );
    }

    Ok(EmbedRunStats {
        model: model.name().to_string(),
        input_kind: input_kind.name().to_string(),
        candidates,
        embedded,
        skipped_empty,
        elapsed_secs: start.elapsed().as_secs_f64(),
    })
}

#[derive(Debug)]
pub struct VariantStatus {
    pub model: String,
    pub input_kind: String,
    pub embedded: i64,
    pub total_eligible: i64,
}

/// How many families have an embedding for each (model, input_kind),
/// vs the count of families with a non-empty purpose (the eligible
/// denominator).
pub fn status(conn: &Connection) -> Result<Vec<VariantStatus>> {
    let total_eligible: i64 = conn.query_row(
        "SELECT COUNT(*) FROM package_family
         WHERE purpose IS NOT NULL AND TRIM(purpose) <> ''",
        [],
        |r| r.get(0),
    )?;
    let mut out = Vec::new();
    for &m in ModelChoice::all() {
        for &k in InputKind::all() {
            let embedded = storage::count(conn, m.name(), k.name())?;
            out.push(VariantStatus {
                model: m.name().to_string(),
                input_kind: k.name().to_string(),
                embedded,
                total_eligible,
            });
        }
    }
    Ok(out)
}
