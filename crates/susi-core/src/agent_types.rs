//! Pure agent data types and the [`GawdAgent`] trait.
//! Concrete agent implementations and persistent mission blackboards live in Susi Commercial Apps.

use crate::susi_error::EaiResult;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GawdAgentInfo {
    pub name: String,
    pub provider: String,
    pub url: String,
    pub rank: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoverableAsset {
    pub tier: String,
    pub name: String,
    pub provider: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub name: String,
    pub description: String,
    pub categories: Vec<String>,
    pub semantic_anchors: Vec<String>,
    pub base_rank: f32,
    #[serde(default)]
    pub is_core: bool,
}

/// Opaque mission blackboard token; stateful store lives in Susi Commercial Apps (`susi-agent-runtime`).
#[derive(Debug, Default)]
pub struct HighDensityContextStore;

pub type SwarmBlackboard = Arc<HighDensityContextStore>;
pub type MissionBlackboard = SwarmBlackboard;

/// Trait every swarm agent implements.
pub trait GawdAgent: Send + Sync {
    fn name(&self) -> String;
    fn rank(&self) -> f32;
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> EaiResult<String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_profile_and_info_round_trip_serde() {
        let profile = AgentProfile {
            name: "SafetyAgent".into(),
            description: "Validates policy".into(),
            categories: vec!["governance".into()],
            semantic_anchors: vec!["audit".into()],
            base_rank: 0.9,
            is_core: true,
        };
        let encoded = serde_json::to_string(&profile).unwrap();
        let decoded: AgentProfile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, profile);

        let info = GawdAgentInfo {
            name: "SafetyAgent".into(),
            provider: "local".into(),
            url: "http://127.0.0.1:9094".into(),
            rank: 0.9,
        };
        let info_json = serde_json::to_string(&info).unwrap();
        let decoded_info: GawdAgentInfo = serde_json::from_str(&info_json).unwrap();
        assert_eq!(decoded_info, info);
    }
}
