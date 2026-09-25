//! Secure Enclave / Secret Store (Swarm OS Bullet 85)
//!
//! Secure enclave for injecting API keys and credentials into sandboxes.

use std::collections::HashMap;
use std::sync::RwLock;

pub struct SecretStore {
    /// Maps cell IDs to a map of environment variables/secrets.
    vault: RwLock<HashMap<String, HashMap<String, String>>>,
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore {
    pub fn new() -> Self {
        Self {
            vault: RwLock::new(HashMap::new()),
        }
    }

    pub fn inject_secret(&self, cell_id: &str, key: &str, value: &str) {
        let mut map = self.vault.write().unwrap_or_else(|e| e.into_inner());
        map.entry(cell_id.to_string())
            .or_default()
            .insert(key.to_string(), value.to_string());
    }

    pub fn get_secret(&self, cell_id: &str, key: &str) -> Option<String> {
        let map = self.vault.read().unwrap_or_else(|e| e.into_inner());
        map.get(cell_id)
            .and_then(|secrets| secrets.get(key).cloned())
    }
}
