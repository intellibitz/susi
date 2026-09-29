//! Unified capability-policy decision contract (VC-201-071).
//!
//! Maps MacPolicy and daemon CapabilityPolicy decisions into one authoritative
//! evaluation: conflicting rules deny consistently across tool, peer, and model.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPoint {
    Tool,
    Peer,
    Model,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayerVerdict {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerDecision {
    pub layer: String,
    pub verdict: LayerVerdict,
}

/// Authoritative contract: any Deny wins (deny-consistent).
#[must_use]
pub fn evaluate(entry: EntryPoint, layers: &[LayerDecision]) -> LayerVerdict {
    let _ = entry;
    if layers.iter().any(|l| l.verdict == LayerVerdict::Deny) {
        LayerVerdict::Deny
    } else if layers.is_empty() {
        LayerVerdict::Deny
    } else {
        LayerVerdict::Allow
    }
}
