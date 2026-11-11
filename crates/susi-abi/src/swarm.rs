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

#[cfg(test)]
mod prop_tests {
    use super::*;

    fn next_u64(state: &mut u64) -> u64 {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    // Property: a bloom filter never produces a false negative — a hash
    // just inserted always tests as possibly present.
    #[test]
    fn inserted_hash_always_may_contain() {
        let mut state = 0x1234_5678_9abc_def0_u64;
        for hash in [0_u64, 1, u64::MAX]
            .into_iter()
            .chain((0..4_096).map(|_| next_u64(&mut state)))
        {
            let mut bloom = CapabilityBloom::empty();
            bloom.insert_hash(hash);
            assert!(bloom.may_contain_hash(hash));
        }
    }

    // Property: bloom filters are monotonic — inserting more hashes can
    // only set bits, never clear one an earlier insert set.
    #[test]
    fn membership_is_monotonic_under_more_inserts() {
        let mut state = 0x0fed_cba9_8765_4321_u64;
        for _ in 0..2_048 {
            let first = next_u64(&mut state);
            let rest_len = (next_u64(&mut state) % 32) as usize;
            let mut bloom = CapabilityBloom::empty();
            bloom.insert_hash(first);
            for _ in 0..rest_len {
                bloom.insert_hash(next_u64(&mut state));
            }
            assert!(bloom.may_contain_hash(first));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pheromone_kind_and_swarm_role_serialize_to_exact_snake_case_wire_tags() {
        let kinds = [
            (PheromoneKind::Intent, "\"intent\""),
            (PheromoneKind::Observation, "\"observation\""),
            (PheromoneKind::Claim, "\"claim\""),
            (PheromoneKind::Receipt, "\"receipt\""),
            (PheromoneKind::ConsensusVote, "\"consensus_vote\""),
            (PheromoneKind::Heartbeat, "\"heartbeat\""),
        ];
        for (kind, expected) in kinds {
            let encoded = serde_json::to_string(&kind).expect("serialize PheromoneKind");
            assert_eq!(encoded, expected);
            let decoded: PheromoneKind =
                serde_json::from_str(&encoded).expect("deserialize PheromoneKind");
            assert_eq!(decoded, kind);
        }

        let roles = [
            (SwarmRole::Kernel, "\"kernel\""),
            (SwarmRole::InferenceDriver, "\"inference_driver\""),
            (SwarmRole::ToolDriver, "\"tool_driver\""),
            (SwarmRole::PlannerCell, "\"planner_cell\""),
            (SwarmRole::ReflexCell, "\"reflex_cell\""),
            (SwarmRole::ExternalPeer, "\"external_peer\""),
        ];
        for (role, expected) in roles {
            let encoded = serde_json::to_string(&role).expect("serialize SwarmRole");
            assert_eq!(encoded, expected);
            let decoded: SwarmRole = serde_json::from_str(&encoded).expect("deserialize SwarmRole");
            assert_eq!(decoded, role);
        }
    }

    #[test]
    fn swarm_pheromone_and_cell_manifest_round_trip_serde() {
        let pheromone = SwarmPheromone {
            id: "phero-1104".to_string(),
            topic: "intent.code.audit".to_string(),
            emitter_id: "cell-gawd-1".to_string(),
            kind: PheromoneKind::Intent,
            intensity: 0.875,
            payload: serde_json::json!({"crate": "susi-abi"}),
            ttl_ms: 5000,
            deposited_at: 1_700_000_500,
        };
        let encoded_p = serde_json::to_string(&pheromone).expect("serialize SwarmPheromone");
        let decoded_p: SwarmPheromone =
            serde_json::from_str(&encoded_p).expect("deserialize SwarmPheromone");
        assert_eq!(decoded_p, pheromone);

        let mut bloom = CapabilityBloom::empty();
        bloom.insert_hash(0xCAFE_BABE);
        let manifest = SwarmCellManifest {
            cell_id: "cell-reflex-7".to_string(),
            role: SwarmRole::ReflexCell,
            capabilities: vec!["wasm.reflex".to_string(), "fast.gate".to_string()],
            bloom_filter: bloom,
            endpoint: "tcp://127.0.0.1:19095".to_string(),
            trust_score: 0.99,
            last_heartbeat: 1_700_000_600,
        };
        let encoded_m = serde_json::to_string(&manifest).expect("serialize SwarmCellManifest");
        let decoded_m: SwarmCellManifest =
            serde_json::from_str(&encoded_m).expect("deserialize SwarmCellManifest");
        assert_eq!(decoded_m, manifest);
        assert!(decoded_m.bloom_filter.may_contain_hash(0xCAFE_BABE));
    }

    #[test]
    fn empty_capability_bloom_contains_no_bits_and_default_matches_empty() {
        let empty = CapabilityBloom::empty();
        let default_bloom = CapabilityBloom::default();
        assert_eq!(empty, default_bloom);
        assert_eq!(empty.0, [0u8; 32]);
        assert!(!empty.may_contain_hash(0));
        assert!(!empty.may_contain_hash(u64::MAX));
    }
}
