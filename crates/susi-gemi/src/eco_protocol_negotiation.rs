//! Protocol-variant negotiation (VC-201-088 / T-CLAUDE-344).
//!
//! "Responses vs Chat", "A2A dialect", "MCP revision" — an endpoint often
//! speaks several variants of a protocol. Negotiation picks the best
//! *common* variant from knowledge-base facts (`implements`,
//! `implements-version`, `supersedes` ordering) and optional probe evidence,
//! never from a hardcoded table.

use crate::models::eco_schema::{Entity, KnowledgeBase, RelationKind};

/// Newest spec-version id first, following the `supersedes` chain and
/// `released` dates of every version of a standard/protocol.
#[must_use]
fn version_rank(kb: &KnowledgeBase, spec_version: &str) -> (String, String) {
    // (released, id) — lexicographic compare on released works for YYYY-MM-DD.
    for e in &kb.entities {
        if let Entity::SpecVersion(v) = e {
            if v.id == spec_version {
                return (v.released.clone().unwrap_or_default(), v.id.clone());
            }
        }
    }
    (String::new(), spec_version.to_string())
}

/// Spec-version ids a component pins via `implements-version`, newest first.
#[must_use]
pub fn implemented_versions(kb: &KnowledgeBase, component: &str) -> Vec<String> {
    let mut versions: Vec<String> = kb
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::ImplementsVersion && r.from == component)
        .map(|r| r.to.clone())
        .collect();
    versions.sort_by_key(|v| std::cmp::Reverse(version_rank(kb, v)));
    versions
}

/// Protocol/standard ids a component implements.
#[must_use]
pub fn implemented_protocols(kb: &KnowledgeBase, component: &str) -> Vec<String> {
    let mut protos: Vec<String> = kb
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::Implements && r.from == component)
        .map(|r| r.to.clone())
        .collect();
    protos.sort();
    protos
}

/// Pick the best protocol variant: `susi_supported` is ordered by susi's own
/// preference; the first one the endpoint implements wins.
#[must_use]
pub fn negotiate_protocol(
    kb: &KnowledgeBase,
    component: &str,
    susi_supported: &[&str],
) -> Option<String> {
    let have = implemented_protocols(kb, component);
    susi_supported
        .iter()
        .find(|p| have.iter().any(|h| h == *p))
        .map(|p| (*p).to_string())
}

/// Pick the spec version: the endpoint's newest `implements-version` that
/// susi also speaks. `client_versions` is the set susi accepts.
#[must_use]
pub fn negotiate_version(
    kb: &KnowledgeBase,
    component: &str,
    client_versions: &[&str],
) -> Option<String> {
    implemented_versions(kb, component)
        .into_iter()
        .find(|v| client_versions.iter().any(|c| c == v))
}

/// Full negotiation result for one endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Negotiation {
    pub component: String,
    /// Selected protocol id, if any.
    pub protocol: Option<String>,
    /// Selected spec-version id, if any.
    pub version: Option<String>,
    /// Probe evidence that backed the pick, when supplied.
    pub evidence: Vec<String>,
}

/// One call: protocol preference + version pinning in a single result.
#[must_use]
pub fn negotiate(
    kb: &KnowledgeBase,
    component: &str,
    susi_protocols: &[&str],
    client_versions: &[&str],
    probe_evidence: &[String],
) -> Negotiation {
    Negotiation {
        component: component.to_string(),
        protocol: negotiate_protocol(kb, component, susi_protocols),
        version: negotiate_version(kb, component, client_versions),
        evidence: probe_evidence.to_vec(),
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
    fn eco_protocol_negotiation_picks_supported_protocol() {
        let kb = kb();
        // vllm implements OpenAI chat; preferring responses still lands on chat
        let p = negotiate_protocol(
            &kb,
            "vllm",
            &["openai-responses", "openai-chat-completions"],
        );
        assert_eq!(p.as_deref(), Some("openai-chat-completions"));
    }

    #[test]
    fn eco_protocol_negotiation_refuses_unknown_variant() {
        let kb = kb();
        assert!(negotiate_protocol(&kb, "vllm", &["grpc-tgi"]).is_none());
        assert!(negotiate_protocol(&kb, "no-such-thing", &["mcp"]).is_none());
    }

    #[test]
    fn eco_protocol_negotiation_pins_newest_common_version() {
        let kb = kb();
        let v = negotiate_version(
            &kb,
            "vllm",
            &[
                "openai-chat-completions-2024-06",
                "openai-chat-completions-2019",
            ],
        );
        assert_eq!(v.as_deref(), Some("openai-chat-completions-2024-06"));
        // a version susi does not speak is not selected even when implemented
        let none = negotiate_version(&kb, "vllm", &["some-other-version"]);
        assert!(none.is_none());
    }

    #[test]
    fn eco_protocol_negotiation_full_result_carries_evidence() {
        let kb = kb();
        let n = negotiate(
            &kb,
            "llama-cpp-server",
            &["openai-chat-completions"],
            &["llamacpp-server-b"],
            &["probe:200 GET /v1/models".to_string()],
        );
        assert_eq!(n.protocol.as_deref(), Some("openai-chat-completions"));
        assert_eq!(n.version.as_deref(), Some("llamacpp-server-b"));
        assert_eq!(n.evidence.len(), 1);
    }
}
