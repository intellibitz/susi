//! Episodic Memory API
//!
//! Provides a native vector-backed episodic memory interface allowing
//! Swarm Cells to store and recall high-dimensional semantic facts over time.

use serde::{Deserialize, Serialize};

/// A memory fragment representing a past event, fact, or semantic context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryFragment {
    /// Unique identifier for this memory.
    pub id: String,
    /// The namespace or context this memory belongs to.
    pub namespace: String,
    /// High-dimensional vector embedding (e.g. 384d or 768d float array).
    pub embedding: Vec<f32>,
    /// The raw text or payload associated with the memory.
    pub payload: String,
    /// Unix timestamp when the memory was created.
    pub timestamp_sec: u64,
}

/// Request to search the episodic memory store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchRequest {
    /// The target namespace to search within.
    pub namespace: String,
    /// The query vector to match against.
    pub query_embedding: Vec<f32>,
    /// Maximum number of results to return.
    pub limit: usize,
    /// Optional minimum cosine similarity score threshold (0.0 to 1.0).
    pub min_score: f32,
}

/// Result of a memory search operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySearchResult {
    pub fragment: MemoryFragment,
    pub cosine_similarity: f32,
}

/// Interface for the Swarm OS Episodic Memory backend.
pub trait EpisodicMemory: Send + Sync {
    /// Stores a new memory fragment.
    fn store(&self, fragment: MemoryFragment) -> Result<(), String>;

    /// Searches for semantically similar memories.
    fn search(&self, req: MemorySearchRequest) -> Result<Vec<MemorySearchResult>, String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct InMemoryEpisodicStore {
        fragments: Mutex<Vec<MemoryFragment>>,
    }

    impl InMemoryEpisodicStore {
        fn new() -> Self {
            Self {
                fragments: Mutex::new(Vec::new()),
            }
        }

        fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
            if a.len() != b.len() || a.is_empty() {
                return 0.0;
            }
            let mut dot = 0.0_f32;
            let mut norm_a = 0.0_f32;
            let mut norm_b = 0.0_f32;
            for (&x, &y) in a.iter().zip(b.iter()) {
                dot += x * y;
                norm_a += x * x;
                norm_b += y * y;
            }
            let denom = norm_a.sqrt() * norm_b.sqrt();
            if denom <= f32::EPSILON {
                0.0
            } else {
                dot / denom
            }
        }
    }

    impl EpisodicMemory for InMemoryEpisodicStore {
        fn store(&self, fragment: MemoryFragment) -> Result<(), String> {
            self.fragments
                .lock()
                .map_err(|_| "poisoned lock".to_string())?
                .push(fragment);
            Ok(())
        }

        fn search(&self, req: MemorySearchRequest) -> Result<Vec<MemorySearchResult>, String> {
            let guard = self
                .fragments
                .lock()
                .map_err(|_| "poisoned lock".to_string())?;
            let mut matches: Vec<MemorySearchResult> = guard
                .iter()
                .filter(|f| f.namespace == req.namespace)
                .filter_map(|f| {
                    let score = Self::cosine_similarity(&f.embedding, &req.query_embedding);
                    (score >= req.min_score).then(|| MemorySearchResult {
                        fragment: f.clone(),
                        cosine_similarity: score,
                    })
                })
                .collect();
            matches.sort_by(|a, b| b.cosine_similarity.total_cmp(&a.cosine_similarity));
            matches.truncate(req.limit);
            Ok(matches)
        }
    }

    #[test]
    fn memory_dtos_round_trip_serde_and_episodic_trait_filters_and_ranks_by_cosine() {
        let frag_a = MemoryFragment {
            id: "mem-1".to_string(),
            namespace: "ws-alpha".to_string(),
            embedding: vec![1.0, 0.0, 0.0],
            payload: "exact axis match".to_string(),
            timestamp_sec: 100,
        };
        let encoded_frag = serde_json::to_string(&frag_a).expect("serialize fragment");
        let decoded_frag: MemoryFragment =
            serde_json::from_str(&encoded_frag).expect("deserialize fragment");
        assert_eq!(decoded_frag, frag_a);

        let search_req = MemorySearchRequest {
            namespace: "ws-alpha".to_string(),
            query_embedding: vec![1.0, 0.0, 0.0],
            limit: 2,
            min_score: 0.5,
        };
        let encoded_req = serde_json::to_string(&search_req).expect("serialize search req");
        let decoded_req: MemorySearchRequest =
            serde_json::from_str(&encoded_req).expect("deserialize search req");
        assert_eq!(decoded_req.namespace, "ws-alpha");
        assert_eq!(decoded_req.limit, 2);

        let store = InMemoryEpisodicStore::new();
        store.store(frag_a.clone()).expect("store frag_a");
        store
            .store(MemoryFragment {
                id: "mem-2".to_string(),
                namespace: "ws-alpha".to_string(),
                embedding: vec![0.8, 0.6, 0.0],
                payload: "diagonal match".to_string(),
                timestamp_sec: 101,
            })
            .expect("store frag_2");
        store
            .store(MemoryFragment {
                id: "mem-3".to_string(),
                namespace: "ws-alpha".to_string(),
                embedding: vec![0.0, 1.0, 0.0],
                payload: "orthogonal below threshold".to_string(),
                timestamp_sec: 102,
            })
            .expect("store frag_3");
        store
            .store(MemoryFragment {
                id: "mem-other-ns".to_string(),
                namespace: "ws-beta".to_string(),
                embedding: vec![1.0, 0.0, 0.0],
                payload: "other namespace".to_string(),
                timestamp_sec: 103,
            })
            .expect("store other namespace");

        let hits = store.search(search_req).expect("search ws-alpha");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].fragment.id, "mem-1");
        assert!((hits[0].cosine_similarity - 1.0).abs() < 1e-5);
        assert_eq!(hits[1].fragment.id, "mem-2");
        assert!((hits[1].cosine_similarity - 0.8).abs() < 1e-5);

        let encoded_res = serde_json::to_string(&hits[0]).expect("serialize search result");
        let decoded_res: MemorySearchResult =
            serde_json::from_str(&encoded_res).expect("deserialize search result");
        assert_eq!(decoded_res.fragment, frag_a);
    }

    #[test]
    fn cosine_similarity_edge_cases_and_search_limit_zero_and_strict_min_score() {
        assert_eq!(
            InMemoryEpisodicStore::cosine_similarity(&[1.0, 0.0], &[1.0, 0.0, 0.0]),
            0.0
        );
        assert_eq!(InMemoryEpisodicStore::cosine_similarity(&[], &[]), 0.0);
        assert_eq!(
            InMemoryEpisodicStore::cosine_similarity(&[0.0, 0.0, 0.0], &[1.0, 0.0, 0.0]),
            0.0
        );

        let store = InMemoryEpisodicStore::new();
        store
            .store(MemoryFragment {
                id: "exact".to_string(),
                namespace: "ns".to_string(),
                embedding: vec![1.0, 0.0, 0.0],
                payload: "exact".to_string(),
                timestamp_sec: 1,
            })
            .expect("store exact");
        store
            .store(MemoryFragment {
                id: "partial".to_string(),
                namespace: "ns".to_string(),
                embedding: vec![0.8, 0.6, 0.0],
                payload: "partial".to_string(),
                timestamp_sec: 2,
            })
            .expect("store partial");

        let zero_limit = store
            .search(MemorySearchRequest {
                namespace: "ns".to_string(),
                query_embedding: vec![1.0, 0.0, 0.0],
                limit: 0,
                min_score: 0.0,
            })
            .expect("search with limit 0");
        assert!(zero_limit.is_empty());

        let strict_hits = store
            .search(MemorySearchRequest {
                namespace: "ns".to_string(),
                query_embedding: vec![1.0, 0.0, 0.0],
                limit: 10,
                min_score: 1.0,
            })
            .expect("search with min_score 1.0");
        assert_eq!(strict_hits.len(), 1);
        assert_eq!(strict_hits[0].fragment.id, "exact");
    }
}
