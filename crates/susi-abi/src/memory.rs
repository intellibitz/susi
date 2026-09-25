//! Episodic Memory API (Swarm OS Bullet 13)
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
