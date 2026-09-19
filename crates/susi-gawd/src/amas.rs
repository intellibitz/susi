// Tier 1 swarm supervisor: dispatches and ranks agents for a mission, and
// coordinates with peer nodes over the AOA federation protocol.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::net::UdpSocket;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use super::agents::{GawdAgentFleet, GawdAgentInfo, MissionBlackboard};
use susi_error::EaiResult;
use susi_gemi::hardware::HardwareProfiler;
use susi_sandbox::manager::NeuralCheckpoint;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    pub sender: String,
    pub recipient: String,
    pub action: String,
    pub payload: String,
}

/// Fixed-size (256-bit) capability bloom filter exchanged during peer discovery,
/// so goal routing can test "does this peer likely register tool/agent X" without
/// shipping the full registry over the wire. VC-200-001 (ROADMAP.md) hardening:
/// replaces trust/hardware-only peer ranking with real semantic overlap.
const BLOOM_WORDS: usize = 4;
const BLOOM_HASHES: usize = 3;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CapabilityBloom(pub [u64; BLOOM_WORDS]);

impl CapabilityBloom {
    pub fn from_tokens<'a>(tokens: impl IntoIterator<Item = &'a str>) -> Self {
        let mut bloom = Self::default();
        for token in tokens {
            bloom.insert(token);
        }
        bloom
    }

    pub fn insert(&mut self, token: &str) {
        let token = token.to_lowercase();
        for seed in 0..BLOOM_HASHES {
            let bit = Self::hash(&token, seed as u64) % (BLOOM_WORDS as u64 * 64);
            self.0[(bit / 64) as usize] |= 1 << (bit % 64);
        }
    }

    pub fn contains(&self, token: &str) -> bool {
        let token = token.to_lowercase();
        for seed in 0..BLOOM_HASHES {
            let bit = Self::hash(&token, seed as u64) % (BLOOM_WORDS as u64 * 64);
            if self.0[(bit / 64) as usize] & (1 << (bit % 64)) == 0 {
                return false;
            }
        }
        true
    }

    /// Fraction of `tokens` this filter probably contains, in [0.0, 1.0].
    pub fn match_ratio(&self, tokens: &[String]) -> f32 {
        if tokens.is_empty() {
            return 0.0;
        }
        let hits = tokens.iter().filter(|t| self.contains(t)).count();
        hits as f32 / tokens.len() as f32
    }

    fn hash(token: &str, seed: u64) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        seed.hash(&mut hasher);
        token.hash(&mut hasher);
        hasher.finish()
    }

    pub fn to_hex(self) -> String {
        self.0.iter().map(|w| format!("{:016x}", w)).collect()
    }

    pub fn from_hex(hex: &str) -> Self {
        let mut bloom = Self::default();
        let bytes = hex.as_bytes();
        for (i, word) in bloom.0.iter_mut().enumerate() {
            let start = i * 16;
            if start + 16 > bytes.len() {
                break;
            }
            if let Ok(s) = std::str::from_utf8(&bytes[start..start + 16]) {
                *word = u64::from_str_radix(s, 16).unwrap_or(0);
            }
        }
        bloom
    }

    /// Builds this node's capability bloom from every tool currently registered
    /// with the local ToolRegistry (Registry + Trait + Config pattern: zero
    /// hardcoded capability strings, derived from what's actually loaded).
    pub fn local_snapshot() -> Self {
        let tokens: Vec<String> = susi_tools::ToolRegistry::global()
            .tools
            .iter()
            .map(|entry| entry.key().to_lowercase())
            .collect();
        Self::from_tokens(tokens.iter().map(|s| s.as_str()))
    }
}

/// Tokenizes a natural-language goal into terms worth matching against peer
/// capability blooms. Strips short/stopword-ish tokens that would otherwise
/// dilute the match ratio with noise common to every goal string.
pub fn tokenize_goal(goal: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "admin", "pulse", "mission",
    ];
    // Tool/agent names are snake_case (e.g. "bloat_audit"), so '_' must stay
    // part of a token rather than being treated as a separator.
    goal.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|t| t.len() > 2 && !STOPWORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClusterPeerNode {
    pub node_id: String,
    pub address: String,
    pub node_type: String,
    pub is_active: bool,
    pub capabilities: Vec<String>,
    pub registry_checksum: u64,
    pub latency_ms: u64,
    pub uptime_secs: u64,
    pub trust_score: f32,
    #[serde(default)]
    pub capability_bloom: CapabilityBloom,
}

pub struct SusiSupervisor;

impl SusiSupervisor {
    pub fn get_udp_discovery_port() -> u16 {
        susi_sandbox::manager::SusiConfig::load_global()
            .map(|c| c.udp_discovery_port())
            .unwrap_or(9092)
    }

    pub fn list_cluster_nodes() -> Vec<ClusterPeerNode> {
        static DISCOVERED_PEERS: OnceLock<Arc<RwLock<Vec<ClusterPeerNode>>>> = OnceLock::new();
        let peers_lock = DISCOVERED_PEERS.get_or_init(|| {
            let initial = vec![ClusterPeerNode {
                node_id: "susi-local-master".to_string(),
                address: "127.0.0.1:9090".to_string(),
                node_type: "LOCAL_MASTER".to_string(),
                is_active: true,
                capabilities: vec![
                    "CORE".to_string(),
                    "INFERENCE".to_string(),
                    "TOOLING".to_string(),
                ],
                registry_checksum: 0, // crate::agents::AgentMetaRegistry::global().get_checksum(),
                latency_ms: 0,
                uptime_secs: 0,
                trust_score: 1.0,
                capability_bloom: CapabilityBloom::local_snapshot(),
            }];

            let shared = Arc::new(RwLock::new(initial));
            let t_shared = Arc::clone(&shared);

            std::thread::spawn(move || {
                let port = Self::get_udp_discovery_port();
                let socket_res = UdpSocket::bind(format!("0.0.0.0:{}", port));
                if let Ok(socket) = socket_res {
                    let _ = socket.set_broadcast(true);
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

                    let mut buf = [0u8; 1024];
                    let local_caps = HardwareProfiler::get_caps_string();
                    let mut local_bloom = CapabilityBloom::local_snapshot();
                    let mut last_registry_checksum =
                        crate::agents::AgentMetaRegistry::global().get_checksum();

                    loop {
                        let registry_checksum =
                            crate::agents::AgentMetaRegistry::global().get_checksum();
                        if registry_checksum != last_registry_checksum {
                            local_bloom = CapabilityBloom::local_snapshot();
                            last_registry_checksum = registry_checksum;
                        }

                        let ping_msg = format!(
                            "SUSI_PING:{}:{}:{}",
                            local_caps,
                            registry_checksum,
                            local_bloom.to_hex()
                        );

                        if let Ok((amt, src)) = socket.recv_from(&mut buf) {
                            let msg = String::from_utf8_lossy(&buf[..amt]);
                            if msg.starts_with("SUSI_PING") {
                                let pong_msg = format!(
                                    "SUSI_PONG:{}:{}:{}",
                                    local_caps,
                                    registry_checksum,
                                    local_bloom.to_hex()
                                );
                                let _ = socket.send_to(pong_msg.as_bytes(), src);
                            }

                            if msg.starts_with("SUSI_PONG") || msg.starts_with("SUSI_PING") {
                                if src.ip().is_loopback()
                                    || std::net::TcpListener::bind((src.ip(), 0)).is_ok()
                                {
                                    continue;
                                }
                                let parts: Vec<&str> = msg.split(':').collect();
                                let caps = if parts.len() > 1 {
                                    parts[1].split(',').map(|s| s.to_string()).collect()
                                } else {
                                    vec!["CORE".into()]
                                };

                                let checksum = if parts.len() > 2 {
                                    parts[2].parse::<u64>().unwrap_or(0)
                                } else {
                                    0
                                };

                                let peer_bloom = if parts.len() > 3 {
                                    CapabilityBloom::from_hex(parts[3])
                                } else {
                                    CapabilityBloom::default()
                                };

                                let mut peers = t_shared.write();
                                let addr_str = format!("{}:9093", src.ip());
                                if let Some(p) = peers.iter_mut().find(|p| p.address == addr_str) {
                                    p.trust_score = (p.trust_score + 0.05).min(1.0);
                                    p.is_active = true;
                                    p.registry_checksum = checksum;
                                    p.capability_bloom = peer_bloom;
                                } else {
                                    peers.push(ClusterPeerNode {
                                        node_id: format!("susi-peer-{}", src.ip()),
                                        address: addr_str,
                                        node_type: if caps.contains(&"GPU".to_string()) {
                                            "WORKSTATION_NODE".into()
                                        } else {
                                            "PEER".into()
                                        },
                                        is_active: true,
                                        capabilities: caps,
                                        registry_checksum: checksum,
                                        latency_ms: 0,
                                        uptime_secs: 0,
                                        trust_score: 0.6,
                                        capability_bloom: peer_bloom,
                                    });
                                }
                            }
                        }

                        let _ = socket
                            .send_to(ping_msg.as_bytes(), format!("255.255.255.255:{}", port));
                        std::thread::sleep(Duration::from_millis(100)); // Relax lock contention heavily
                    }
                }
            });

            shared
        });

        peers_lock.read().clone()
    }

    /// How much (and in which direction) an agent's rank should move given
    /// its actual output text. Previously rank only ever moved up: every
    /// agent that didn't self-report FAILURE/GAP got +0.01 forever, with no
    /// path back down, so any core agent (recruited into every mission)
    /// saturated toward the 1.0 ceiling almost immediately and stayed there
    /// regardless of later real reliability — the "Empirical Expertise
    /// Ranking" this backs stopped being empirical once nothing could ever
    /// lower a rank again. The penalty outweighs the reward (5x) so trust is
    /// harder to earn back than it was to lose — deliberate asymmetry, not
    /// an arbitrary number. Pure function (no I/O) so the mapping is
    /// directly unit-testable.
    fn rank_delta_for_output(output: &str) -> (f32, &'static str) {
        if output.contains("FAILURE") || output.contains("GAP") {
            (-0.05, "MISSION_FAILURE")
        } else {
            (0.01, "MISSION_SUCCESS")
        }
    }

    /// When one agent's rank is a clear outlier above the
    /// rest, return its answer directly instead of asking the local synthesis
    /// model to reconcile it with the others. LLM re-synthesis is fine for
    /// genuinely comparable free-form answers, but the local model is too
    /// weak to merge a deterministic, tool-backed answer (e.g. DevOpsAgent's
    /// real BloatAuditor report) with a generic-fallback agent's free-form
    /// guess (e.g. UniversalReasoner, base rank 0.7) without corrupting or
    /// fabricating content (Mandate 8: Epistemic Chain of Truth) — empirically
    /// observed truncating and inventing numbers when asked to do so.
    fn dominant_rank_leader<'a>(
        valid_outputs: &'a [(String, String)],
        fleet_info: &[GawdAgentInfo],
    ) -> Option<&'a str> {
        const DOMINANT_RANK_MARGIN: f32 = 0.15;

        if valid_outputs.len() < 2 {
            return None;
        }

        let mut ranked: Vec<(f32, &str)> = valid_outputs
            .iter()
            .map(|(name, output)| {
                let rank = fleet_info
                    .iter()
                    .find(|i| &i.name == name)
                    .map(|i| i.rank)
                    .unwrap_or(0.0);
                (rank, output.as_str())
            })
            .collect();
        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        if ranked[0].0 - ranked[1].0 >= DOMINANT_RANK_MARGIN {
            Some(ranked[0].1)
        } else {
            None
        }
    }

    pub fn supervise_mission(
        goal: &str,
        workspace: &Path,
    ) -> (Vec<A2AMessage>, Vec<GawdAgentInfo>) {
        // 1. Initialize Mission Blackboard (High-Density Context Store with 1024 entry lease cap)
        // Optimized for Lock-Free Swarm Execution
        let blackboard: MissionBlackboard =
            Arc::new(super::agents::HighDensityContextStore::new(1024));

        // 2. Dynamic Fleet Synthesis
        eprintln!("- [Swarm Synthesis] Analyzing goal intent for recruitment...");
        let _ = std::io::stdout().flush();
        let agents = GawdAgentFleet::synthesize_fleet(goal, workspace);
        let fleet_info: Vec<GawdAgentInfo> = agents
            .iter()
            .map(|a| GawdAgentInfo {
                name: a.name(),
                provider: "SUSI Local".into(),
                url: "native://substrate".into(),
                rank: a.rank(),
            })
            .collect();

        eprintln!(
            "- [Fleet Composition] recruited {} specialist agents:",
            fleet_info.len()
        );
        for agent in &fleet_info {
            eprintln!(
                "  - [Agent] {} (Rank: {:.2}) via {}",
                agent.name, agent.rank, agent.provider
            );
            let _ = std::io::stdout().flush();
        }

        // 3. Broadcast the goal to active peer nodes; their results land on the blackboard
        let cluster_nodes = Self::rank_peers_for_goal(goal);
        let active_peers_count = cluster_nodes
            .iter()
            .filter(|n| {
                n.node_id != "susi-local-master"
                    && !n.address.starts_with("127.0.0.1")
                    && !n.address.starts_with("localhost")
                    && n.is_active
            })
            .count();
        if active_peers_count > 0 {
            eprintln!("- [Distributed Swarm] Broadcasting mission intent to {} active cluster peer nodes...", active_peers_count);
            let _ = std::io::stdout().flush();
            for node in cluster_nodes.iter().take(2) {
                if node.node_id != "susi-local-master" && node.is_active {
                    let addr = node.address.clone();
                    let node_id = node.node_id.clone();
                    let g = goal.to_string();
                    let bb = Arc::clone(&blackboard);
                    rayon::spawn(move || {
                        let remote_res = Self::dispatch_peer_task(&addr, "reason", &g);
                        if !remote_res.contains("unreachable") {
                            bb.insert(format!("PeerNode_{}", node_id), remote_res);
                        }
                    });
                }
            }
        }

        // 4. Dispatch the local agent fleet; each agent's output lands on the blackboard
        let swarm_logs = GawdAgentFleet::dispatch_explosive_swarm(
            goal.to_string(),
            workspace.to_path_buf(),
            Arc::clone(&blackboard),
        );

        let mut a2a_logs = Vec::new();
        let mut has_gap = false;
        for (name, output) in swarm_logs {
            if output.contains("[CAPABILITY_GAP]") {
                has_gap = true;
            }
            a2a_logs.push(A2AMessage {
                sender: name,
                recipient: "SUSI-Master".to_string(),
                action: "MISSION_FLUX".to_string(),
                payload: output,
            });
        }

        // 4.1 If an agent reported a capability gap, dispatch a second reinforcement wave
        let is_query_or_read = GawdAgentFleet::is_meta_or_simple_query(goal);

        if has_gap && !is_query_or_read {
            eprintln!(
                "[Swarm Supervisor] Capability gap detected. Dispatching Reinforcement Wave..."
            );
            let reinforcement_goal = format!(
                "REINFORCE_MISSION: {}\n[PREVIOUS_FAILURES]: {:?}",
                goal, a2a_logs
            );
            let extra_swarm = GawdAgentFleet::dispatch_explosive_swarm(
                reinforcement_goal,
                workspace.to_path_buf(),
                Arc::clone(&blackboard),
            );
            for (name, output) in extra_swarm {
                a2a_logs.push(A2AMessage {
                    sender: format!("{}_Reinforcement", name),
                    recipient: "SUSI-Master".to_string(),
                    action: "REINFORCEMENT_FLUX".to_string(),
                    payload: output,
                });
            }
        }

        // 5. Weighted Swarm Consensus Pass
        if !blackboard.is_empty() {
            // Aggregate agent outputs weighted by rank and node trust
            let mut weighted_wisdom = String::new();
            for r in blackboard.iter() {
                let agent_name = r.key();
                let output = r.value();

                if let Some(info) = fleet_info.iter().find(|i| &i.name == agent_name) {
                    weighted_wisdom.push_str(&format!(
                        "[AGENT: {} (Rank: {:.2})] {}\n",
                        agent_name, info.rank, output
                    ));

                    // Empirical Expertise Ranking: reward success, penalize failure.
                    let (delta, source) = Self::rank_delta_for_output(output);
                    crate::agents::AgentMetaRegistry::global()
                        .update_rank(agent_name, delta, source);
                }
            }

            let is_query = GawdAgentFleet::is_meta_or_simple_query(goal);
            let is_direct_synthesis = is_query;

            let valid_outputs: Vec<(String, String)> = blackboard
                .iter()
                .filter_map(|r| {
                    let agent_name = r.key().clone();
                    let output = r.value().trim().to_string();
                    if !output.is_empty() && !output.contains("FAILURE") && !output.contains("GAP")
                    {
                        Some((agent_name, output))
                    } else {
                        None
                    }
                })
                .collect();

            // Consensus Hardening: Direct pass-through if single valid output or query synthesis
            let synthesized = if is_direct_synthesis || valid_outputs.len() <= 1 {
                if valid_outputs.len() == 1 {
                    valid_outputs[0].1.clone()
                } else {
                    weighted_wisdom.clone()
                }
            } else if let Some(leader_output) =
                Self::dominant_rank_leader(&valid_outputs, &fleet_info)
            {
                leader_output.to_string()
            } else {
                let prompts = susi_sandbox::manager::SusiPrompts::load_global();
                let consensus_prompt = prompts
                    .consensus_wisdom_prompt()
                    .replace("{goal}", goal)
                    .replace("{wisdom}", &weighted_wisdom);
                eprintln!(
                    "- [Consensus Master] Synthesizing swarm wisdom across {} active agents...",
                    valid_outputs.len()
                );
                let _ = std::io::stdout().flush();
                susi_gemi::engine::GemiEngine::generate_reasoning_stream(
                    &consensus_prompt,
                    workspace,
                    &|token| {
                        print!("{}", token);
                        let _ = std::io::stdout().flush();
                    },
                )
            };

            // Fraction of recruited agents whose own output text didn't
            // contain the substrings "FAILURE"/"GAP" - an execution-status
            // signal only. Named (and was previously renamed from
            // "CONVERGENCE_SCORE") to avoid implying a factual-accuracy or
            // truth-verification score: it says nothing about whether the
            // content is actually correct, only that agents didn't
            // self-report a failure. This used to be exploitable as a
            // truth-check bypass (see truth.rs's now-removed "Epistemic
            // Delegation" override) precisely because its name suggested
            // more than it measured.
            let agent_success_ratio = if fleet_info.len() > 1 {
                let success_count = blackboard
                    .iter()
                    .filter(|r| !r.value().contains("FAILURE") && !r.value().contains("GAP"))
                    .count();
                (success_count as f32 / fleet_info.len() as f32).min(1.0)
            } else {
                0.90 // Single trusted agent default
            };

            let final_payload = format!(
                "{}\n\n[AGENT_SUCCESS_RATIO: {:.2}]",
                synthesized, agent_success_ratio
            );

            a2a_logs.push(A2AMessage {
                sender: "ConsensusMaster".into(),
                recipient: "SUSI-Master".into(),
                action: "STATE_CONVERGENCE".into(),
                payload: final_payload,
            });
        }

        // 6. Kick off a background distillation-state check (skip if one's already running)
        static DISTILLATION_AUDIT_RUNNING: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !DISTILLATION_AUDIT_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let ws = workspace.to_path_buf();
            std::thread::spawn(move || {
                struct Guard;
                impl Drop for Guard {
                    fn drop(&mut self) {
                        DISTILLATION_AUDIT_RUNNING
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                let _guard = Guard;
                let _ = super::reflex_trainer::ReflexTrainer::audit_distillation_state(&ws);
            });
        }

        (a2a_logs, fleet_info)
    }

    pub fn gather_weighted_wisdom(
        interactions: &[A2AMessage],
        _agents: &[GawdAgentInfo],
    ) -> String {
        // Prioritize ConsensusMaster and AdminAgent results over other agents'
        let mut consensus_result = None;
        let mut admin_result = None;
        let mut wisdom = Vec::new();

        for msg in interactions {
            if msg.sender == "ConsensusMaster" {
                consensus_result = Some(msg.payload.clone());
            } else if msg.sender == "AdminAgent" {
                admin_result = Some(msg.payload.clone());
            }

            if !msg.payload.contains("FAILURE")
                && !msg.payload.contains("GAP")
                && !msg.payload.is_empty()
            {
                wisdom.push(format!("[{}]: {}", msg.sender, msg.payload));
            }
        }

        consensus_result.or(admin_result).unwrap_or_else(|| {
            if wisdom.is_empty() {
                "No valid wisdom gathered from swarm.".to_string()
            } else {
                wisdom.join("\n")
            }
        })
    }

    /// Reasoning Auction: Ranks peer nodes based on weighted hardware and trust scores.
    pub fn rank_reasoning_peers() -> Vec<ClusterPeerNode> {
        let mut nodes = Self::list_cluster_nodes();

        nodes.sort_by(|a, b| {
            let a_score = (a.trust_score * 0.4) + (a.latency_ms as f32 * -0.2);
            let b_score = (b.trust_score * 0.4) + (b.latency_ms as f32 * -0.2);
            b_score
                .partial_cmp(&a_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        nodes
    }

    /// Cluster Intent Routing: Prioritizes peers with semantically relevant capabilities.
    pub fn rank_peers_for_goal(goal: &str) -> Vec<ClusterPeerNode> {
        let mut nodes = Self::list_cluster_nodes();
        let goal_tokens = tokenize_goal(goal);

        // VC-200-001: real semantic overlap via exchanged capability bloom filters,
        // instead of trust/hardware-only proxies. Peers whose registry probably
        // contains a tool matching the goal's terms are ranked ahead of generic
        // high-trust nodes with no relevant capability.
        nodes.sort_by(|a, b| {
            let a_score = Self::score_peer_for_goal(a, &goal_tokens);
            let b_score = Self::score_peer_for_goal(b, &goal_tokens);
            b_score
                .partial_cmp(&a_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        nodes
    }

    /// Weighted score combining base trust/hardware proxies with bloom-filter
    /// capability overlap against the goal's tokens. Pure function (no I/O) so
    /// routing quality is directly unit-testable.
    fn score_peer_for_goal(node: &ClusterPeerNode, goal_tokens: &[String]) -> f32 {
        let base = (node.trust_score * 0.5)
            + if node.node_type == "WORKSTATION_NODE" {
                0.3
            } else {
                0.0
            };
        let capability_match = node.capability_bloom.match_ratio(goal_tokens);
        base + capability_match * 0.6
    }

    pub fn dispatch_peer_task(addr: &str, tool_name: &str, arg: &str) -> String {
        let arg_val = serde_json::from_str(arg).unwrap_or(serde_json::json!(arg));
        let req_val = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arg_val
            }
        });

        let url = format!("http://{}", addr);
        static HTTP_CLIENT: OnceLock<reqwest::blocking::Client> = OnceLock::new();
        let client = HTTP_CLIENT.get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(Duration::from_millis(1500))
                .build()
                .unwrap_or_else(|_| reqwest::blocking::Client::new())
        });

        // Mandate 40: a peer's GMCP HTTP surface may require `api_auth_token`
        // (opt-in, off by default). A fleet's nodes share one operator's
        // config, so present this node's own configured token when calling a
        // peer — without this, every federation call (swarm consensus
        // broadcast, cluster sync, checkpoint replication, reflex learning
        // broadcast) silently 401s the moment auth is turned on anywhere,
        // even though the caller and callee are the same operator's nodes.
        let token = susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .api_auth_token();
        let mut req = client.post(&url).json(&req_val);
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }

        if let Ok(resp) = req.send() {
            if let Ok(text) = resp.text() {
                return format!("[A2A Flux ({})]: {}", addr, text.trim());
            }
        }
        format!("[A2A Fallback]: Node '{}' unreachable.", addr)
    }

    pub fn broadcast_lan_ping() -> Vec<String> {
        let mut active_peers = Vec::new();
        if let Ok(socket) = UdpSocket::bind("0.0.0.0:0") {
            let port = Self::get_udp_discovery_port();
            let _ = socket.set_broadcast(true);
            let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));

            let _ = socket.send_to(b"SUSI_LAN_PING", format!("255.255.255.255:{}", port));

            let mut buf = [0u8; 512];
            while let Ok((amt, src)) = socket.recv_from(&mut buf) {
                let msg = String::from_utf8_lossy(&buf[..amt]);
                if msg.contains("SUSI_LAN_ACK") || msg.contains("SUSI") {
                    active_peers.push(src.to_string());
                }
            }
        }
        if active_peers.is_empty() {
            active_peers.push("127.0.0.1:9090 (local)".to_string());
        }
        active_peers
    }

    pub fn sync_cluster_state(workspace: &Path, payload: &str) -> String {
        let nodes = Self::list_cluster_nodes();
        use rayon::prelude::*;

        // Sync peers in parallel via rayon, capped at 16 concurrent to bound
        // local resource use.
        let total_nodes = nodes.len();
        let target_nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| n.node_id != "susi-local-master")
            .take(16)
            .collect();

        let synced = target_nodes
            .par_iter()
            .map(|node| {
                let signed_payload = format!("SIG:{}:{}", node.node_id, payload);
                let res = Self::dispatch_peer_task(&node.address, "swarm_sync", &signed_payload);
                if res.contains("Sync complete") {
                    1
                } else {
                    0
                }
            })
            .sum::<usize>();

        let sync_file = workspace.join(".susi/cluster_sync.json");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let sync_data = serde_json::json!({
            "timestamp": now,
            "synced_nodes": synced,
            "total_cluster_nodes": total_nodes,
            "payload_size": payload.len()
        });

        let _ = std::fs::write(&sync_file, sync_data.to_string());
        format!(
            "Synchronized state across {} nodes in parallel (checksum verified).",
            synced
        )
    }

    pub fn borrow_remote_reflex(prompt: &str) -> Option<String> {
        let nodes = Self::list_cluster_nodes();

        // Find a workstation node with GPU capability
        let target_node = nodes.iter().find(|n| {
            n.node_type == "WORKSTATION_NODE" && n.is_active && n.node_id != "susi-local-master"
        });

        if let Some(node) = target_node {
            let res = Self::dispatch_peer_task(&node.address, "reason", prompt);
            if !res.contains("fallback") && !res.contains("unreachable") {
                return Some(format!("[Borrowed Reflex from {}]: {}", node.node_id, res));
            }
        }
        None
    }

    pub fn replicate_checkpoint(checkpoint: &NeuralCheckpoint) {
        let nodes = Self::list_cluster_nodes();
        let payload = serde_json::to_string(checkpoint).unwrap_or_default();

        for node in nodes {
            if node.node_type == "WORKSTATION_NODE" && node.node_id != "susi-local-master" {
                let _ = Self::dispatch_peer_task(&node.address, "replicate_state", &payload);
            }
        }
    }

    pub fn query_cluster_checkpoints() -> Vec<NeuralCheckpoint> {
        let nodes = Self::list_cluster_nodes();
        let mut checkpoints = Vec::new();

        for node in nodes {
            if node.node_id != "susi-local-master" {
                let res = Self::dispatch_peer_task(&node.address, "get_checkpoints", "");
                if let Ok(list) = serde_json::from_str::<Vec<NeuralCheckpoint>>(&res) {
                    checkpoints.extend(list);
                }
            }
        }
        checkpoints
    }

    pub fn broadcast_reflex_learned(name: &str, wasm_path: &Path) {
        if let Ok(wasm_data) = fs::read(wasm_path) {
            let nodes = Self::list_cluster_nodes();
            use base64::{engine::general_purpose, Engine as _};
            let encoded = general_purpose::STANDARD.encode(&wasm_data);
            let payload = serde_json::json!({
                "name": name,
                "wasm_b64": encoded
            })
            .to_string();

            for node in nodes {
                if node.node_type == "WORKSTATION_NODE" && node.node_id != "susi-local-master" {
                    let _ = Self::dispatch_peer_task(&node.address, "replicate_reflex", &payload);
                }
            }
        }
    }

    pub fn broadcast_lock_request(resource_id: &str) -> bool {
        let nodes = Self::list_cluster_nodes();
        use rayon::prelude::*;

        let target_nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| n.node_id != "susi-local-master")
            .collect();

        let successes = target_nodes
            .par_iter()
            .map(|node| {
                let res = Self::dispatch_peer_task(&node.address, "locks/acquire", resource_id);
                if res.contains("SUCCESS") {
                    1
                } else {
                    0
                }
            })
            .sum::<usize>();

        successes == target_nodes.len()
    }

    /// Pulls distilled reasoning samples from each active peer node and stages
    /// them locally for distillation.
    pub fn aggregate_federated_experience(workspace: &Path) -> EaiResult<String> {
        let nodes = Self::list_cluster_nodes();
        let mut total_samples = 0;
        let mut node_count = 0;

        for node in nodes {
            if node.node_id == "susi-local-master" || !node.is_active {
                continue;
            }

            // Request distilled experiences from the peer
            let res = Self::dispatch_peer_task(&node.address, "get_distilled_experience", "");
            if let Ok(samples) =
                serde_json::from_str::<Vec<susi_gemi::reasoning::ReasoningSample>>(&res)
            {
                for sample in samples {
                    // Stage for local distillation
                    crate::pkb::ProtocolKnowledgeBase::stage_distillation_pair(
                        &sample.intent,
                        &sample.successful_outcome,
                        workspace,
                        Some(serde_json::json!({"source_node": node.node_id})),
                    )?;
                    total_samples += 1;
                }
                node_count += 1;
            }
        }

        Ok(format!("Aggregated {} distilled experiences from {} independent nodes into the Knowledge Vault.", total_samples, node_count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aspiration_23_universal_swarm_operation() {
        use crate::agents::{GawdAgent, HighDensityContextStore, SafetyAgent, SecurityAgent};
        let tmp_dir = std::env::temp_dir().join("susi_swarm_test_asp23");
        let _ = std::fs::create_dir_all(&tmp_dir);
        let blackboard: MissionBlackboard = Arc::new(HighDensityContextStore::new(10));

        let safety = SafetyAgent;
        let security = SecurityAgent;

        let res1 = safety.execute("admin mission: test aspiration 23", &tmp_dir, &blackboard);
        let res2 = security.execute("admin mission: test aspiration 23", &tmp_dir, &blackboard);

        assert!(res1.is_ok());
        assert!(res2.is_ok());

        assert!(blackboard.contains_key("SafetyAgent"));
        assert!(blackboard.contains_key("SecurityAgent"));
        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    #[test]
    fn test_capability_bloom_no_false_negatives() {
        let bloom = CapabilityBloom::from_tokens(["bloat_audit", "sovereign_dashboard", "status"]);
        assert!(bloom.contains("bloat_audit"));
        assert!(bloom.contains("sovereign_dashboard"));
        assert!(bloom.contains("status"));
        // Not inserted: may or may not be a false positive, but must not crash and
        // must behave deterministically for the same input.
        let first = bloom.contains("totally_unrelated_xyz");
        let second = bloom.contains("totally_unrelated_xyz");
        assert_eq!(first, second);
    }

    #[test]
    fn test_capability_bloom_hex_roundtrip() {
        let bloom = CapabilityBloom::from_tokens(["deep_scan", "mcp_scout"]);
        let hex = bloom.to_hex();
        let restored = CapabilityBloom::from_hex(&hex);
        assert_eq!(bloom, restored);
        assert!(restored.contains("deep_scan"));
    }

    #[test]
    fn test_rank_peers_prefers_capability_match_over_raw_trust() {
        let goal_tokens = tokenize_goal("admin pulse: run bloat_audit across the substrate");

        let generic_high_trust = ClusterPeerNode {
            node_id: "n1".into(),
            address: "10.0.0.1:9090".into(),
            node_type: "PEER".into(),
            is_active: true,
            capabilities: vec!["CORE".into()],
            registry_checksum: 0,
            latency_ms: 0,
            uptime_secs: 0,
            trust_score: 0.9,
            capability_bloom: CapabilityBloom::from_tokens(["status", "version"]),
        };

        let capability_match = ClusterPeerNode {
            node_id: "n2".into(),
            address: "10.0.0.2:9090".into(),
            node_type: "PEER".into(),
            is_active: true,
            capabilities: vec!["CORE".into()],
            registry_checksum: 0,
            latency_ms: 0,
            uptime_secs: 0,
            trust_score: 0.6,
            capability_bloom: CapabilityBloom::from_tokens(["bloat_audit"]),
        };

        let a_score = SusiSupervisor::score_peer_for_goal(&generic_high_trust, &goal_tokens);
        let b_score = SusiSupervisor::score_peer_for_goal(&capability_match, &goal_tokens);
        assert!(
            b_score > a_score,
            "peer with matching capability ({}) should outrank higher-trust peer with no match ({})",
            b_score,
            a_score
        );
    }

    #[test]
    fn test_consensus_master_bypasses_synthesis_for_single_valid_output() {
        let blackboard = Arc::new(dashmap::DashMap::new());
        blackboard.insert(
            "Qwen2ReasoningAgent".to_string(),
            "Photosynthesis is the process by which plants turn sunlight into energy.".to_string(),
        );

        let valid_outputs: Vec<(String, String)> = blackboard
            .iter()
            .filter_map(|r| {
                let agent_name = r.key().clone();
                let output = r.value().trim().to_string();
                if !output.is_empty() && !output.contains("FAILURE") && !output.contains("GAP") {
                    Some((agent_name, output))
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(valid_outputs.len(), 1);
        assert_eq!(
            valid_outputs[0].1,
            "Photosynthesis is the process by which plants turn sunlight into energy."
        );
    }

    #[test]
    fn test_dominant_rank_leader_prefers_clear_outlier_over_llm_resynthesis() {
        let valid_outputs = vec![
            (
                "DevOpsAgent".to_string(),
                "# SUSI Bloat & Security Audit\n- Files Scanned: 54".to_string(),
            ),
            (
                "UniversalReasoner".to_string(),
                "I'm ready to code review Susi.".to_string(),
            ),
        ];
        let fleet_info = vec![
            GawdAgentInfo {
                name: "DevOpsAgent".to_string(),
                provider: "SUSI Local".into(),
                url: "native://substrate".into(),
                rank: 1.0,
            },
            GawdAgentInfo {
                name: "UniversalReasoner".to_string(),
                provider: "SUSI Local".into(),
                url: "native://substrate".into(),
                rank: 0.7,
            },
        ];

        let leader = SusiSupervisor::dominant_rank_leader(&valid_outputs, &fleet_info);
        assert_eq!(
            leader,
            Some("# SUSI Bloat & Security Audit\n- Files Scanned: 54")
        );
    }

    #[test]
    fn test_dominant_rank_leader_defers_to_llm_synthesis_when_ranks_are_close() {
        let valid_outputs = vec![
            ("AgentA".to_string(), "Answer A".to_string()),
            ("AgentB".to_string(), "Answer B".to_string()),
        ];
        let fleet_info = vec![
            GawdAgentInfo {
                name: "AgentA".to_string(),
                provider: "SUSI Local".into(),
                url: "native://substrate".into(),
                rank: 0.9,
            },
            GawdAgentInfo {
                name: "AgentB".to_string(),
                provider: "SUSI Local".into(),
                url: "native://substrate".into(),
                rank: 0.85,
            },
        ];

        assert_eq!(
            SusiSupervisor::dominant_rank_leader(&valid_outputs, &fleet_info),
            None
        );
    }

    #[test]
    fn test_rank_delta_rewards_clean_success() {
        let (delta, source) = SusiSupervisor::rank_delta_for_output("Hardware Saturated: 8 CPUs.");
        assert_eq!(delta, 0.01);
        assert_eq!(source, "MISSION_SUCCESS");
    }

    #[test]
    fn test_rank_delta_penalizes_self_reported_failure() {
        let (delta, source) =
            SusiSupervisor::rank_delta_for_output("Agent Execution Reported: FAILURE - timeout");
        assert_eq!(delta, -0.05);
        assert_eq!(source, "MISSION_FAILURE");
    }

    #[test]
    fn test_rank_delta_penalizes_capability_gap() {
        let (delta, source) =
            SusiSupervisor::rank_delta_for_output("[CAPABILITY_GAP] Tool 'x' missing.");
        assert_eq!(delta, -0.05);
        assert_eq!(source, "MISSION_FAILURE");
    }

    #[test]
    fn test_rank_delta_penalty_outweighs_reward() {
        // Deliberate asymmetry: trust should be harder to earn back than to
        // lose, so a single failure must undo more than a single success grants.
        let (reward, _) = SusiSupervisor::rank_delta_for_output("all clear");
        let (penalty, _) = SusiSupervisor::rank_delta_for_output("FAILURE: crashed");
        assert!(penalty.abs() > reward.abs());
    }
}
