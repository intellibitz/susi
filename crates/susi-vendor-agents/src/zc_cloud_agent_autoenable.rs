//! Auto-enable cloud agents when keys are present.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudAgentToggle {
    pub name: String,
    pub enabled: bool,
}

#[must_use]
pub fn autoenable_cloud_agents(keys: &[&str]) -> Vec<CloudAgentToggle> {
    let mut out = Vec::new();
    if keys.contains(&"openai") {
        out.push(CloudAgentToggle {
            name: "openai-agent".into(),
            enabled: true,
        });
    }
    if keys.contains(&"anthropic") {
        out.push(CloudAgentToggle {
            name: "anthropic-agent".into(),
            enabled: true,
        });
    }
    out
}

#[cfg(test)]
mod zc_cloud_agent_autoenable_tests {
    use super::*;

    #[test]
    fn zc_cloud_agent_autoenable_from_keys() {
        let t = autoenable_cloud_agents(&["openai"]);
        assert_eq!(t.len(), 1);
        assert!(t[0].enabled);
    }
}
