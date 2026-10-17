//! Semantic search using text embeddings
//!
//! Enables "find similar papers" functionality by computing
//! vector embeddings and using cosine similarity.
//!
//! # Model id (ADR-0028 D4)
//!
//! [`SemanticSearch::model_id`] returns the exact string this instance
//! stamps into `StoredVector.model` when writing, and must filter reads by
//! (`EmbeddingStore::has_vectors_for_model`,
//! `EmbeddingStore::load_vectors_by_type_and_model`). [`FASTEMBED_MODEL_ID`]
//! names that string for the default model (`AllMiniLML6V2`) — the exact
//! value the `impress.memory.embed` backfill executor (ADR-0028 D7) will
//! stamp on every vector it writes.

use std::path::PathBuf;

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use crate::PublicationEmbedding;

#[derive(Error, Debug)]
pub enum EmbeddingError {
    #[error("Model initialization failed: {0}")]
    InitError(String),
    #[error("Embedding generation failed: {0}")]
    EmbeddingFailed(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

/// Canonical id fastembed's default model (`AllMiniLML6V2`) stamps into
/// `StoredVector.model` and filters reads by (ADR-0028 D2, D4). Equal to
/// `SemanticSearch::new().model_id()`; kept as a named constant so call
/// sites that need the id without constructing a `SemanticSearch` (tests,
/// the backfill executor's deterministic vector-id scheme) don't restate the
/// string by hand. Defined ungated at the crate root — sidecar readers filter
/// by this id without linking the embedder — and re-exported here beside the
/// embedder it describes.
pub use crate::FASTEMBED_MODEL_ID;

/// Env var overriding fastembed's model cache directory. fastembed itself
/// already honors `FASTEMBED_CACHE_DIR` (`fastembed::common::get_cache_dir`),
/// but that name is easy to confuse with this suite's own `IMPRESS_*` env
/// grammar (`IMPRESS_SKIP_X86`, `IMPRESS_OMLX_URL`, ...). This is the name
/// documented and read *here*: set it to point every `SemanticSearch::new()`
/// / `with_model()` call at a pre-populated cache for offline or CI runs
/// that must not attempt the ~100MB first-run model download. Use
/// `with_model_and_cache_dir` instead when the directory should not come
/// from the environment.
pub const IMPRESS_FASTEMBED_CACHE_ENV: &str = "IMPRESS_FASTEMBED_CACHE";

fn canonical_model_id(model_name: &str) -> String {
    format!("fastembed/{model_name}")
}

/// Semantic search engine
pub struct SemanticSearch {
    model: TextEmbedding,
    model_name: String,
    model_id: String,
}

impl SemanticSearch {
    /// Initialize with the default model (all-MiniLM-L6-v2)
    pub fn new() -> Result<Self, EmbeddingError> {
        Self::with_model(EmbeddingModel::AllMiniLML6V2)
    }

    /// Initialize with a specific model. Honors
    /// [`IMPRESS_FASTEMBED_CACHE_ENV`] when set; use
    /// [`SemanticSearch::with_model_and_cache_dir`] to set the cache
    /// directory explicitly instead of through the environment.
    pub fn with_model(model: EmbeddingModel) -> Result<Self, EmbeddingError> {
        let model_name = format!("{:?}", model);

        let mut options = InitOptions::new(model).with_show_download_progress(true);
        if let Ok(dir) = std::env::var(IMPRESS_FASTEMBED_CACHE_ENV) {
            options = options.with_cache_dir(PathBuf::from(dir));
        }

        let text_embedding = TextEmbedding::try_new(options)
            .map_err(|e| EmbeddingError::InitError(e.to_string()))?;

        Ok(Self {
            model: text_embedding,
            model_id: canonical_model_id(&model_name),
            model_name,
        })
    }

    /// Initialize with a specific model and an explicit cache directory,
    /// taking precedence over [`IMPRESS_FASTEMBED_CACHE_ENV`]. For callers
    /// (tests, a configured backfill executor) that need a specific cache
    /// location regardless of the process environment.
    pub fn with_model_and_cache_dir(
        model: EmbeddingModel,
        cache_dir: impl Into<PathBuf>,
    ) -> Result<Self, EmbeddingError> {
        let model_name = format!("{:?}", model);

        let options = InitOptions::new(model)
            .with_show_download_progress(true)
            .with_cache_dir(cache_dir.into());

        let text_embedding = TextEmbedding::try_new(options)
            .map_err(|e| EmbeddingError::InitError(e.to_string()))?;

        Ok(Self {
            model: text_embedding,
            model_id: canonical_model_id(&model_name),
            model_name,
        })
    }

    /// The canonical model id this instance stamps into `StoredVector.model`
    /// and that model-aware reads must filter by (ADR-0028 D4). See the
    /// module docs and [`FASTEMBED_MODEL_ID`].
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Generate embedding for a publication
    ///
    /// Combines title, authors, and abstract for best results
    pub fn embed_publication(
        &self,
        publication_id: &str,
        title: &str,
        authors: &[String],
        abstract_text: Option<&str>,
    ) -> Result<PublicationEmbedding, EmbeddingError> {
        // Combine fields into a single text
        let mut text = title.to_string();

        if !authors.is_empty() {
            text.push_str(". Authors: ");
            text.push_str(&authors.join(", "));
        }

        if let Some(abstract_str) = abstract_text {
            text.push_str(". ");
            // Truncate abstract if too long (model has token limit)
            // Use char boundary-safe truncation to avoid panics on non-ASCII text
            let truncated: String = abstract_str.chars().take(1000).collect();
            text.push_str(&truncated);
        }

        let embeddings = self
            .model
            .embed(vec![text], None)
            .map_err(|e| EmbeddingError::EmbeddingFailed(e.to_string()))?;

        let vector = embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::EmbeddingFailed("No embedding returned".to_string()))?;

        Ok(PublicationEmbedding {
            publication_id: publication_id.to_string(),
            vector,
            model: self.model_name.clone(),
        })
    }

    /// Generate embeddings for multiple publications
    pub fn embed_publications(
        &self,
        publications: Vec<(String, String, Vec<String>, Option<String>)>,
    ) -> Result<Vec<PublicationEmbedding>, EmbeddingError> {
        let texts: Vec<String> = publications
            .iter()
            .map(|(_, title, authors, abstract_text)| {
                let mut text = title.clone();
                if !authors.is_empty() {
                    text.push_str(". Authors: ");
                    text.push_str(&authors.join(", "));
                }
                if let Some(abs) = abstract_text {
                    text.push_str(". ");
                    // Use char boundary-safe truncation to avoid panics on non-ASCII text
                    let truncated: String = abs.chars().take(1000).collect();
                    text.push_str(&truncated);
                }
                text
            })
            .collect();

        let embeddings = self
            .model
            .embed(texts, None)
            .map_err(|e| EmbeddingError::EmbeddingFailed(e.to_string()))?;

        Ok(publications
            .into_iter()
            .zip(embeddings)
            .map(|((id, _, _, _), vector)| PublicationEmbedding {
                publication_id: id,
                vector,
                model: self.model_name.clone(),
            })
            .collect())
    }

    /// Embed a search query
    pub fn embed_query(&self, query: &str) -> Result<Vec<f32>, EmbeddingError> {
        let embeddings = self
            .model
            .embed(vec![query.to_string()], None)
            .map_err(|e| EmbeddingError::EmbeddingFailed(e.to_string()))?;

        embeddings
            .into_iter()
            .next()
            .ok_or_else(|| EmbeddingError::EmbeddingFailed("No embedding returned".to_string()))
    }

    /// Embed arbitrary text — a general-purpose alias for callers that are
    /// not embedding a search query (a memory claim body, a
    /// `content-chunk@1.0.0`, ...). "Query" reads oddly for those callers,
    /// but the underlying call is identical, so this simply delegates to
    /// [`SemanticSearch::embed_query`] rather than duplicating it.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>, EmbeddingError> {
        self.embed_query(text)
    }
}

/// Embedding storage format for persistence
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEmbedding {
    pub publication_id: String,
    pub vector: Vec<f32>,
    pub model: String,
    pub created_at: String,
}

impl From<PublicationEmbedding> for StoredEmbedding {
    fn from(emb: PublicationEmbedding) -> Self {
        Self {
            publication_id: emb.publication_id,
            vector: emb.vector,
            model: emb.model,
            created_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // No test here constructs a `SemanticSearch`: that downloads a ~100MB
    // ONNX model on first run, which has no place in a unit test run. The
    // id-formatting logic is pure, so it is tested directly instead.

    #[test]
    fn test_canonical_model_id_matches_default_constant() {
        // "AllMiniLML6V2" is `format!("{:?}", EmbeddingModel::AllMiniLML6V2)`
        // — what `SemanticSearch::new()` computes internally. This asserts
        // that computation lines up with the string documented (and relied
        // on by the backfill executor) as `FASTEMBED_MODEL_ID`.
        assert_eq!(canonical_model_id("AllMiniLML6V2"), FASTEMBED_MODEL_ID);
    }

    #[test]
    fn test_canonical_model_id_namespaces_by_model() {
        assert_eq!(
            canonical_model_id("BGESmallENV15"),
            "fastembed/BGESmallENV15"
        );
    }
}
