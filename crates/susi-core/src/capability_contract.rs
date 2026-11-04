//! Unified capability-policy decision contract (VC-201-071).
//!
//! Maps MacPolicy and daemon CapabilityPolicy decisions into one authoritative
//! evaluation: conflicting rules deny consistently across tool, peer, and model.

use crate::mac_policy::{CapabilityToken, MacPolicy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryPoint {
    Tool,
    Peer,
    Model,
}

impl EntryPoint {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Peer => "peer",
            Self::Model => "model",
        }
    }
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

impl LayerDecision {
    /// Derive a layer decision directly from `MacPolicy`.
    #[must_use]
    pub fn from_mac(policy: &MacPolicy, subject: &str, action: &str, resource: &str) -> Self {
        let verdict = if policy.is_permitted(subject, action, resource) {
            LayerVerdict::Allow
        } else {
            LayerVerdict::Deny
        };
        Self {
            layer: "mac".to_string(),
            verdict,
        }
    }

    /// Derive a layer decision by verifying a `CapabilityToken`.
    #[must_use]
    pub fn from_token(policy: &MacPolicy, token: &CapabilityToken) -> Self {
        let verdict = if policy.verify(token) {
            LayerVerdict::Allow
        } else {
            LayerVerdict::Deny
        };
        Self {
            layer: "capability".to_string(),
            verdict,
        }
    }

    /// Derive a layer decision from an external or daemon capability evaluation.
    #[must_use]
    pub fn from_daemon_capability(allowed: bool) -> Self {
        Self {
            layer: "daemon_capability".to_string(),
            verdict: if allowed {
                LayerVerdict::Allow
            } else {
                LayerVerdict::Deny
            },
        }
    }

    /// Entry-point specific decision layer.
    #[must_use]
    pub fn for_entry_point(entry: EntryPoint, verdict: LayerVerdict) -> Self {
        Self {
            layer: format!("entry:{}", entry.as_str()),
            verdict,
        }
    }
}

/// Known recognized authoritative policy layers.
const RECOGNIZED_LAYERS: &[&str] = &["mac", "capability", "daemon_capability", "cap"];

/// Authoritative contract:
/// - Layers must not be empty.
/// - The mandatory access control ("mac") layer MUST participate. If missing, evaluation denies.
/// - Unrecognized layers cannot authorize and cause rejection.
/// - Any Deny on applicable layers wins (deny-consistent across all layers).
/// - Entry-point specific layers (e.g. `entry:<entry_point>`) are enforced for their target.
#[must_use]
pub fn evaluate(entry: EntryPoint, layers: &[LayerDecision]) -> LayerVerdict {
    if layers.is_empty() {
        return LayerVerdict::Deny;
    }

    // Every layer must be a recognized policy layer or an entry-point layer
    let has_unrecognized = layers.iter().any(|l| {
        !RECOGNIZED_LAYERS.contains(&l.layer.as_str())
            && !l.layer.starts_with("entry:")
            && l.layer != "tool"
            && l.layer != "peer"
            && l.layer != "model"
    });
    if has_unrecognized {
        return LayerVerdict::Deny;
    }

    // The mandatory MAC layer must participate and be present.
    let has_mac = layers.iter().any(|l| l.layer == "mac");
    if !has_mac {
        return LayerVerdict::Deny;
    }

    let entry_tag = format!("entry:{}", entry.as_str());

    for l in layers {
        if l.layer.starts_with("entry:")
            || l.layer == "tool"
            || l.layer == "peer"
            || l.layer == "model"
        {
            // Entry-point specific layer: applies only if it matches current entry point
            if (l.layer == entry_tag || l.layer == entry.as_str())
                && l.verdict == LayerVerdict::Deny
            {
                return LayerVerdict::Deny;
            }
        } else if l.verdict == LayerVerdict::Deny {
            // General authoritative layer (mac, capability, daemon_capability): Deny wins
            return LayerVerdict::Deny;
        }
    }

    LayerVerdict::Allow
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn evaluate_denies_empty_layers_or_missing_mac_layer() {
        assert_eq!(evaluate(EntryPoint::Tool, &[]), LayerVerdict::Deny);
        let without_mac = [LayerDecision::from_daemon_capability(true)];
        assert_eq!(evaluate(EntryPoint::Tool, &without_mac), LayerVerdict::Deny);
    }

    #[test]
    fn evaluate_denies_unrecognized_layer_names() {
        let layers = [
            LayerDecision {
                layer: "mac".into(),
                verdict: LayerVerdict::Allow,
            },
            LayerDecision {
                layer: "custom_untrusted".into(),
                verdict: LayerVerdict::Allow,
            },
        ];
        assert_eq!(evaluate(EntryPoint::Peer, &layers), LayerVerdict::Deny);
    }

    #[test]
    fn evaluate_enforces_entry_point_specific_deny_only_on_matching_target() {
        let layers = [
            LayerDecision {
                layer: "mac".into(),
                verdict: LayerVerdict::Allow,
            },
            LayerDecision::for_entry_point(EntryPoint::Peer, LayerVerdict::Deny),
        ];
        assert_eq!(evaluate(EntryPoint::Peer, &layers), LayerVerdict::Deny);
        assert_eq!(evaluate(EntryPoint::Tool, &layers), LayerVerdict::Allow);
        assert_eq!(evaluate(EntryPoint::Model, &layers), LayerVerdict::Allow);
    }

    #[test]
    fn layer_decision_from_mac_and_token_reflect_policy_verification() {
        let policy = MacPolicy::new(
            [0x11u8; 32],
            crate::mac_policy::PrivacyMode::Balanced,
            false,
        );
        let token = policy
            .grant("agent-1", "invoke", "tool:search", Some(60))
            .unwrap();
        let mac_ok = LayerDecision::from_mac(&policy, "agent-1", "invoke", "tool:search");
        let mac_denied = LayerDecision::from_mac(&policy, "agent-1", "invoke", "tool:delete");
        assert_eq!(mac_ok.verdict, LayerVerdict::Allow);
        assert_eq!(mac_denied.verdict, LayerVerdict::Deny);

        let tok_ok = LayerDecision::from_token(&policy, &token);
        assert_eq!(tok_ok.verdict, LayerVerdict::Allow);
        assert_eq!(
            evaluate(EntryPoint::Tool, &[mac_ok, tok_ok]),
            LayerVerdict::Allow
        );
    }
}
