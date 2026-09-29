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
    #[must_use]
    pub fn default_profile() -> Self {
        Self {
            versions: vec!["0.2".into(), "0.3".into()],
            dialects: vec!["json-rpc".into(), "http+json".into()],
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
        p.select("0.3", "json-rpc").unwrap();
        assert_eq!(p.selected_version.as_deref(), Some("0.3"));
        let v02 = load_fixture("a2a-0.2.json");
        assert!(v02["relations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "supersedes" || r["kind"] == "version-of"));
    }
}
