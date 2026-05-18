//! fastembed model wrappers. Models are large (130-250 MB) and slow to
//! initialize (~1-3s of ONNX graph load + tokenizer setup), so each model
//! lives behind a lazily-initialized Mutex singleton. First call to a
//! model triggers download (cached under HF cache dir, ~/.cache by
//! default) and graph init; subsequent calls reuse the loaded session.

use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use once_cell::sync::Lazy;

/// Supported embedding models. The `name()` value is what gets persisted
/// in the `family_embeddings.model` column — pick names that are stable
/// and self-describing so a future schema reader can identify them
/// without consulting this code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelChoice {
    BgeSmallEnV15,
    NomicEmbedTextV15,
}

impl ModelChoice {
    pub fn all() -> &'static [ModelChoice] {
        &[ModelChoice::BgeSmallEnV15, ModelChoice::NomicEmbedTextV15]
    }

    /// Persisted name. Stable across versions; new models get new names.
    pub fn name(&self) -> &'static str {
        match self {
            ModelChoice::BgeSmallEnV15 => "bge-small-en-v1.5",
            ModelChoice::NomicEmbedTextV15 => "nomic-embed-text-v1.5",
        }
    }

    pub fn dim(&self) -> usize {
        match self {
            ModelChoice::BgeSmallEnV15 => 384,
            ModelChoice::NomicEmbedTextV15 => 768,
        }
    }

    /// CLI-friendly short alias for `--models`.
    pub fn parse(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "bge" | "bge-small" | "bge-small-en-v1.5" => Ok(ModelChoice::BgeSmallEnV15),
            "nomic" | "nomic-embed" | "nomic-embed-text-v1.5" => {
                Ok(ModelChoice::NomicEmbedTextV15)
            }
            other => Err(anyhow!(
                "unknown model '{other}' (expected: bge | nomic, or the full name)"
            )),
        }
    }

    fn fastembed_kind(&self) -> EmbeddingModel {
        match self {
            ModelChoice::BgeSmallEnV15 => EmbeddingModel::BGESmallENV15,
            ModelChoice::NomicEmbedTextV15 => EmbeddingModel::NomicEmbedTextV15,
        }
    }
}

static BGE: Lazy<Mutex<Option<TextEmbedding>>> = Lazy::new(|| Mutex::new(None));
static NOMIC: Lazy<Mutex<Option<TextEmbedding>>> = Lazy::new(|| Mutex::new(None));

fn slot(choice: ModelChoice) -> &'static Mutex<Option<TextEmbedding>> {
    match choice {
        ModelChoice::BgeSmallEnV15 => &BGE,
        ModelChoice::NomicEmbedTextV15 => &NOMIC,
    }
}

/// Encode a batch of documents. Initializes the model on first call.
///
/// `batch_size` is forwarded to fastembed which then chunks internally
/// for ONNX session efficiency. Pass `None` to let the library choose.
pub fn encode_batch(
    choice: ModelChoice,
    texts: &[String],
    batch_size: Option<usize>,
) -> Result<Vec<Vec<f32>>> {
    let mut slot = slot(choice).lock().unwrap();
    if slot.is_none() {
        eprintln!("[embed] initializing model {} (first use — may download ~100-250 MB)", choice.name());
        let model = TextEmbedding::try_new(
            InitOptions::new(choice.fastembed_kind()).with_show_download_progress(true),
        )
        .with_context(|| format!("init fastembed model {}", choice.name()))?;
        *slot = Some(model);
    }
    let model = slot.as_ref().expect("just initialized");
    let vectors = model
        .embed(texts.to_vec(), batch_size)
        .with_context(|| format!("encode batch ({} docs) with {}", texts.len(), choice.name()))?;

    if let Some(first) = vectors.first() {
        if first.len() != choice.dim() {
            return Err(anyhow!(
                "{}: expected dim {}, fastembed produced {}",
                choice.name(),
                choice.dim(),
                first.len()
            ));
        }
    }
    Ok(vectors)
}
