//! Canonical swarm-event wire contract. Dispatch state lives in the commercial runtime.
use serde::{Deserialize, Serialize};

/// v1 payload enum — the complete set of swarm event kinds for schema
/// version 1. Wire form is the versioned [`SwarmEvent`] envelope; deserialize
/// through [`SwarmEvent::decode`], never directly, so unknown future
/// versions degrade to a warning instead of a parse failure or panic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SwarmEventType {
    AgentStarted {
        agent_name: String,
    },
    AgentObservation {
        agent_name: String,
        observation: String,
    },
    SubTaskSpawned {
        parent_agent: String,
        sub_task: String,
    },
    EvidenceProduced {
        agent_name: String,
        summary: String,
    },
    AgentCompleted {
        agent_name: String,
        elapsed_ms: u64,
    },
}

/// Current wire schema version emitted by [`SwarmEvent::new`].
pub const SWARM_EVENT_SCHEMA_V1: u32 = 1;

/// Versioned event envelope for the swarm bus wire representation.
///
/// `schema_version` travels alongside the payload so external consumers
/// (extension packs, peers) can gate on a contract instead of guessing at
/// the payload shape. Unknown versions are skipped by [`SwarmEvent::decode`]
/// with a warning — never a panic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmEvent {
    pub schema_version: u32,
    #[serde(flatten)]
    pub kind: SwarmEventType,
}

impl SwarmEvent {
    /// Wrap a v1 payload in the current-version envelope.
    pub fn new(kind: SwarmEventType) -> Self {
        Self {
            schema_version: SWARM_EVENT_SCHEMA_V1,
            kind,
        }
    }

    /// Decode one event from its JSON wire form. Returns `Ok(None)` — after
    /// logging a warning — for envelopes whose `schema_version` this build
    /// does not understand; `Err` only for malformed JSON or a versioned
    /// payload that fails to parse.
    pub fn decode(json: &str) -> Result<Option<Self>, serde_json::Error> {
        let envelope: serde_json::Value = serde_json::from_str(json)?;
        let version = envelope
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        if version != u64::from(SWARM_EVENT_SCHEMA_V1) {
            eprintln!("susi-core: swarm bus: dropping event with unknown schema_version={version}");
            return Ok(None);
        }
        serde_json::from_value(envelope).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swarm_event_envelope_carries_schema_version() {
        let event = SwarmEvent::new(SwarmEventType::AgentStarted {
            agent_name: "a".into(),
        });
        assert_eq!(event.schema_version, SWARM_EVENT_SCHEMA_V1);
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"schema_version\":1"));
        let decoded = SwarmEvent::decode(&json).unwrap().unwrap();
        assert!(matches!(
            decoded.kind,
            SwarmEventType::AgentStarted { ref agent_name } if agent_name == "a"
        ));
    }

    #[test]
    fn swarm_event_decode_drops_unknown_version_gracefully() {
        let json = r#"{"schema_version":99,"AgentStarted":{"agent_name":"a"}}"#;
        assert!(SwarmEvent::decode(json).unwrap().is_none());
        // Missing version is also unknown — degrade, don't panic.
        let json = r#"{"AgentStarted":{"agent_name":"a"}}"#;
        assert!(SwarmEvent::decode(json).unwrap().is_none());
    }

    #[test]
    fn swarm_event_decode_errors_on_malformed_json() {
        assert!(SwarmEvent::decode("not json").is_err());
    }
}
