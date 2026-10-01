//! Infer SUSI_AGENT from parent process / tool markers.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub agent: String,
}

/// Infer agent id from env override or process/command markers.
///
/// Order matters: the first marker that matches wins, so a wrapper that
/// mentions two tools reports the one listed first. `SUSI` means "no tool
/// marker found" — callers must treat it as unknown, not as an identity, since
/// every markerless process would otherwise share one token.
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
    } else if lower.contains("antigravity") {
        "ANTIGRAVITY"
    } else if lower.contains("gemini") {
        "GEMINI"
    } else if lower.contains("devin") {
        "DEVIN"
    } else if lower.contains("aider") {
        "AIDER"
    } else {
        "SUSI"
    };
    AgentIdentity {
        agent: agent.into(),
    }
}

/// The agent a process chain names, or `None` when nothing matches.
///
/// This is what keeps two workers from sharing a token: `susi.agent` is only
/// set in worktrees `workflow start` created, so everywhere else the fallback
/// is the clone-wide `user.name` — which resolved the primary checkout, a codex
/// worktree and a claude worktree all to `INTELLIBITZ`, one
/// `refs/claim-agents/INTELLIBITZ` and one `T-INTELLIBITZ-<n>` namespace.
#[must_use]
pub fn detect_agent_from_chain(chain: &str) -> Option<String> {
    let detected = detect_agent(None, chain);
    (detected.agent != "SUSI").then_some(detected.agent)
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

    #[test]
    fn zc_agent_identity_covers_every_swarm_tool() {
        for (cmd, expected) in [
            ("/opt/cursor-agent --force", "CURSOR"),
            ("claude --dangerously-skip-permissions", "CLAUDE"),
            ("/usr/local/bin/codex exec", "CODEX"),
            ("antigravity run", "ANTIGRAVITY"),
            ("gemini chat", "GEMINI"),
            ("devin-20260930-163229", "DEVIN"),
            ("aider --model gpt", "AIDER"),
        ] {
            assert_eq!(detect_agent(None, cmd).agent, expected, "{cmd}");
        }
    }

    #[test]
    fn zc_agent_identity_chain_returns_none_without_a_marker() {
        // The whole point: a markerless chain must not become an identity that
        // several workers then share.
        assert_eq!(detect_agent_from_chain("/bin/bash -c git status"), None);
        assert_eq!(detect_agent_from_chain(""), None);
        assert_eq!(
            detect_agent_from_chain("bash -c sleep 1\n/usr/bin/cursor-agent"),
            Some("CURSOR".into())
        );
        // The environment override still beats the markers.
        assert_eq!(
            detect_agent(Some("deepseek"), "cursor-agent").agent,
            "DEEPSEEK"
        );
    }
}
