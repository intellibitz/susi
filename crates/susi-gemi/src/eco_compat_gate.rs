//! Compatibility gate before enabling anything (VC-201-088 / T-CLAUDE-346).
//!
//! Before a provider, agent or MCP server is enabled, its declared protocol
//! and version are checked against what susi speaks — from the knowledge
//! base, not from hope. The verdict carries the reason so the refusal or
//! warning is explainable.

use crate::models::eco_schema::{Entity, KnowledgeBase, RelationKind};

/// Gate outcome for one candidate component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The component implements a protocol susi speaks; versions pinned are
    /// all current.
    Allow,
    /// Compatible, but with a caveat the operator should see (e.g. the pinned
    /// version is superseded).
    Warn(String),
    /// Not compatible; the reason names what is missing or unknown.
    Refuse(String),
}

/// Is `spec_version` superseded by a newer recorded version?
fn superseded(kb: &KnowledgeBase, spec_version: &str) -> Option<String> {
    kb.relations
        .iter()
        .find(|r| r.kind == RelationKind::Supersedes && r.to == spec_version)
        .map(|r| r.from.clone())
}

/// Gate one component against the protocols susi speaks.
#[must_use]
pub fn check(kb: &KnowledgeBase, component: &str, susi_protocols: &[&str]) -> Verdict {
    if kb.entity(component).is_none() {
        return Verdict::Refuse(format!("{component} is not in the knowledge base"));
    }
    let implemented: Vec<String> = kb
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Implements && r.from == component)
        .map(|r| r.to.clone())
        .collect();
    if !implemented
        .iter()
        .any(|p| susi_protocols.iter().any(|s| s == p))
    {
        return Verdict::Refuse(format!(
            "{component} implements none of susi's protocols ({})",
            susi_protocols.join(", ")
        ));
    }
    let stale_pin = kb
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::ImplementsVersion && r.from == component)
        .filter_map(|r| superseded(kb, &r.to))
        .next();
    if let Some(newer) = stale_pin {
        return Verdict::Warn(format!(
            "{component} pins a superseded spec version; {newer} is current"
        ));
    }
    Verdict::Allow
}

/// Gate one spec-version directly (enabling a pinned revision).
#[must_use]
pub fn check_version(kb: &KnowledgeBase, spec_version: &str) -> Verdict {
    let known = kb
        .entities
        .iter()
        .any(|e| matches!(e, Entity::SpecVersion(v) if v.id == spec_version));
    if !known {
        return Verdict::Refuse(format!("{spec_version} is not a recorded spec version"));
    }
    match superseded(kb, spec_version) {
        Some(newer) => Verdict::Warn(format!("{spec_version} is superseded by {newer}")),
        None => Verdict::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::eco_store;

    fn kb() -> KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_compat_gate_allows_compatible_component() {
        let kb = kb();
        assert_eq!(
            check(&kb, "vllm", &["openai-chat-completions"]),
            Verdict::Allow
        );
    }

    #[test]
    fn eco_compat_gate_refuses_without_common_protocol() {
        let kb = kb();
        let v = check(&kb, "vllm", &["grpc-tgi", "mqtt"]);
        match v {
            Verdict::Refuse(reason) => assert!(reason.contains("none of susi's protocols")),
            other => panic!("expected refuse, got {other:?}"),
        }
    }

    #[test]
    fn eco_compat_gate_refuses_unknown_component() {
        let kb = kb();
        let v = check(&kb, "never-heard-of-it", &["mcp"]);
        match v {
            Verdict::Refuse(reason) => assert!(reason.contains("not in the knowledge base")),
            other => panic!("expected refuse, got {other:?}"),
        }
    }

    #[test]
    fn eco_compat_gate_warns_on_superseded_pin() {
        let kb = kb();
        // a component pinning a2a-0.2 (superseded by a2a-0.3) warns.
        // If none exists in data, construct the check at version level.
        let v = check_version(&kb, "a2a-0.2");
        match v {
            Verdict::Warn(reason) => assert!(reason.contains("a2a-0.3")),
            other => panic!("expected warn, got {other:?}"),
        }
        assert_eq!(check_version(&kb, "a2a-0.3"), Verdict::Allow);
    }

    #[test]
    fn eco_compat_gate_refuses_unrecorded_version() {
        let kb = kb();
        match check_version(&kb, "v9.9.9") {
            Verdict::Refuse(_) => {}
            other => panic!("expected refuse, got {other:?}"),
        }
    }
}
