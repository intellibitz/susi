// SUSI Asynchronous Swarm Event Bus
// Real-time inter-agent messaging and telemetry streaming via flume.
// Each typed subscription receives its own copy of subsequent events.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::any::{Any, TypeId};
use std::sync::Arc;

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
            tracing::warn!(
                schema_version = version,
                "swarm bus: dropping event with unknown schema_version"
            );
            return Ok(None);
        }
        serde_json::from_value(envelope).map(Some)
    }
}

/// Process-wide typed bus for substrate-internal telemetry (schedule
/// decisions, dispatch traces). Distinct from the per-mission channels from
/// [`create_swarm_bus`]: subscribers here observe events across the whole
/// process, not one mission.
pub fn global_bus() -> &'static TypedEventBus {
    static BUS: std::sync::OnceLock<TypedEventBus> = std::sync::OnceLock::new();
    BUS.get_or_init(TypedEventBus::new)
}

pub type LegacySwarmEventBus = (
    flume::Sender<SwarmEventType>,
    flume::Receiver<SwarmEventType>,
);

pub fn create_swarm_bus() -> LegacySwarmEventBus {
    flume::unbounded()
}

type Subscribers<E> = parking_lot::Mutex<Vec<flume::Sender<E>>>;

/// Broadcasts events to independent subscribers of each event type.
/// Events published before subscription are not retained. Cloning a returned
/// receiver shares that subscription's queue rather than creating a new one.
#[derive(Default, Clone)]
pub struct TypedEventBus {
    channels: Arc<DashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl TypedEventBus {
    pub fn new() -> Self {
        Self::default()
    }

    #[allow(clippy::expect_used)]
    pub fn publish<E: Send + Sync + Clone + 'static>(&self, event: E) {
        let subscribers = self
            .channels
            .get(&TypeId::of::<E>())
            .map(|entry| Arc::clone(entry.value()));
        if let Some(subscribers) = subscribers {
            // Mandate 42: safe - the map is keyed by TypeId::of::<E>(), and
            // subscribe() (below) only ever inserts an Arc<Subscribers<E>>
            // under that same key for that same E, so a lookup for E's key
            // can never yield a value of any other concrete type.
            let subscribers = subscribers
                .downcast::<Subscribers<E>>()
                .expect("event type must match its subscriber list");
            subscribers
                .lock()
                .retain(|sender| sender.send(event.clone()).is_ok());
        }
    }

    #[allow(clippy::expect_used)]
    pub fn subscribe<E: Send + Sync + Clone + 'static>(&self) -> flume::Receiver<E> {
        let subscribers = {
            let entry = self
                .channels
                .entry(TypeId::of::<E>())
                .or_insert_with(|| Arc::new(Subscribers::<E>::new(Vec::new())));
            Arc::clone(entry.value())
        };
        // Mandate 42: safe - same invariant as publish() above: this entry
        // was either just inserted as Arc<Subscribers<E>> by the
        // or_insert_with closure right above, or already held that same
        // concrete type from a prior subscribe::<E>() call under this key.
        let subscribers = subscribers
            .downcast::<Subscribers<E>>()
            .expect("event type must match its subscriber list");
        let (sender, receiver) = flume::unbounded();
        let mut subscribers = subscribers.lock();
        subscribers.retain(|sender| !sender.is_disconnected());
        subscribers.push(sender);
        receiver
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcasts_to_every_subscription_across_bus_clones() {
        let bus = TypedEventBus::new();
        let first = bus.subscribe::<u32>();
        let second = bus.clone().subscribe::<u32>();
        bus.publish(42_u32);
        assert_eq!(first.try_recv(), Ok(42));
        assert_eq!(second.try_recv(), Ok(42));
    }

    #[test]
    fn isolates_types_and_does_not_replay_old_events() {
        let bus = TypedEventBus::new();
        bus.publish(1_u32);
        let numbers = bus.subscribe::<u32>();
        let strings = bus.subscribe::<String>();
        assert!(numbers.try_recv().is_err());
        bus.publish(2_u32);
        assert_eq!(numbers.try_recv(), Ok(2));
        assert!(strings.try_recv().is_err());
        drop(numbers);
        bus.publish(3_u32);
        let replacement = bus.subscribe::<u32>();
        assert!(replacement.try_recv().is_err());
        bus.publish(4_u32);
        assert_eq!(replacement.try_recv(), Ok(4));
    }

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
