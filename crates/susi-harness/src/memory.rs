//! Embedding preparation for a future durable harness memory store.
//!
//! This module currently creates vectors only; it neither persists entries nor
//! retrieves prior records. It must not be advertised as permanent memory.

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

/// Compatibility handle for preparing memory entries; persistence is not implemented.
pub struct MemoryStore {
    // Note: Future integration with tantivy for doc_id/source mapping
}

impl MemoryStore {
    /// Initialize the embedding-preparation handle.
    #[must_use]
    pub fn new() -> Self {
        Self {}
    }

    /// Prepare an entry and its embedding without persisting it.
    ///
    /// The content is passed through the `susi-vendor-fastembed` local model
    /// to generate a multi-dimensional embedding for retrieval.
    ///
    /// # Errors
    /// Fails if the Candle embedding model cannot be loaded or inference fails.
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
    /// Fails if the Candle embedding model cannot be loaded or inference fails.
    pub fn recall(&self, query: &str) -> EaiResult<Vec<f32>> {
        embed_one(query)
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}
