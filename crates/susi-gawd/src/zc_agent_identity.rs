//! Infer SUSI_AGENT from parent process / tool markers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent: String,
}

/// Infer agent id from env override or process/command markers.
#[must_use]
pub fn detect_agent(env_agent: Option<&str>, parent_cmd: &str) -> AgentIdentity {
    if let Some(a) = env_agent.filter(|s| !s.trim().is_empty()) {
        return AgentIdentity {
            agent: a.trim().to_ascii_uppercase(),
        };
    }
    let lower = parent_cmd.to_ascii_lowercase();
    let agent = if lower.contains("cursor") {
        "CURSOR"
    } else if lower.contains("claude") || lower.contains("anthropic") {
        "CLAUDE"
    } else if lower.contains("codex") {
        "CODEX"
    } else if lower.contains("gemini") {
        "GEMINI"
    } else if lower.contains("devin") {
        "DEVIN"
    } else {
        "SUSI"
    };
    AgentIdentity {
        agent: agent.into(),
    }
}

#[cfg(test)]
mod zc_agent_identity_tests {
    use super::*;

    #[test]
    fn zc_agent_identity_from_tool_markers() {
        assert_eq!(detect_agent(None, "/usr/bin/cursor-agent").agent, "CURSOR");
        assert_eq!(detect_agent(Some("claude"), "bash").agent, "CLAUDE");
        assert_eq!(detect_agent(None, "unknown").agent, "SUSI");
    }
}
