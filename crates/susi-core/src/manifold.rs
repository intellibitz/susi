// Classifies an intent string's scope of impact and risk level via keyword
// matching, to decide whether it needs swarm dispatch and verification.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeOfImpact {
    Read,       // Zero-mutation query
    Write,      // Workspace file I/O
    Mutate,     // Substrate modification / admin
    SelfExtend, // Reflex synthesis / code evolution
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskProfile {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntentManifold {
    pub raw_intent: String,
    pub scope_of_impact: ScopeOfImpact,
    pub risk_profile: RiskProfile,
    pub requires_swarm: bool,
    pub requires_verification: bool,
}

impl IntentManifold {
    pub fn analyze(intent: &str) -> Self {
        let lower = intent.to_lowercase();
        // CLI bare intents are often prefixed with a verb token (`run …`).
        // Strip it so "run Read Cargo.toml … Cite the file." stays Read —
        // otherwise the trailing "file" word forces Write and burns a swarm.
        let focused = lower
            .strip_prefix("run ")
            .or_else(|| lower.strip_prefix("automate "))
            .unwrap_or(lower.as_str());

        let scope_of_impact = if is_workspace_file_read(focused)
            || focused.trim() == "identity"
            || focused.trim() == "status"
            || focused.trim() == "susi status"
            || focused.trim() == "models"
            || focused.trim() == "list models"
            || focused.trim() == "version"
            || focused.trim() == "susi version"
            || focused.trim() == "dashboard"
            || focused.trim() == "bloat audit"
            || focused == "ls"
            || focused.starts_with("ls ")
            || focused == "dir"
            || focused == "list directory"
            || focused == "list files"
            || focused == "who am i"
            || focused == "whoami"
        {
            ScopeOfImpact::Read
        } else if focused.contains("write")
            || focused.contains("save")
            || focused.contains("edit")
            || focused.contains("file")
        {
            ScopeOfImpact::Write
        } else if focused.contains("admin")
            || focused.contains("sync")
            || focused.contains("audit")
            || focused.contains("release")
            || focused.contains("verify")
        {
            ScopeOfImpact::Mutate
        } else {
            ScopeOfImpact::SelfExtend
        };

        let risk_profile = if focused.contains("rm -rf")
            || focused.contains("drop")
            || focused.contains("delete /")
        {
            RiskProfile::Critical
        } else if focused.contains("exec")
            || focused.contains("command")
            || focused.contains("install")
        {
            RiskProfile::High
        } else if scope_of_impact == ScopeOfImpact::Mutate {
            RiskProfile::Medium
        } else {
            RiskProfile::Low
        };

        let requires_swarm = scope_of_impact != ScopeOfImpact::Read;
        let requires_verification = scope_of_impact != ScopeOfImpact::Read;

        Self {
            raw_intent: intent.to_string(),
            scope_of_impact,
            risk_profile,
            requires_swarm,
            requires_verification,
        }
    }
}

/// `read Cargo.toml` / `cat src/lib.rs` — zero-mutation path-shaped queries.
fn is_workspace_file_read(focused: &str) -> bool {
    let rest = focused
        .trim()
        .strip_prefix("read ")
        .or_else(|| focused.trim().strip_prefix("cat "));
    let Some(rest) = rest else {
        return false;
    };
    let token = rest.split_whitespace().next().unwrap_or("");
    !token.is_empty() && (token.contains('.') || token.contains('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_read_cargo_toml_is_read_not_write() {
        let m = IntentManifold::analyze(
            "run Read Cargo.toml in this workspace and reply with only the package name from [package]. Cite the file.",
        );
        assert_eq!(m.scope_of_impact, ScopeOfImpact::Read);
        assert!(!m.requires_swarm);
    }

    #[test]
    fn bare_read_path_is_read() {
        let m = IntentManifold::analyze("read src/main.rs");
        assert_eq!(m.scope_of_impact, ScopeOfImpact::Read);
    }

    #[test]
    fn write_file_still_write() {
        let m = IntentManifold::analyze("write a file named notes.md");
        assert_eq!(m.scope_of_impact, ScopeOfImpact::Write);
    }
}
