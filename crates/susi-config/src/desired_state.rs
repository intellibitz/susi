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
