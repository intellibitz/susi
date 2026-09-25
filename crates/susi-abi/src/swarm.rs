//! SUSI Swarm OS Stigmergic Pheromone and Cell Manifest Protocol.
//!
//! Biological stigmergy primitives for decentralized coordination without master bottleneck.

use serde::{Deserialize, Serialize};

/// Classification of a stigmergic pheromone deposited into the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PheromoneKind {
    /// An intent or task requirement seeking capable executor cells.
    Intent,
    /// An observation or intermediate result from an agent.
    Observation,
    /// An asserted hypothesis or claim undergoing validation.
    Claim,
    /// A cryptographic tool execution receipt grounding reality.
    Receipt,
    /// A signed vote or endorsement toward quorum consensus.
    ConsensusVote,
    /// A periodic heartbeat signal announcing cell liveness.
    Heartbeat,
}

/// A biological stigmergic pheromone deposited onto the shared blackboard/field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmPheromone {
    /// Unique pheromone identifier.
    pub id: String,
    /// Semantic topic or intent namespace (e.g. `intent.git.diff`, `consensus.vote`).
    pub topic: String,
    /// Cell ID of the agent or driver that emitted this pheromone.
    pub emitter_id: String,
    /// Classification of this signal.
    pub kind: PheromoneKind,
    /// Signal intensity or priority (0.0 to 1.0). Decays over time or distance.
    pub intensity: f32,
    /// Structured payload data.
    pub payload: serde_json::Value,
    /// Time-to-live in milliseconds before this pheromone naturally evaporates.
    pub ttl_ms: u64,
    /// Unix timestamp in seconds when the pheromone was deposited.
    pub deposited_at: u64,
}

/// Functional role played by an autonomous Swarm Cell within the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwarmRole {
    /// Ring 0 Microkernel: bus arbiter, security gate, audit sealer.
    Kernel,
    /// Ring 1 Neural Inference Driver: provides local or cloud LLM generation.
    InferenceDriver,
    /// Ring 1 Tool Driver: executes MCP tools, filesystem, Docker, or CLI.
    ToolDriver,
    /// Ring 2 Workflow & DAG Planner: orchestrates multi-agent missions.
    PlannerCell,
    /// Ring 2 Reflex Cell: specialized WASI module executing sub-millisecond reflexes.
    ReflexCell,
    /// Ring 3 External Agent Peer: Claude Code, OpenHands, Aider, or LangGraph.
    ExternalPeer,
}

/// 256-bit Capability Bloom Filter used for O(1) matching of intents to capable cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapabilityBloom(pub [u8; 32]);

impl CapabilityBloom {
    /// Creates an empty bloom filter.
    #[must_use]
    pub const fn empty() -> Self {
        Self([0u8; 32])
    }

    /// Sets the bit corresponding to a hashed capability name.
    pub fn insert_hash(&mut self, hash: u64) {
        let bit_index_1 = (hash % 256) as usize;
        let bit_index_2 = ((hash >> 16) % 256) as usize;
        self.0[bit_index_1 / 8] |= 1 << (bit_index_1 % 8);
        self.0[bit_index_2 / 8] |= 1 << (bit_index_2 % 8);
    }

    /// Tests whether a hashed capability could be present in this bloom filter.
    #[must_use]
    pub fn may_contain_hash(&self, hash: u64) -> bool {
        let bit_index_1 = (hash % 256) as usize;
        let bit_index_2 = ((hash >> 16) % 256) as usize;
        let has_1 = (self.0[bit_index_1 / 8] & (1 << (bit_index_1 % 8))) != 0;
        let has_2 = (self.0[bit_index_2 / 8] & (1 << (bit_index_2 % 8))) != 0;
        has_1 && has_2
    }
}

/// Declaration card of an autonomous Swarm Cell within the OS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwarmCellManifest {
    /// Persistent or ephemeral identity of the cell.
    pub cell_id: String,
    /// Functional role of this cell.
    pub role: SwarmRole,
    /// List of explicit human-readable capabilities provided.
    pub capabilities: Vec<String>,
    /// Compact 256-bit capability bloom filter for rapid routing.
    pub bloom_filter: CapabilityBloom,
    /// IPC endpoint URL (e.g. `ipc:///tmp/susi_cell.sock` or `http://127.0.0.1:9091`).
    pub endpoint: String,
    /// Trust score (0.0 to 1.0) based on historical evidence accuracy.
    pub trust_score: f32,
    /// Unix timestamp of last observed heartbeat.
    pub last_heartbeat: u64,
}
