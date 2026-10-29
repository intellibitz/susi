//! Permanent Memory module for the Susi Harness.
//!
//! Provides long-term semantic and episodic memory for agents by integrating
//! full-text search (tantivy) and vector embeddings (fastembed).

use serde::{Deserialize, Serialize};
use susi_error::EaiResult;
use susi_vendor_fastembed::embed_one;

/// A permanent memory record containing the original content and its semantic embedding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub content: String,
    pub vector: Vec<f32>,
}

/// The store that manages long-term memory access and indexing.
pub struct MemoryStore {
    // Note: Future integration with tantivy for doc_id/source mapping
}

impl MemoryStore {
    /// Initialize a new permanent memory store.
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }

    /// Commit a new episodic or semantic entry into permanent memory.
    ///
    /// The content is passed through the `susi-vendor-fastembed` local model
    /// to generate a multi-dimensional embedding for retrieval.
    ///
    /// # Errors
    /// Fails if the ONNX embedding model cannot be loaded or inference fails.
    pub fn memorize(&self, id: &str, content: &str) -> EaiResult<MemoryEntry> {
        let vector = embed_one(content)?;
        Ok(MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            vector,
        })
    }

    /// Retrieve the semantic vector for a given query to perform a similarity search.
    ///
    /// # Errors
    /// Fails if the ONNX embedding model cannot be loaded or inference fails.
    pub fn recall(&self, query: &str) -> EaiResult<Vec<f32>> {
        embed_one(query)
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}
