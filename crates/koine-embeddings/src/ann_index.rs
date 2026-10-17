//! Approximate Nearest Neighbor index using HNSW
//!
//! Provides O(log n) similarity search for embeddings, significantly faster
//! than brute-force O(n) search for large collections.

use hnsw_rs::prelude::*;

use serde::{Deserialize, Serialize};
use std::sync::RwLock;

/// Result of a similarity search
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnnSimilarityResult {
    pub publication_id: String,
    pub similarity: f32,
}

/// Configuration for the HNSW index
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnnIndexConfig {
    /// Maximum number of connections per node (M parameter)
    pub max_connections: usize,
    /// Initial capacity
    pub capacity: usize,
    /// Maximum layer depth
    pub max_layer: usize,
    /// Construction-time search width
    pub ef_construction: usize,
}

impl Default for AnnIndexConfig {
    fn default() -> Self {
        Self {
            max_connections: 16,
            capacity: 10000,
            max_layer: 16,
            ef_construction: 200,
        }
    }
}

/// Below this size, `search` uses an exact brute-force scan instead of the
/// HNSW graph. hnsw_rs 0.3.4's layered search probabilistically drops
/// reachable points on small graphs (measured: ~7% of 3-point indexes
/// return <3 results at k=3 regardless of ef; the loss rate grows sharply
/// as k approaches n). Exact scan is both correct and faster at this
/// scale; the HNSW path takes over where its log-scaling actually pays.
const EXACT_SEARCH_THRESHOLD: usize = 1024;

/// HNSW index for fast similarity search
pub struct AnnIndex {
    hnsw: RwLock<Hnsw<'static, f32, DistCosine>>,
    id_map: RwLock<Vec<String>>,
    /// Vector copies for the exact-search path (and eventual rebuild-on-load;
    /// `save()` currently persists only the id_map). Kept in insertion order,
    /// parallel to `id_map`.
    vectors: RwLock<Vec<Vec<f32>>>,
    config: AnnIndexConfig,
}

impl AnnIndex {
    /// Create a new empty index with default configuration
    pub fn new() -> Self {
        Self::with_config(AnnIndexConfig::default())
    }

    /// Create a new empty index with custom configuration
    pub fn with_config(config: AnnIndexConfig) -> Self {
        let hnsw = Hnsw::new(
            config.max_connections,
            config.capacity,
            config.max_layer,
            config.ef_construction,
            DistCosine,
        );
        Self {
            hnsw: RwLock::new(hnsw),
            id_map: RwLock::new(Vec::new()),
            vectors: RwLock::new(Vec::new()),
            config,
        }
    }

    /// Get the number of items in the index
    pub fn len(&self) -> usize {
        self.id_map.read().unwrap().len()
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add an embedding to the index
    pub fn add(&self, publication_id: &str, embedding: &[f32]) {
        let mut id_map = self.id_map.write().unwrap();
        let idx = id_map.len();
        id_map.push(publication_id.to_string());
        drop(id_map);
        self.vectors.write().unwrap().push(embedding.to_vec());

        let hnsw = self.hnsw.read().unwrap();
        hnsw.insert((embedding, idx));
    }

    /// Add multiple embeddings at once (more efficient for batch operations)
    pub fn add_batch(&self, items: Vec<(String, Vec<f32>)>) {
        if items.is_empty() {
            return;
        }

        let mut id_map = self.id_map.write().unwrap();
        let start_idx = id_map.len();

        let data: Vec<(&Vec<f32>, usize)> = items
            .iter()
            .enumerate()
            .map(|(i, (id, emb))| {
                id_map.push(id.clone());
                (emb, start_idx + i)
            })
            .collect();

        drop(id_map);
        {
            let mut vectors = self.vectors.write().unwrap();
            for (_, emb) in &items {
                vectors.push(emb.clone());
            }
        }

        let hnsw = self.hnsw.read().unwrap();
        for (emb, idx) in data {
            hnsw.insert((emb, idx));
        }
    }

    /// Find k most similar publications
    pub fn search(&self, query: &[f32], k: usize) -> Vec<AnnSimilarityResult> {
        if self.len() <= EXACT_SEARCH_THRESHOLD {
            return self.search_exact(query, k);
        }
        let ef_search = (k * 2).max(50); // Search beam width
        let hnsw = self.hnsw.read().unwrap();
        let id_map = self.id_map.read().unwrap();

        let results = hnsw.search(query, k, ef_search);

        results
            .into_iter()
            .map(|neighbour| AnnSimilarityResult {
                publication_id: id_map.get(neighbour.d_id).cloned().unwrap_or_default(),
                similarity: 1.0 - neighbour.distance, // Convert distance to similarity
            })
            .collect()
    }

    /// Exact top-k by cosine similarity over the stored vectors. Used for
    /// small indexes where HNSW recall is unreliable (see
    /// `EXACT_SEARCH_THRESHOLD`); O(n·d) but n is bounded by the threshold.
    fn search_exact(&self, query: &[f32], k: usize) -> Vec<AnnSimilarityResult> {
        let vectors = self.vectors.read().unwrap();
        let id_map = self.id_map.read().unwrap();
        let q_norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();

        let mut scored: Vec<AnnSimilarityResult> = vectors
            .iter()
            .zip(id_map.iter())
            .map(|(v, id)| {
                let dot: f32 = v.iter().zip(query).map(|(a, b)| a * b).sum();
                let v_norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                let denom = q_norm * v_norm;
                AnnSimilarityResult {
                    publication_id: id.clone(),
                    similarity: if denom > 0.0 { dot / denom } else { 0.0 },
                }
            })
            .collect();
        scored.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(k);
        scored
    }

    /// Serialize index to bytes
    pub fn save(&self) -> Result<Vec<u8>, String> {
        let id_map = self.id_map.read().unwrap();
        // Note: hnsw_rs doesn't directly support serialization of the index
        // We save the id_map and would need to rebuild the index on load
        bincode::serialize(&(id_map.clone(), &self.config))
            .map_err(|e| format!("Serialization error: {}", e))
    }

    /// Get the configuration used for this index
    pub fn config(&self) -> AnnIndexConfig {
        self.config.clone()
    }
}

impl Default for AnnIndex {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the small-index recall flake: hnsw_rs's layered
    /// search dropped reachable points ~7% of the time on a 3-point index
    /// (random layer assignment), which made `chunk_index`'s
    /// test_scoped_search fail intermittently and silently weakened scoped
    /// RAG search. The exact-scan path below EXACT_SEARCH_THRESHOLD must
    /// return complete results every time.
    #[test]
    fn test_small_index_recall_is_complete_and_deterministic() {
        for _ in 0..50 {
            let index = AnnIndex::new();
            index.add("a", &[1.0, 0.0, 0.0]);
            index.add("b", &[0.9, 0.1, 0.0]);
            index.add("c", &[0.95, 0.05, 0.0]);
            let results = index.search(&[1.0, 0.0, 0.0], 3);
            assert_eq!(results.len(), 3, "small-index search must be exhaustive");
            assert_eq!(results[0].publication_id, "a");
            assert_eq!(results[1].publication_id, "c");
            assert_eq!(results[2].publication_id, "b");
            assert!(results[0].similarity > 0.999);
        }
    }

    #[test]
    fn test_add_batch_feeds_exact_path() {
        let index = AnnIndex::new();
        index.add_batch(vec![
            ("a".into(), vec![1.0, 0.0, 0.0]),
            ("b".into(), vec![0.0, 1.0, 0.0]),
        ]);
        let results = index.search(&[0.0, 1.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].publication_id, "b");
    }

    #[test]
    fn test_ann_index_basic() {
        let index = AnnIndex::new();

        // Add some embeddings
        index.add("pub1", &[1.0, 0.0, 0.0]);
        index.add("pub2", &[0.9, 0.1, 0.0]);
        index.add("pub3", &[0.0, 1.0, 0.0]);

        assert_eq!(index.len(), 3);

        // Search
        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        // pub1 should be most similar to itself
        assert_eq!(results[0].publication_id, "pub1");
        assert!(results[0].similarity > 0.99);
    }

    #[test]
    fn test_ann_index_batch() {
        let index = AnnIndex::new();

        let items = vec![
            ("pub1".to_string(), vec![1.0, 0.0, 0.0]),
            ("pub2".to_string(), vec![0.0, 1.0, 0.0]),
            ("pub3".to_string(), vec![0.0, 0.0, 1.0]),
        ];

        index.add_batch(items);
        assert_eq!(index.len(), 3);

        let results = index.search(&[0.0, 1.0, 0.0], 1);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].publication_id, "pub2");
    }
}
