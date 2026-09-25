//! Strict Sandbox Network Allow-lists (Swarm OS Bullet 22)
//!
//! Enforces strict outbound network filtering for sandboxes.

use std::collections::HashSet;
use std::sync::RwLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboundDecision {
    Allow,
    Deny,
    Proxy(String),
}

pub struct NetworkFirewall {
    /// Maps cell IDs to allowed outbound domains.
    allow_lists: RwLock<std::collections::HashMap<String, HashSet<String>>>,
    /// Maps cell IDs to domain -> proxy hop (Bullet 52).
    proxies: RwLock<std::collections::HashMap<String, std::collections::HashMap<String, String>>>,
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
            proxies: RwLock::new(std::collections::HashMap::new()),
        }
    }

    pub fn set_proxy(&self, cell_id: &str, domain: &str, hop: &str) {
        self.proxies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(cell_id.to_string())
            .or_default()
            .insert(domain.to_string(), hop.to_string());
    }

    /// Allow if the domain is on the cell's allow-list, proxy if a hop is
    /// configured, otherwise deny (Bullet 52).
    pub fn decide(&self, cell_id: &str, domain: &str) -> OutboundDecision {
        let allowed = self
            .allow_lists
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(cell_id)
            .is_some_and(|domains| domains.contains(domain));
        if allowed {
            return OutboundDecision::Allow;
        }
        if let Some(hop) = self
            .proxies
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(cell_id)
            .and_then(|domains| domains.get(domain))
        {
            return OutboundDecision::Proxy(hop.clone());
        }
        OutboundDecision::Deny
    }

    pub fn grant_access(&self, cell_id: &str, domain: &str) {
        let mut map = self.allow_lists.write().unwrap_or_else(|e| e.into_inner());
        map.entry(cell_id.to_string())
            .or_default()
            .insert(domain.to_string());
    }

    pub fn check_outbound(&self, cell_id: &str, requested_domain: &str) -> Result<(), String> {
        let map = self.allow_lists.read().unwrap_or_else(|e| e.into_inner());
        if let Some(allowed) = map.get(cell_id)
            && allowed.contains(requested_domain)
        {
            return Ok(());
        }
        Err(format!(
            "Network firewall blocked outbound request to {}",
            requested_domain
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_allow_proxy_or_deny() {
        let firewall = NetworkFirewall::new();
        firewall.grant_access("cell", "example.com");
        firewall.set_proxy("cell", "internal", "proxy.local");
        assert_eq!(
            firewall.decide("cell", "example.com"),
            OutboundDecision::Allow
        );
        assert_eq!(
            firewall.decide("cell", "internal"),
            OutboundDecision::Proxy("proxy.local".into())
        );
        assert_eq!(firewall.decide("cell", "elsewhere"), OutboundDecision::Deny);
    }
}
