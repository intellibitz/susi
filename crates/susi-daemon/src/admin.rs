//! Administrative Diagnostics Endpoint (Swarm OS Bullet 100)
//!
//! Exposes a global diagnostics handler restricted to the `root` Swarm OS capability.

use crate::security::CapabilityPolicy;
use serde::Serialize;

/// Global diagnostics payload.
#[derive(Debug, Serialize)]
pub struct SwarmDiagnostics {
    pub active_cells: usize,
    pub loaded_plugins: usize,
    pub uptime_seconds: u64,
}

/// The admin server for processing root-level queries.
pub struct AdminServer {
    start_time: std::time::Instant,
}

impl Default for AdminServer {
    fn default() -> Self {
        Self::new()
    }
}

impl AdminServer {
    pub fn new() -> Self {
        Self {
            start_time: std::time::Instant::now(),
        }
    }

    /// Fetches global diagnostics, but strictly requires the caller to possess
    /// the `root` capability.
    pub fn get_diagnostics(&self, policy: &CapabilityPolicy, active_cells: usize, loaded_plugins: usize) -> Result<SwarmDiagnostics, String> {
        // Enforce Root Capability
        let mut has_root = false;
        for grant in policy.grants() {
            if grant.capability == "root" {
                has_root = true;
                break;
            }
        }

        if !has_root {
            return Err(format!("Access Denied: Caller '{}' lacks the 'root' capability required for global diagnostics", policy.cell_id()));
        }

        Ok(SwarmDiagnostics {
            active_cells,
            loaded_plugins,
            uptime_seconds: self.start_time.elapsed().as_secs(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::CapabilityGrant;

    #[test]
    fn test_admin_root_enforcement() {
        let admin = AdminServer::new();
        
        let standard_policy = CapabilityPolicy::new("cell-standard", vec![
            CapabilityGrant { capability: "infer".to_string(), scope: None, ephemeral: false }
        ]);
        
        // Should deny
        assert!(admin.get_diagnostics(&standard_policy, 10, 5).is_err());
        
        let root_policy = CapabilityPolicy::new("cell-admin", vec![
            CapabilityGrant { capability: "root".to_string(), scope: None, ephemeral: false }
        ]);
        
        // Should allow
        let diag = admin.get_diagnostics(&root_policy, 10, 5).unwrap();
        assert_eq!(diag.active_cells, 10);
        assert_eq!(diag.loaded_plugins, 5);
    }
}
