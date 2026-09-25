//! Agent templates (Swarm OS Bullet 67)
//!
//! Manifests for the common roles. Each capability is inserted into the
//! bloom filter so routing can see it.

use susi_abi::swarm::{CapabilityBloom, SwarmCellManifest, SwarmRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentTemplate {
    Coder,
    Tester,
    Analyst,
    Ops,
}

pub fn capability_hash(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

pub fn scaffold(template: AgentTemplate, cell_id: &str) -> SwarmCellManifest {
    let (role, capabilities): (SwarmRole, &[&str]) = match template {
        AgentTemplate::Coder => (SwarmRole::ReflexCell, &["code:edit", "tool:git"]),
        AgentTemplate::Tester => (SwarmRole::ReflexCell, &["code:test"]),
        AgentTemplate::Analyst => (SwarmRole::PlannerCell, &["data:read"]),
        AgentTemplate::Ops => (SwarmRole::ToolDriver, &["incident:respond"]),
    };
    let mut bloom_filter = CapabilityBloom::empty();
    for capability in capabilities {
        bloom_filter.insert_hash(capability_hash(capability));
    }
    SwarmCellManifest {
        cell_id: cell_id.to_string(),
        role,
        capabilities: capabilities.iter().map(|cap| (*cap).to_string()).collect(),
        bloom_filter,
        endpoint: format!("ipc:///tmp/{cell_id}.sock"),
        trust_score: 0.5,
        last_heartbeat: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tester_manifest_declares_code_test_and_the_bloom_contains_it() {
        let manifest = scaffold(AgentTemplate::Tester, "tester-1");
        assert_eq!(manifest.capabilities, vec!["code:test".to_string()]);
        assert!(
            manifest
                .bloom_filter
                .may_contain_hash(capability_hash("code:test"))
        );
        assert!(
            !manifest
                .bloom_filter
                .may_contain_hash(capability_hash("code:edit"))
        );
    }
}
