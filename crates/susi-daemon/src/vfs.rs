//! Virtual File System (VFS) Abstraction (Swarm OS Bullets 4 & 29)
//!
//! Provides a pseudo-filesystem where swarm resources (models, tools, peers)
//! are mapped to `/dev/*` nodes. Access is strictly capability-gated.

use crate::security::CapabilityPolicy;

/// Represents an opened handle to a pseudo-device.
pub struct VfsHandle {
    pub path: String,
    pub cell_id: String,
}

/// A node in the Swarm OS Virtual File System.
pub enum VfsNode {
    /// LLM inference endpoint (requires `infer` capability).
    Llm,
    /// Peer routing table (requires `net:peers` capability).
    Peers,
    /// Host tools proxy (requires `tool:*` capability).
    Tools,
    /// Blackboard shared memory (requires `blackboard:read`).
    Blackboard,
}

impl VfsNode {
    /// Resolves a path to a VFS node.
    pub fn resolve(path: &str) -> Option<Self> {
        match path {
            "/dev/llm" => Some(Self::Llm),
            "/dev/peers" => Some(Self::Peers),
            "/dev/tools" => Some(Self::Tools),
            "/dev/blackboard" => Some(Self::Blackboard),
            _ => None,
        }
    }

    /// Returns the capability required to access this node.
    pub fn required_capability(&self) -> &'static str {
        match self {
            Self::Llm => "infer",
            Self::Peers => "net:peers",
            Self::Tools => "tool:*",
            Self::Blackboard => "blackboard:read",
        }
    }
}

/// The VFS Manager maps pseudo-files to resources and enforces capabilities.
pub struct VfsManager;

impl Default for VfsManager {
    fn default() -> Self {
        Self::new()
    }
}

impl VfsManager {
    pub fn new() -> Self {
        Self
    }

    /// Attempts to open a pseudo-device on behalf of a cell.
    pub fn open(&self, cell_id: &str, path: &str, policy: &CapabilityPolicy) -> Result<VfsHandle, String> {
        let node = VfsNode::resolve(path).ok_or_else(|| format!("VFS node not found: {}", path))?;
        
        // Mock a syscall request to check capabilities
        let required = node.required_capability();
        
        let mut authorized = false;
        for grant in policy.grants() {
            if grant.capability == required || (required.ends_with('*') && grant.capability.starts_with(&required[..required.len()-1])) {
                authorized = true;
                break;
            }
        }

        if authorized {
            Ok(VfsHandle {
                path: path.to_string(),
                cell_id: cell_id.to_string(),
            })
        } else {
            Err(format!("Access Denied: Cell {} lacks capability '{}' to access {}", cell_id, required, path))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::CapabilityGrant;

    #[test]
    fn test_vfs_capability_enforcement() {
        let vfs = VfsManager::new();
        
        // Deny-by-default policy
        let empty_policy = CapabilityPolicy::new("cell-1", vec![]);
        assert!(vfs.open("cell-1", "/dev/llm", &empty_policy).is_err());
        assert!(vfs.open("cell-1", "/dev/unknown", &empty_policy).is_err());

        // Granted policy
        let granted_policy = CapabilityPolicy::new("cell-2", vec![
            CapabilityGrant {
                capability: "infer".to_string(),
                scope: None,
                ephemeral: false,
            }
        ]);
        
        assert!(vfs.open("cell-2", "/dev/llm", &granted_policy).is_ok());
        assert!(vfs.open("cell-2", "/dev/peers", &granted_policy).is_err()); // Not granted
    }
}
