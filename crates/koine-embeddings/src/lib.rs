//! The impress suite's embedding stack.
//!
//! This crate owns **the maths and the sidecar**: cosine similarity, the HNSW
//! index and its exact-search fallback, the chunk index, the SQLite-backed
//! vector/chunk store, and the fastembed text embedder. It is plain Rust —
//! no UniFFI, no `lazy_static` handle registries, no app dependency — so any
//! consumer in the suite (an app core, a `*-service` trait, `impress-mcp`)
//! can link it without inheriting an app's FFI surface.
//!
//! **FFI lives with the app cores that ship frameworks.** `imbib-core` keeps
//! the handle-based `#[uniffi::export]` shims and mirror `uniffi::Record`
//! types that Swift binds against, and converts to and from the types here at
//! the boundary. Moving an export into this crate would put a second UniFFI
//! scaffolding in the graph for a crate that ships no framework; adding one
//! here would fork the Swift surface. Do neither.
//!
//! # Features
//!
//! - `store` (default) — the SQLite sidecar: [`EmbeddingStore`] and its rows.
//! - `index` (default) — [`AnnIndex`] and [`ChunkIndex`], the HNSW engines.
//! - `embedder` — [`SemanticSearch`], the fastembed model wrapper. Off by
//!   default: it pulls a ~100MB model download on first use.
//!
//! [`cosine_similarity`], [`find_similar`], [`SimilarityResult`] and
//! [`PublicationEmbedding`] are dependency-free and always available.

use serde::{Deserialize, Serialize};

#[cfg(feature = "index")]
pub mod ann_index;
#[cfg(feature = "index")]
pub mod chunk_index;
#[cfg(feature = "store")]
pub mod embedding_store;
#[cfg(feature = "embedder")]
pub mod semantic;

/// The canonical model id the suite's fastembed embedder stamps into
/// `StoredVector.model` (ADR-0028 D4). Ungated: readers that filter the
/// sidecar by model (coverage counts, recall) need the id without linking
/// the ONNX runtime the `embedder` feature drags in.
pub const FASTEMBED_MODEL_ID: &str = "fastembed/AllMiniLML6V2";

#[cfg(feature = "index")]
pub use ann_index::{AnnIndex, AnnIndexConfig, AnnSimilarityResult};
#[cfg(feature = "index")]
pub use chunk_index::{ChunkIndex, ChunkSimilarityResult};
#[cfg(feature = "store")]
pub use embedding_store::{
    EmbeddingIndexStatus, EmbeddingStore, ModelStats, PublicationEmbeddingStatus, SourceTypeCount,
    StoredChunk, StoredVector, SOURCE_TYPE_CHUNK, SOURCE_TYPE_PUBLICATION,
};
#[cfg(feature = "embedder")]
pub use semantic::{EmbeddingError, SemanticSearch, StoredEmbedding};

// ---------------------------------------------------------------------------
// Dependency-free similarity primitives
// ---------------------------------------------------------------------------

/// Embedding vector for a publication.
#[derive(Clone, Debug, PartialEq)]
pub struct PublicationEmbedding {
    pub publication_id: String,
    pub vector: Vec<f32>,
    pub model: String,
}

/// A publication scored against a query embedding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimilarityResult {
    pub publication_id: String,
    pub similarity: f32,
}

/// Compute cosine similarity between two vectors.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

/// Find most similar publications by embedding.
pub fn find_similar(
    query_embedding: &[f32],
    candidate_embeddings: Vec<PublicationEmbedding>,
    top_k: u32,
) -> Vec<SimilarityResult> {
    let top_k = top_k as usize;
    let mut results: Vec<SimilarityResult> = candidate_embeddings
        .into_iter()
        .map(|emb| {
            let similarity = cosine_similarity(query_embedding, &emb.vector);
            SimilarityResult {
                publication_id: emb.publication_id,
                similarity,
            }
        })
        .collect();

    // Sort by similarity descending
    results.sort_by(|a, b| {
        b.similarity
            .partial_cmp(&a.similarity)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    results.truncate(top_k);
    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.001);

        let c = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &c).abs() < 0.001);
    }

    #[test]
    fn test_find_similar() {
        let query = vec![1.0, 0.0, 0.0];
        let candidates = vec![
            PublicationEmbedding {
                publication_id: "a".to_string(),
                vector: vec![0.9, 0.1, 0.0],
                model: "test".to_string(),
            },
            PublicationEmbedding {
                publication_id: "b".to_string(),
                vector: vec![0.0, 1.0, 0.0],
                model: "test".to_string(),
            },
        ];

        let results = find_similar(&query, candidates, 2u32);
        assert_eq!(results[0].publication_id, "a");
        assert!(results[0].similarity > results[1].similarity);
    }
}
