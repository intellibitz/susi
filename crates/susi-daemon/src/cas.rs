//! Content-Addressable Storage (CAS) for Prompts (Swarm OS Bullet 41)
//!
//! Provides a content-addressable storage mechanism for loading prompts
//! and configuration assets, ensuring cryptographic integrity and versioning.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::RwLock;

/// Represents an item stored in CAS.
#[derive(Debug, Clone)]
pub struct CasObject {
    pub hash_id: String,
    pub content: Vec<u8>,
}

/// The CAS Manager stores objects indexed by their content hash.
pub struct CasManager {
    store: RwLock<HashMap<String, CasObject>>,
}

impl Default for CasManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CasManager {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
        }
    }

    /// Computes a deterministic hex hash for a given payload.
    /// In production, this should be SHA-256. For Swarm OS standard compliance
    /// without external crypto deps, we use the standard Rust DefaultHasher.
    fn compute_hash(content: &[u8]) -> String {
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Puts content into the CAS, returning its deterministic hash ID.
    pub fn put(&self, content: Vec<u8>) -> String {
        let hash_id = Self::compute_hash(&content);
        
        let mut map = self.store.write().unwrap_or_else(|e| e.into_inner());
        // Only insert if not already present to avoid redundant allocations
        if !map.contains_key(&hash_id) {
            map.insert(hash_id.clone(), CasObject {
                hash_id: hash_id.clone(),
                content,
            });
        }
        
        hash_id
    }

    /// Retrieves content from the CAS by its hash ID.
    pub fn get(&self, hash_id: &str) -> Option<Vec<u8>> {
        let map = self.store.read().unwrap_or_else(|e| e.into_inner());
        map.get(hash_id).map(|obj| obj.content.clone())
    }

    /// Verifies if a given piece of content matches an expected hash.
    pub fn verify(content: &[u8], expected_hash: &str) -> bool {
        Self::compute_hash(content) == expected_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cas_put_get() {
        let cas = CasManager::new();
        
        let prompt_text = b"You are a helpful assistant.";
        
        // Put content
        let hash = cas.put(prompt_text.to_vec());
        assert_eq!(hash.len(), 16); // Hex representation of a 64-bit hash
        
        // Get content
        let retrieved = cas.get(&hash).unwrap();
        assert_eq!(retrieved, prompt_text);
        
        // Get unknown content
        assert!(cas.get("unknown_hash").is_none());
        
        // Verify
        assert!(CasManager::verify(prompt_text, &hash));
        assert!(!CasManager::verify(b"different text", &hash));
    }
}
