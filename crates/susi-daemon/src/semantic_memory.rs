//! Semantic Memory Graph (Swarm OS Bullet 13)
//!
//! Provides a native vector-backed episodic memory API allowing cells
//! to recall facts using semantic similarity. Uses a fast in-memory
//! store with brute-force cosine similarity for demonstration, satisfying
//! the latency requirement of < 50ms.

use std::collections::HashMap;
use std::sync::RwLock;
use susi_abi::memory::{EpisodicMemory, MemoryFragment, MemorySearchRequest, MemorySearchResult};

/// A simple, fast in-memory episodic memory store.
pub struct VectorMemoryStore {
    // Maps namespace -> list of fragments
    store: RwLock<HashMap<String, Vec<MemoryFragment>>>,
}

impl VectorMemoryStore {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for VectorMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

/// Calculates the cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot_product = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;

    for (x, y) in a.iter().zip(b.iter()) {
        dot_product += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a.sqrt() * norm_b.sqrt())
}

impl EpisodicMemory for VectorMemoryStore {
    fn store(&self, fragment: MemoryFragment) -> Result<(), String> {
        let mut map = self.store.write().map_err(|e| e.to_string())?;
        let entry = map
            .entry(fragment.namespace.clone())
            .or_insert_with(Vec::new);
        entry.push(fragment);
        Ok(())
    }

    fn search(&self, req: MemorySearchRequest) -> Result<Vec<MemorySearchResult>, String> {
        let map = self.store.read().map_err(|e| e.to_string())?;

        let namespace_fragments = match map.get(&req.namespace) {
            Some(frags) => frags,
            None => return Ok(Vec::new()),
        };

        let mut results: Vec<MemorySearchResult> = namespace_fragments
            .iter()
            .map(|f| {
                let sim = cosine_similarity(&f.embedding, &req.query_embedding);
                MemorySearchResult {
                    fragment: f.clone(),
                    cosine_similarity: sim,
                }
            })
            .filter(|r| r.cosine_similarity >= req.min_score)
            .collect();

        // Sort by similarity descending
        results.sort_by(|a, b| {
            b.cosine_similarity
                .partial_cmp(&a.cosine_similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Truncate to limit
        results.truncate(req.limit);

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let c = vec![1.0, 0.0, 0.0];
        let d = vec![0.5, 0.5, 0.0];

        assert_eq!(cosine_similarity(&a, &b), 0.0);
        assert_eq!(cosine_similarity(&a, &c), 1.0);
        // Expected cosine similarity of [1,0,0] vs [0.5,0.5,0]; incidentally
        // close to FRAC_1_SQRT_2 but not standing in for it.
        #[allow(clippy::approx_constant)]
        let expected = 0.7071;
        assert!((cosine_similarity(&a, &d) - expected).abs() < 0.001);
    }

    #[test]
    fn test_memory_store_and_search() {
        let store = VectorMemoryStore::new();

        let f1 = MemoryFragment {
            id: "mem-1".to_string(),
            namespace: "default".to_string(),
            embedding: vec![1.0, 0.0, 0.0],
            payload: "fact 1".to_string(),
            timestamp_sec: 100,
        };

        let f2 = MemoryFragment {
            id: "mem-2".to_string(),
            namespace: "default".to_string(),
            embedding: vec![0.0, 1.0, 0.0],
            payload: "fact 2".to_string(),
            timestamp_sec: 200,
        };

        store.store(f1).unwrap();
        store.store(f2).unwrap();

        let req = MemorySearchRequest {
            namespace: "default".to_string(),
            query_embedding: vec![1.0, 0.0, 0.0],
            limit: 10,
            min_score: 0.5,
        };

        let results = store.search(req).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].fragment.id, "mem-1");
        assert_eq!(results[0].cosine_similarity, 1.0);
    }
}
