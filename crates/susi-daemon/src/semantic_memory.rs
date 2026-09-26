//! Semantic Memory Graph (Swarm OS Bullet 13)
//!
//! Provides a native vector-backed episodic memory API allowing cells
//! to recall facts using semantic similarity. Uses a fast in-memory
//! store with brute-force cosine similarity for demonstration, satisfying
//! the latency requirement of < 50ms.

use crate::susi_abi::memory::{
    EpisodicMemory, MemoryFragment, MemorySearchRequest, MemorySearchResult,
};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Per-namespace tenant access control (Swarm OS Bullet 39): "multi-tenant
/// memory namespaces so different projects/orgs coexist safely." Deny by
/// default — a cell may only read or write a namespace it's been
/// explicitly granted, so one tenant's project namespace can't be read or
/// polluted by another's cells.
#[derive(Default)]
pub struct NamespaceAcl {
    grants: RwLock<HashMap<String, HashSet<String>>>, // namespace -> cell_ids
}

impl NamespaceAcl {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grant(&self, namespace: &str, cell_id: &str) {
        self.grants
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(namespace.to_string())
            .or_default()
            .insert(cell_id.to_string());
    }

    pub fn revoke(&self, namespace: &str, cell_id: &str) {
        if let Some(members) = self
            .grants
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(namespace)
        {
            members.remove(cell_id);
        }
    }

    pub fn is_authorized(&self, namespace: &str, cell_id: &str) -> bool {
        self.grants
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(namespace)
            .is_some_and(|members| members.contains(cell_id))
    }
}

/// A simple, fast in-memory episodic memory store.
pub struct VectorMemoryStore {
    // Maps namespace -> list of fragments
    store: RwLock<HashMap<String, Vec<MemoryFragment>>>,
    acl: NamespaceAcl,
}

impl VectorMemoryStore {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
            acl: NamespaceAcl::new(),
        }
    }

    /// Grants `cell_id` access to `namespace` (Bullet 39).
    pub fn grant_namespace_access(&self, namespace: &str, cell_id: &str) {
        self.acl.grant(namespace, cell_id);
    }

    pub fn revoke_namespace_access(&self, namespace: &str, cell_id: &str) {
        self.acl.revoke(namespace, cell_id);
    }

    /// Tenant-isolated write (Bullet 39): only succeeds if `cell_id` has
    /// been granted access to `fragment.namespace`. The plain `store`
    /// ([`EpisodicMemory`] trait method) stays ungated for callers that
    /// already enforce access elsewhere (e.g. a MAC capability check
    /// upstream of this store).
    pub fn store_as(&self, cell_id: &str, fragment: MemoryFragment) -> Result<(), String> {
        if !self.acl.is_authorized(&fragment.namespace, cell_id) {
            return Err(format!(
                "cell '{cell_id}' has no access to namespace '{}'",
                fragment.namespace
            ));
        }
        self.store(fragment)
    }

    /// Tenant-isolated search (Bullet 39): same gate as [`store_as`](Self::store_as).
    pub fn search_as(
        &self,
        cell_id: &str,
        req: MemorySearchRequest,
    ) -> Result<Vec<MemorySearchResult>, String> {
        if !self.acl.is_authorized(&req.namespace, cell_id) {
            return Err(format!(
                "cell '{cell_id}' has no access to namespace '{}'",
                req.namespace
            ));
        }
        self.search(req)
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

    fn fragment(namespace: &str, id: &str) -> MemoryFragment {
        MemoryFragment {
            id: id.to_string(),
            namespace: namespace.to_string(),
            embedding: vec![1.0, 0.0, 0.0],
            payload: "fact".to_string(),
            timestamp_sec: 0,
        }
    }

    #[test]
    fn ungranted_cell_cannot_write_or_read_the_namespace() {
        let store = VectorMemoryStore::new();
        assert!(
            store
                .store_as("cell-a", fragment("tenant-x", "mem-1"))
                .is_err()
        );

        let req = MemorySearchRequest {
            namespace: "tenant-x".to_string(),
            query_embedding: vec![1.0, 0.0, 0.0],
            limit: 10,
            min_score: 0.0,
        };
        assert!(store.search_as("cell-a", req).is_err());
    }

    #[test]
    fn granted_cell_can_write_and_read_its_namespace() {
        let store = VectorMemoryStore::new();
        store.grant_namespace_access("tenant-x", "cell-a");
        store
            .store_as("cell-a", fragment("tenant-x", "mem-1"))
            .unwrap();

        let req = MemorySearchRequest {
            namespace: "tenant-x".to_string(),
            query_embedding: vec![1.0, 0.0, 0.0],
            limit: 10,
            min_score: 0.0,
        };
        let results = store.search_as("cell-a", req).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn a_grant_on_one_namespace_does_not_leak_into_another() {
        let store = VectorMemoryStore::new();
        store.grant_namespace_access("tenant-x", "cell-a");

        // cell-a is authorized for tenant-x but not tenant-y: one tenant's
        // grant must not cross into another's namespace.
        assert!(
            store
                .store_as("cell-a", fragment("tenant-y", "mem-1"))
                .is_err()
        );
    }

    #[test]
    fn revoking_access_denies_further_writes() {
        let store = VectorMemoryStore::new();
        store.grant_namespace_access("tenant-x", "cell-a");
        store
            .store_as("cell-a", fragment("tenant-x", "mem-1"))
            .unwrap();

        store.revoke_namespace_access("tenant-x", "cell-a");
        assert!(
            store
                .store_as("cell-a", fragment("tenant-x", "mem-2"))
                .is_err()
        );
    }
}
