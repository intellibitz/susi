// SUSI Asynchronous Swarm Event Bus
// Real-time inter-agent messaging and telemetry streaming via flume.
// Refactored to feature an innovative generically-typed dynamic Pub/Sub event bus.

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::any::{Any, TypeId};
use std::sync::Arc;

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

pub type LegacySwarmEventBus = (
    flume::Sender<SwarmEventType>,
    flume::Receiver<SwarmEventType>,
);

pub fn create_swarm_bus() -> LegacySwarmEventBus {
    flume::unbounded()
}

/// A highly innovative, dynamically typed Pub/Sub event bus leveraging Rust generics.
/// Automatically allocates and multiplexes independent flume channels per Event type.
#[derive(Default, Clone)]
pub struct TypedEventBus {
    channels: Arc<DashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
}

impl TypedEventBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Dispatches an event of generic type E.
    pub fn publish<E: Send + Sync + Clone + 'static>(&self, event: E) {
        if let Some(chan) = self.get_channel::<E>() {
            let _ = chan.0.send(event);
        }
    }

    /// Subscribes to events of strictly generic type E.
    pub fn subscribe<E: Send + Sync + Clone + 'static>(&self) -> flume::Receiver<E> {
        self.get_or_create_channel::<E>().1.clone()
    }

    fn get_channel<E: Send + Sync + 'static>(
        &self,
    ) -> Option<Arc<(flume::Sender<E>, flume::Receiver<E>)>> {
        self.channels
            .get(&TypeId::of::<E>())
            .and_then(|any| any.clone().downcast().ok())
    }

    fn get_or_create_channel<E: Send + Sync + 'static>(
        &self,
    ) -> Arc<(flume::Sender<E>, flume::Receiver<E>)> {
        let entry = self
            .channels
            .entry(TypeId::of::<E>())
            .or_insert_with(|| Arc::new(flume::unbounded::<E>()));
        entry.clone().downcast().unwrap()
    }
}
