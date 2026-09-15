// SUSI Continuous Intent Manifold
// Replaces static enum classification with dynamic intent impact and risk profiling.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeOfImpact {
    Read,        // Zero-mutation query
    Write,       // Workspace file I/O
    Mutate,      // Substrate modification / admin
    SelfExtend,  // Reflex synthesis / code evolution
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

        let scope_of_impact = if lower.contains("identity") || lower.contains("status") || lower.contains("models") || lower.contains("version")
            || lower == "ls" || lower.starts_with("ls ") || lower == "dir" || lower.contains("list directory") || lower.contains("list files")
            || lower.contains("who am i") || lower.contains("whoami") {
            ScopeOfImpact::Read
        } else if lower.contains("write") || lower.contains("save") || lower.contains("edit") || lower.contains("file") {
            ScopeOfImpact::Write
        } else if lower.contains("admin") || lower.contains("sync") || lower.contains("audit") || lower.contains("release") || lower.contains("verify") {
            ScopeOfImpact::Mutate
        } else {
            ScopeOfImpact::SelfExtend
        };

        let risk_profile = if lower.contains("rm -rf") || lower.contains("drop") || lower.contains("delete /") {
            RiskProfile::Critical
        } else if lower.contains("exec") || lower.contains("command") || lower.contains("install") {
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
