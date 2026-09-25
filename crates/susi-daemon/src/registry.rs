//! Distributed Service Registry (Swarm OS Bullet 16)
//!
//! A unified key-value distributed registry that allows cells to publish
//! their network multiaddrs and available semantic services for peer discovery.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Represents a published service capability in the Swarm OS network.
#[derive(Debug, Clone)]
pub struct PublishedService {
    pub cell_id: String,
    pub multiaddr: String, // e.g., "/ip4/192.168.1.10/tcp/8080"
    pub services: HashSet<String>,
}

/// A registry managing cell network addresses and capabilities.
pub struct ServiceRegistry {
    /// Maps a cell ID to its published service definition.
    entries: RwLock<HashMap<String, PublishedService>>,
}

impl Default for ServiceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
        }
    }

    /// Publishes or updates a cell's available services.
    pub fn publish(&self, cell_id: &str, multiaddr: &str, services: Vec<String>) {
        let mut map = self.entries.write().unwrap_or_else(|e| e.into_inner());
        let mut service_set = HashSet::new();
        for svc in services {
            service_set.insert(svc);
        }

        map.insert(
            cell_id.to_string(),
            PublishedService {
                cell_id: cell_id.to_string(),
                multiaddr: multiaddr.to_string(),
                services: service_set,
            },
        );
    }

    /// Removes a cell from the registry (e.g. upon graceful exit).
    pub fn deregister(&self, cell_id: &str) {
        let mut map = self.entries.write().unwrap_or_else(|e| e.into_inner());
        map.remove(cell_id);
    }

    /// Queries the registry for all cells providing a specific service.
    /// Returns a list of cell IDs and their multiaddrs.
    pub fn discover_service(&self, service_name: &str) -> Vec<(String, String)> {
        let map = self.entries.read().unwrap_or_else(|e| e.into_inner());
        let mut results = Vec::new();

        for (cell_id, entry) in map.iter() {
            if entry.services.contains(service_name) {
                results.push((cell_id.clone(), entry.multiaddr.clone()));
            }
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_service_registry_discovery() {
        let registry = ServiceRegistry::new();

        registry.publish(
            "cell-1",
            "/ip4/10.0.0.1/tcp/5001",
            vec!["parser:pdf".to_string(), "infer:llama".to_string()],
        );
        registry.publish(
            "cell-2",
            "/ip4/10.0.0.2/tcp/5001",
            vec!["parser:docx".to_string()],
        );
        registry.publish(
            "cell-3",
            "/ip4/10.0.0.3/tcp/5001",
            vec!["infer:llama".to_string()],
        );

        let llama_providers = registry.discover_service("infer:llama");
        assert_eq!(llama_providers.len(), 2);

        let pdf_providers = registry.discover_service("parser:pdf");
        assert_eq!(pdf_providers.len(), 1);
        assert_eq!(pdf_providers[0].1, "/ip4/10.0.0.1/tcp/5001");

        let unknown = registry.discover_service("unknown:service");
        assert_eq!(unknown.len(), 0);
    }
}
