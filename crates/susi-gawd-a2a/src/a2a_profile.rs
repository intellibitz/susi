//! A2A versions and wire dialects (VC-201-087 / T-CLAUDE-277).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct A2aProfile {
    pub versions: Vec<String>,
    pub dialects: Vec<String>,
    pub selected_version: Option<String>,
    pub selected_dialect: Option<String>,
}

impl A2aProfile {
    /// What `serve` actually answers: the JSON-RPC binding (`/` and SSE
    /// `/stream`) at the protocol version the agent card advertises
    /// (`ra2a::PROTOCOL_VERSION`), including the 1.0 method names
    /// (`SendMessage`, `SubscribeToTask`, ...; `server::canonical_method`)
    /// beside the 0.3 slash names kept as legacy aliases. The HTTP+JSON
    /// (REST) binding is not mounted (see `server::serve`) and 0.2 is not
    /// spoken, so neither is claimed.
    #[must_use]
    pub fn default_profile() -> Self {
        Self {
            versions: vec![ra2a::PROTOCOL_VERSION.into()],
            dialects: vec!["json-rpc".into()],
            selected_version: None,
            selected_dialect: None,
        }
    }

    pub fn select(&mut self, version: &str, dialect: &str) -> Result<(), String> {
        if !self.versions.iter().any(|v| v == version) {
            return Err(format!("unsupported version {version}"));
        }
        if !self.dialects.iter().any(|d| d == dialect) {
            return Err(format!("unsupported dialect {dialect}"));
        }
        self.selected_version = Some(version.to_string());
        self.selected_dialect = Some(dialect.to_string());
        Ok(())
    }

    /// Verify structural and selection invariants on an [`A2aProfile`]:
    /// non-empty versions and dialects without blank entries, coupled selection
    /// (`selected_version.is_some() == selected_dialect.is_some()`), and membership
    /// of any selected version/dialect in the supported lists.
    pub fn verify_profile_invariants(&self) -> Result<(), String> {
        if self.versions.is_empty() || self.versions.iter().any(|v| v.trim().is_empty()) {
            return Err("A2aProfile versions must be non-empty without blank entries".to_string());
        }
        if self.dialects.is_empty() || self.dialects.iter().any(|d| d.trim().is_empty()) {
            return Err("A2aProfile dialects must be non-empty without blank entries".to_string());
        }
        if self.selected_version.is_some() != self.selected_dialect.is_some() {
            return Err(
                "A2aProfile selected_version and selected_dialect must both be set or both be None"
                    .to_string(),
            );
        }
        if let Some(ref v) = self.selected_version {
            if !self.versions.iter().any(|item| item == v) {
                return Err(format!(
                    "selected_version `{v}` is not in supported versions"
                ));
            }
        }
        if let Some(ref d) = self.selected_dialect {
            if !self.dialects.iter().any(|item| item == d) {
                return Err(format!(
                    "selected_dialect `{d}` is not in supported dialects"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod eco_a2a_versions_tests {
    use super::*;

    fn load_fixture(name: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/ecosystem")
            .join(name);
        let text =
            std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()));
        serde_json::from_str(&text).expect("json")
    }

    #[test]
    fn eco_a2a_versions_fixture_documents_0_2_and_0_3() {
        let proto = load_fixture("a2a.json");
        assert_eq!(proto["entity"]["id"], "a2a");
        let v03 = load_fixture("a2a-0.3.json");
        assert_eq!(v03["entity"]["version"], "0.3");
        let p = &v03["entity"]["provenance"];
        assert!(p["source"].as_str().unwrap().contains("a2a"));
        assert_eq!(p["confidence"], "verified");
    }

    #[test]
    fn eco_a2a_versions_client_selects_dialect() {
        let mut p = A2aProfile::default_profile();
        p.select(ra2a::PROTOCOL_VERSION, "json-rpc").unwrap();
        assert_eq!(p.selected_version.as_deref(), Some(ra2a::PROTOCOL_VERSION));
        // Only what is served may be selected.
        assert!(p.select("0.2", "json-rpc").is_err());
        assert!(p.select(ra2a::PROTOCOL_VERSION, "http+json").is_err());
        assert_eq!(p.selected_dialect.as_deref(), Some("json-rpc"));
        let v02 = load_fixture("a2a-0.2.json");
        assert!(v02["relations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "supersedes" || r["kind"] == "version-of"));
    }
}

#[cfg(test)]
mod honesty_tests {
    use super::A2aProfile;
    use crate::executor::GawdA2AExecutor;

    /// B6: the profile claims exactly what the served agent card advertises —
    /// one JSON-RPC interface at one protocol version, no REST dialect.
    #[test]
    fn profile_matches_the_served_agent_card() {
        let profile = A2aProfile::default_profile();
        let card = GawdA2AExecutor::new().agent_card();
        let mut card_versions: Vec<String> = card
            .supported_interfaces
            .iter()
            .map(|i| i.protocol_version.clone())
            .collect();
        card_versions.dedup();
        assert_eq!(profile.versions, card_versions);
        assert_eq!(profile.dialects, ["json-rpc"]);
        assert!(card
            .supported_interfaces
            .iter()
            .all(|i| i.protocol_binding.0.contains("JSONRPC")));
    }

    #[test]
    fn executor_skills_match_profile_capabilities_without_ui_claims() {
        let skills = GawdA2AExecutor::skills();
        assert!(skills.len() >= 8);
        for skill in &skills {
            assert!(
                !skill.description.trim().is_empty(),
                "empty skill {}",
                skill.id
            );
            let lower = skill.description.to_ascii_lowercase();
            for banned in ["dashboard", "egui", "tauri", "webview", "gui"] {
                assert!(
                    !lower.contains(banned),
                    "skill {} must not claim UI surface `{banned}`: {}",
                    skill.id,
                    skill.description
                );
            }
        }
    }
}

#[cfg(test)]
mod eco_a2a_security_tests {
    fn load(name: &str) -> serde_json::Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/ecosystem/profiles")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
    }

    #[test]
    fn eco_a2a_security_documents_schemes_and_extensions() {
        let p = load("a2a-security.json");
        assert!(p["auth"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["scheme"] == "bearer"));
        assert!(p["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "extensions"));
        assert!(p["response_shapes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "agent-card"));
        assert_eq!(p["provenance"]["confidence"], "verified");
    }

    #[test]
    fn a2a_profile_invariants_validate_selection_and_reject_unlisted_or_half_selected_state() {
        let mut p = super::A2aProfile::default_profile();
        assert!(p.verify_profile_invariants().is_ok());

        p.select(ra2a::PROTOCOL_VERSION, "json-rpc").unwrap();
        assert!(p.verify_profile_invariants().is_ok());

        let mut half = p.clone();
        half.selected_dialect = None;
        assert!(half.verify_profile_invariants().is_err());

        let mut unlisted = p.clone();
        unlisted.selected_version = Some("99.0".to_string());
        assert!(unlisted.verify_profile_invariants().is_err());

        let mut empty_versions = p;
        empty_versions.versions.clear();
        assert!(empty_versions.verify_profile_invariants().is_err());
    }
}
