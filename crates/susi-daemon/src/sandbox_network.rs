//! Strict Sandbox Network Allow-lists (Swarm OS Bullet 22)
//!
//! Enforces strict outbound network filtering for sandboxes.

use std::collections::HashSet;
use std::sync::RwLock;

pub struct NetworkFirewall {
    /// Maps cell IDs to allowed outbound domains.
    allow_lists: RwLock<std::collections::HashMap<String, HashSet<String>>>,
}

impl Default for NetworkFirewall {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkFirewall {
    pub fn new() -> Self {
        Self {
            allow_lists: RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub fn grant_access(&self, cell_id: &str, domain: &str) {
        let mut map = self.allow_lists.write().unwrap_or_else(|e| e.into_inner());
        map.entry(cell_id.to_string()).or_default().insert(domain.to_string());
    }

    #[allow(clippy::collapsible_if)]
    pub fn check_outbound(&self, cell_id: &str, requested_domain: &str) -> Result<(), String> {
        let map = self.allow_lists.read().unwrap_or_else(|e| e.into_inner());
        if let Some(allowed) = map.get(cell_id) {
            if allowed.contains(requested_domain) {
                return Ok(());
            }
        }
        Err(format!("Network firewall blocked outbound request to {}", requested_domain))
    }
}
