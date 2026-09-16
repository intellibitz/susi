// SUSI Asynchronous Swarm Event Bus
// Real-time inter-agent messaging and telemetry streaming via flume.

use serde::{Deserialize, Serialize};

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

pub type SwarmEventBus = (
    flume::Sender<SwarmEventType>,
    flume::Receiver<SwarmEventType>,
);

pub fn create_swarm_bus() -> SwarmEventBus {
    flume::unbounded()
}
