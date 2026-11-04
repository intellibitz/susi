//! Versioned desired-state configuration contract (VC-201-061).
//!
//! Describes models, runtimes, providers, peers, tools, and policy with
//! explicit schema versions and validated cross-references. Unknown keys
//! are preserved on round-trip; invalid relationships fail before mutation.

use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Current desired-state schema version this binary writes.
pub const DESIRED_STATE_SCHEMA: &str = "susi.desired_state/v1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesiredState {
    pub schema_version: String,
    #[serde(default)]
    pub models: Vec<DesiredRef>,
    #[serde(default)]
    pub runtimes: Vec<DesiredRef>,
    #[serde(default)]
    pub providers: Vec<DesiredRef>,
    #[serde(default)]
    pub peers: Vec<DesiredRef>,
    #[serde(default)]
    pub tools: Vec<DesiredRef>,
    #[serde(default)]
    pub policy: BTreeMap<String, Value>,
    /// Unknown top-level keys preserved across round-trips.
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DesiredRef {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    /// Optional dependency ids that must exist in the same document.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, Value>,
}

impl DesiredState {
    #[must_use]
    pub fn empty_v1() -> Self {
        Self {
            schema_version: DESIRED_STATE_SCHEMA.to_string(),
            models: Vec::new(),
            runtimes: Vec::new(),
            providers: Vec::new(),
            peers: Vec::new(),
            tools: Vec::new(),
            policy: BTreeMap::new(),
            extra: BTreeMap::new(),
        }
    }

    /// All declared ids across entity collections.
    #[must_use]
    pub fn all_ids(&self) -> BTreeSet<String> {
        let mut ids = BTreeSet::new();
        for list in [
            &self.models,
            &self.runtimes,
            &self.providers,
            &self.peers,
            &self.tools,
        ] {
            for r in list {
                if !r.id.is_empty() {
                    ids.insert(r.id.clone());
                }
            }
        }
        ids
    }

    fn entity_lists(&self) -> [&Vec<DesiredRef>; 5] {
        [
            &self.models,
            &self.runtimes,
            &self.providers,
            &self.peers,
            &self.tools,
        ]
    }
}

/// Validate references and schema version. Does not mutate.
pub fn validate_desired_state(state: &DesiredState) -> EaiResult<()> {
    if state.schema_version != DESIRED_STATE_SCHEMA {
        return Err(EaiError::config(format!(
            "unsupported desired-state schema `{}` (want {DESIRED_STATE_SCHEMA})",
            state.schema_version
        )));
    }
    let ids = state.all_ids();
    let mut seen = BTreeSet::new();
    for list in state.entity_lists() {
        for r in list {
            if r.id.is_empty() {
                return Err(EaiError::config(
                    "desired-state entity id must be non-empty",
                ));
            }
            if !seen.insert(r.id.clone()) {
                return Err(EaiError::config(format!(
                    "duplicate desired-state id `{}`",
                    r.id
                )));
            }
            for dep in &r.requires {
                if !ids.contains(dep) {
                    return Err(EaiError::config(format!(
                        "entity `{}` requires unknown id `{dep}`",
                        r.id
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Parse + validate. Fails before any caller mutates host state.
pub fn parse_desired_state(json: &str) -> EaiResult<DesiredState> {
    let state: DesiredState =
        serde_json::from_str(json).map_err(|e| EaiError::config(e.to_string()))?;
    validate_desired_state(&state)?;
    Ok(state)
}

/// Round-trip JSON while preserving unknown keys in `extra` / per-entity `extra`.
pub fn round_trip_desired_state(state: &DesiredState) -> EaiResult<DesiredState> {
    let json = serde_json::to_string(state).map_err(|e| EaiError::config(e.to_string()))?;
    parse_desired_state(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entity(id: &str, kind: &str, requires: &[&str]) -> DesiredRef {
        DesiredRef {
            id: id.into(),
            kind: kind.into(),
            requires: requires.iter().map(|s| (*s).to_string()).collect(),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_v1_and_populated_cross_collection_graph_validate_cleanly() {
        let mut state = DesiredState::empty_v1();
        assert_eq!(state.schema_version, DESIRED_STATE_SCHEMA);
        assert!(state.all_ids().is_empty());
        assert!(validate_desired_state(&state).is_ok());

        state.runtimes.push(entity("rt:local", "onnx", &[]));
        state.providers.push(entity("prov:gemini", "cloud", &[]));
        state.models.push(entity(
            "model:embed",
            "embedding",
            &["rt:local", "prov:gemini"],
        ));
        state
            .tools
            .push(entity("tool:search", "mcp", &["model:embed"]));
        state
            .peers
            .push(entity("peer:worker", "agent", &["tool:search"]));

        assert_eq!(state.all_ids().len(), 5);
        assert!(validate_desired_state(&state).is_ok());
    }

    #[test]
    fn validate_desired_state_rejects_bad_schema_empty_id_duplicates_and_unknown_deps() {
        let mut bad_schema = DesiredState::empty_v1();
        bad_schema.schema_version = "susi.desired_state/v999".into();
        let err = validate_desired_state(&bad_schema).unwrap_err().to_string();
        assert!(err.contains("unsupported desired-state schema"));

        let mut empty_id = DesiredState::empty_v1();
        empty_id.models.push(entity("", "llm", &[]));
        let err = validate_desired_state(&empty_id).unwrap_err().to_string();
        assert!(err.contains("desired-state entity id must be non-empty"));

        let mut dup_id = DesiredState::empty_v1();
        dup_id.models.push(entity("shared:id", "llm", &[]));
        dup_id.tools.push(entity("shared:id", "mcp", &[]));
        let err = validate_desired_state(&dup_id).unwrap_err().to_string();
        assert!(err.contains("duplicate desired-state id `shared:id`"));

        let mut unknown_dep = DesiredState::empty_v1();
        unknown_dep
            .peers
            .push(entity("peer:a", "agent", &["missing:dep"]));
        let err = validate_desired_state(&unknown_dep)
            .unwrap_err()
            .to_string();
        assert!(err.contains("entity `peer:a` requires unknown id `missing:dep`"));
    }

    #[test]
    fn parse_and_round_trip_desired_state_preserve_top_level_and_entity_extra_fields() {
        let raw = r#"{
            "schema_version": "susi.desired_state/v1",
            "models": [
                {
                    "id": "model:primary",
                    "kind": "chat",
                    "requires": ["prov:openai"],
                    "custom_weight": 42
                }
            ],
            "providers": [
                {
                    "id": "prov:openai",
                    "kind": "cloud",
                    "region_hint": "us-east"
                }
            ],
            "policy": {
                "max_tokens": 4096
            },
            "future_top_level_flag": true
        }"#;

        let parsed = parse_desired_state(raw).expect("valid desired state parses");
        assert_eq!(
            parsed.extra.get("future_top_level_flag"),
            Some(&Value::Bool(true))
        );
        assert_eq!(parsed.policy.get("max_tokens"), Some(&Value::from(4096)));
        assert_eq!(
            parsed.models[0].extra.get("custom_weight"),
            Some(&Value::from(42))
        );
        assert_eq!(
            parsed.providers[0].extra.get("region_hint"),
            Some(&Value::String("us-east".into()))
        );

        let round_tripped = round_trip_desired_state(&parsed).expect("round-trip succeeds");
        assert_eq!(round_tripped, parsed);
    }
}
