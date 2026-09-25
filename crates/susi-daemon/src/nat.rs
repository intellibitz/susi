//! NAT Traversal Abstraction (Swarm OS Bullet 38)
//!
//! Automatically handles NAT traversal (e.g., STUN/TURN abstractions) to allow
//! agents to communicate directly across corporate firewalls.

use std::sync::RwLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatStatus {
    Open,
    Symmetric,
    PortRestricted,
    Unknown,
}

/// A simulated NAT traversal manager for the Swarm OS.
pub struct NatManager {
    status: RwLock<NatStatus>,
    public_ip: RwLock<Option<String>>,
}

impl Default for NatManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NatManager {
    pub fn new() -> Self {
        Self {
            status: RwLock::new(NatStatus::Unknown),
            public_ip: RwLock::new(None),
        }
    }

    /// Performs a mock STUN request to discover the public IP and NAT type.
    pub fn perform_discovery(&self, mock_ip: &str, mock_status: NatStatus) {
        let mut status = self.status.write().unwrap_or_else(|e| e.into_inner());
        *status = mock_status;

        let mut ip = self.public_ip.write().unwrap_or_else(|e| e.into_inner());
        *ip = Some(mock_ip.to_string());
    }

    /// Generates a valid multiaddr for external peers to reach this daemon,
    /// factoring in the discovered NAT rules.
    pub fn generate_external_multiaddr(&self, local_port: u16) -> Result<String, String> {
        let status = self.status.read().unwrap_or_else(|e| e.into_inner());
        let ip = self.public_ip.read().unwrap_or_else(|e| e.into_inner());

        if *status == NatStatus::Symmetric {
            return Err(
                "Symmetric NAT detected. Direct P2P requires a TURN relay (not yet mocked)."
                    .to_string(),
            );
        }

        if let Some(pub_ip) = ip.as_ref() {
            Ok(format!("/ip4/{}/tcp/{}", pub_ip, local_port))
        } else {
            Err("Public IP not yet discovered. Call perform_discovery first.".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nat_discovery_and_routing() {
        let nat = NatManager::new();

        assert!(nat.generate_external_multiaddr(8080).is_err());

        nat.perform_discovery("203.0.113.5", NatStatus::Open);
        let addr = nat.generate_external_multiaddr(8080).unwrap();
        assert_eq!(addr, "/ip4/203.0.113.5/tcp/8080");

        nat.perform_discovery("203.0.113.6", NatStatus::Symmetric);
        assert!(nat.generate_external_multiaddr(8080).is_err()); // Symmetric blocks direct
    }
}
