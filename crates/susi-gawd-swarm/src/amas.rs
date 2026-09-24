// Tier 1 swarm supervisor: dispatches and ranks agents for a mission, and
// coordinates with peer nodes over the AOA federation protocol.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::net::UdpSocket;
use std::path::Path;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::peer_registry;
use crate::susi_core::plane_bus::gemi::HardwareProfiler;
use susi_gawd_agents::agents::{GawdAgentFleet, GawdAgentInfo, MissionBlackboard};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    pub sender: String,
    pub recipient: String,
    pub action: String,
    pub payload: String,
}

/// Fixed-size (256-bit) capability bloom filter exchanged during peer discovery,
/// so goal routing can test "does this peer likely register tool/agent X" without
/// shipping the full registry over the wire. VC-200-001 (roadmap.json) hardening:
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
        let tokens: Vec<String> = crate::susi_core::registry::CapabilityRegistry::global()
            .list_tools()
            .into_iter()
            .map(|name| name.to_lowercase())
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

/// How a peer entered the cluster roster — controls whether the host bearer
/// token may ever be presented on outbound calls to that address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PeerAdmission {
    /// This process / loopback — same operator, same host.
    Local,
    /// Verified by the `~/.susi/cluster.key` HMAC handshake (or an operator
    /// allowlist). May receive the host bearer and feed mission quorum.
    Explicit,
    /// Learned from unauthenticated UDP discovery. **Never** receive the host
    /// bearer token — any LAN host that answers a ping would otherwise steal it.
    #[default]
    Discovered,
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
    #[serde(default)]
    pub admission: PeerAdmission,
    /// Unix seconds of the last pong received from this peer. `0` means the
    /// peer was rehydrated from disk but has not re-verified since boot —
    /// such entries start inactive and must pong before they vote or lead.
    #[serde(default)]
    pub last_seen_secs: u64,
}

/// A peer that has not ponged within this window is marked inactive by the
/// scout sweep — dead members lose quorum weight and election eligibility
/// until they re-verify with a fresh signed pong.
pub const PEER_STALE_SECS: u64 = 30;

impl ClusterPeerNode {
    /// True when this peer has not ponged within `PEER_STALE_SECS`. Local
    /// nodes are exempt — the loopback entry is always live.
    pub fn is_stale(&self, now_secs: u64) -> bool {
        !matches!(self.admission, PeerAdmission::Local)
            && now_secs.saturating_sub(self.last_seen_secs) > PEER_STALE_SECS
    }
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub struct SusiSupervisor;

impl SusiSupervisor {
    pub fn get_udp_discovery_port() -> u16 {
        crate::susi_paths::ports::UDP_DISCOVERY
    }

    pub fn list_cluster_nodes() -> Vec<ClusterPeerNode> {
        static DISCOVERED_PEERS: OnceLock<Arc<RwLock<Vec<ClusterPeerNode>>>> = OnceLock::new();
        let peers_lock = DISCOVERED_PEERS.get_or_init(|| {
            let initial = vec![ClusterPeerNode {
                // The persisted wire identity — consensus needs every
                // node's coordinator seq chain, chain linkage, and
                // leader election to live in its own id namespace.
                node_id: crate::susi_config::cluster_key::wire_node_id(),
                address: format!("127.0.0.1:{}", crate::susi_paths::ports::GMCP),
                node_type: "LOCAL_MASTER".to_string(),
                is_active: true,
                capabilities: vec![
                    "CORE".to_string(),
                    "INFERENCE".to_string(),
                    "TOOLING".to_string(),
                ],
                registry_checksum: 0, // susi_gawd_agents::agents::AgentMetaRegistry::global().get_checksum(),
                latency_ms: 0,
                uptime_secs: 0,
                trust_score: 1.0,
                capability_bloom: CapabilityBloom::local_snapshot(),
                admission: PeerAdmission::Local,
                last_seen_secs: now_secs(),
            }];

            // Rehydrate cluster-key-verified peers from the persistent
            // registry — only `Explicit` entries ever land in peers.json, so
            // a signed peer survives restart without re-handshaking.
            let mut initial = initial;
            for peer in peer_registry::load_persisted_peers() {
                if !initial.iter().any(|n| n.address == peer.address) {
                    initial.push(peer);
                }
            }

            let shared = Arc::new(RwLock::new(initial));
            let t_shared = Arc::clone(&shared);

            std::thread::spawn(move || {
                // Host contract: UDP 9092 belongs to the daemon alone.
                // Peer scouts bind an ephemeral port and talk *to* 9092.
                let port = Self::get_udp_discovery_port();
                let socket_res = UdpSocket::bind("0.0.0.0:0");
                if let Ok(socket) = socket_res {
                    let _ = socket.set_broadcast(true);
                    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

                    let mut buf = [0u8; 1024];
                    let local_caps = HardwareProfiler::get_caps_string();
                    let mut local_bloom = CapabilityBloom::local_snapshot();
                    let mut last_registry_checksum =
                        susi_gawd_agents::agents::AgentMetaRegistry::global().get_checksum();
                    // Outstanding signed-handshake nonces: a SUSI_PONG_SIG
                    // must echo one to prove the peer holds
                    // `~/.susi/cluster.key`. A short window is kept — a
                    // directed ping to a gossip-learned peer can legit
                    // answer a cycle late.
                    let mut pending_nonces: std::collections::VecDeque<String> =
                        std::collections::VecDeque::new();
                    // Roster write throttle — see the persist call below.
                    let mut last_persist = std::time::Instant::now();
                    // Ban-list re-read throttle — see the sweep below.
                    let mut last_ban_check = std::time::Instant::now();
                    // Persisted-roster rehydrate throttle — see below.
                    let mut last_rehydrate = std::time::Instant::now();
                    // Commit-ledger anti-entropy throttle — see below.
                    // Back-dated so the first sweep fires ~30s in — just
                    // after the persisted-roster rehydrate populates the
                    // live roster — rather than 5 min after boot. A node
                    // rejoining after downtime should catch up on the
                    // ledger (and committed membership) quickly, not
                    // wait out a full interval.
                    let mut last_commit_sync =
                        std::time::Instant::now() - std::time::Duration::from_secs(270);

                    loop {
                        let registry_checksum =
                            susi_gawd_agents::agents::AgentMetaRegistry::global().get_checksum();
                        if registry_checksum != last_registry_checksum {
                            local_bloom = CapabilityBloom::local_snapshot();
                            last_registry_checksum = registry_checksum;
                        }

                        // Liveness sweep first so it runs even on the signed-pong
                        // fast path (which `continue`s before the ping sends):
                        // a peer silent for PEER_STALE_SECS loses quorum weight
                        // and election eligibility until it re-verifies.
                        let now = now_secs();
                        let mut peers = t_shared.write();
                        for p in peers.iter_mut() {
                            if p.is_stale(now) {
                                p.is_active = false;
                            }
                        }
                        // Operator eviction applies to the live roster too —
                        // re-read the ban file every ~10s (not every probe)
                        // and drop banned members promptly.
                        if last_ban_check.elapsed().as_secs() >= 10 {
                            let banned = peer_registry::load_banned_peers();
                            if !banned.is_empty() {
                                peers.retain(|p| {
                                    matches!(p.admission, PeerAdmission::Local)
                                        || !banned.iter().any(|b| {
                                            b.node_id == p.node_id || b.address == p.address
                                        })
                                });
                            }
                            last_ban_check = std::time::Instant::now();
                        }
                        // Operator `peers add` lands on disk mid-flight —
                        // rehydrate persisted Explicit members into the
                        // live roster every ~15s so an add takes effect
                        // without a daemon restart, including cross-subnet
                        // where broadcast never reaches. Existing rows keep
                        // their live state; a live Discovered row upgrades.
                        if last_rehydrate.elapsed().as_secs() >= 15 {
                            for saved in peer_registry::load_persisted_peers() {
                                let looped = saved
                                    .address
                                    .split(':')
                                    .next()
                                    .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                                    .is_some_and(|ip| ip.is_loopback());
                                if looped {
                                    continue;
                                }
                                match peers.iter_mut().find(|p| p.address == saved.address) {
                                    Some(p) => {
                                        if matches!(p.admission, PeerAdmission::Discovered) {
                                            p.admission = PeerAdmission::Explicit;
                                        }
                                    }
                                    None => peers.push(saved),
                                }
                            }
                            last_rehydrate = std::time::Instant::now();
                        }
                        // Commit-ledger anti-entropy (Raft's periodic
                        // AppendEntries): every ~5 min pull each live
                        // Explicit peer's ledger and append what we're
                        // missing — a node offline during pushes
                        // self-heals instead of waiting for a manual
                        // `commits sync`. Membership records ride the
                        // same pull: committed roster deltas apply on
                        // append, so membership converges too.
                        let mut sync_addrs: Vec<String> = Vec::new();
                        if last_commit_sync.elapsed().as_secs() >= 300 {
                            sync_addrs = peers
                                .iter()
                                .filter(|p| {
                                    p.is_active && matches!(p.admission, PeerAdmission::Explicit)
                                })
                                .map(|p| p.address.clone())
                                .collect();
                            last_commit_sync = std::time::Instant::now();
                        }
                        drop(peers);
                        // Network I/O happens outside the roster lock.
                        for addr in sync_addrs {
                            Self::sync_commit_ledger_from(&addr);
                        }

                        let ping_msg = format!(
                            "SUSI_PING:{}:{}:{}",
                            local_caps,
                            registry_checksum,
                            local_bloom.to_hex()
                        );

                        if let Ok((amt, src)) = socket.recv_from(&mut buf) {
                            let msg = String::from_utf8_lossy(&buf[..amt]);
                            // Signed pong: peer proved it holds cluster.key and
                            // echoed our nonce — promote to Explicit + persist.
                            let verified = pending_nonces.iter().find_map(|nonce| {
                                crate::susi_config::cluster_key::verify_signed_pong(&msg, nonce)
                            });
                            if let Some((node_id, checksum, bloom_hex, roster)) = verified {
                                {
                                    // Loopback is never a peer: only one
                                    // daemon can bind 9092 on a host, so a
                                    // loopback pong is always our own —
                                    // admitting it would let mission
                                    // dispatch recurse into ourselves.
                                    if src.ip().is_loopback() {
                                        continue;
                                    }
                                    let addr_str = format!(
                                        "{}:{}",
                                        src.ip(),
                                        crate::susi_paths::ports::GMCP_HTTP
                                    );
                                    // Operator eviction: a banned member's
                                    // handshake is cryptographically valid
                                    // but must not re-enter the roster.
                                    if peer_registry::is_banned(&node_id, &addr_str) {
                                        continue;
                                    }
                                    let peer_bloom = CapabilityBloom::from_hex(&bloom_hex);
                                    let mut peers = t_shared.write();
                                    let (entry, is_new, admission_changed) = if let Some(p) =
                                        peers.iter_mut().find(|p| p.address == addr_str)
                                    {
                                        let changed =
                                            !matches!(p.admission, PeerAdmission::Explicit);
                                        p.trust_score = (p.trust_score + 0.05).min(1.0);
                                        p.is_active = true;
                                        p.registry_checksum = checksum;
                                        p.capability_bloom = peer_bloom;
                                        p.admission = PeerAdmission::Explicit;
                                        p.last_seen_secs = now_secs();
                                        (p.clone(), false, changed)
                                    } else {
                                        let node = ClusterPeerNode {
                                            node_id,
                                            address: addr_str,
                                            node_type: "PEER".into(),
                                            is_active: true,
                                            capabilities: vec!["CORE".into()],
                                            registry_checksum: checksum,
                                            latency_ms: 0,
                                            uptime_secs: 0,
                                            trust_score: 0.8,
                                            capability_bloom: peer_bloom,
                                            // Cluster-key handshake verified —
                                            // may receive the host bearer.
                                            admission: PeerAdmission::Explicit,
                                            last_seen_secs: now_secs(),
                                        };
                                        peers.push(node.clone());
                                        (node, true, false)
                                    };
                                    drop(peers);
                                    // Persist immediately for a new member or
                                    // an admission upgrade; throttle refresh
                                    // writes — a pong every ~600ms must not
                                    // rewrite the roster each time.
                                    const PERSIST_INTERVAL_SECS: u64 = 60;
                                    if is_new
                                        || admission_changed
                                        || last_persist.elapsed().as_secs() >= PERSIST_INTERVAL_SECS
                                    {
                                        peer_registry::persist_verified_peer(&entry);
                                        last_persist = std::time::Instant::now();
                                    }
                                    // Roster gossip: the responder vouches
                                    // for its own verified members — learn
                                    // them as Discovered and let the
                                    // directed signed-ping sweep below
                                    // promote the ones that verify for us.
                                    // Never downgrades an existing row and
                                    // never persists an unverified entry.
                                    if !roster.is_empty() {
                                        let mut peers = t_shared.write();
                                        for (gid, gaddr) in roster {
                                            let banned = peer_registry::is_banned(&gid, &gaddr);
                                            let known = peers
                                                .iter()
                                                .any(|p| p.node_id == gid || p.address == gaddr);
                                            let looped = gaddr
                                                .split(':')
                                                .next()
                                                .and_then(|h| h.parse::<std::net::IpAddr>().ok())
                                                .is_some_and(|ip| ip.is_loopback());
                                            if banned || known || looped {
                                                continue;
                                            }
                                            peers.push(ClusterPeerNode {
                                                node_id: gid,
                                                address: gaddr,
                                                node_type: "PEER".into(),
                                                is_active: true,
                                                capabilities: vec!["CORE".into()],
                                                registry_checksum: 0,
                                                latency_ms: 0,
                                                uptime_secs: 0,
                                                trust_score: 0.5,
                                                capability_bloom: CapabilityBloom::default(),
                                                // Vouched by a member, not yet
                                                // verified by us — the directed
                                                // ping sweep upgrades it.
                                                admission: PeerAdmission::Discovered,
                                                last_seen_secs: now_secs(),
                                            });
                                        }
                                    }
                                    continue;
                                }
                            }
                            // Scouts never answer discovery — that is the daemon's job on 9092.
                            if msg.starts_with("SUSI_PONG")
                                || msg.starts_with("SUSI_LAN_PONG")
                                || msg.starts_with("SUSI_PING")
                            {
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
                                let addr_str =
                                    format!("{}:{}", src.ip(), crate::susi_paths::ports::GMCP_HTTP);
                                if let Some(p) = peers.iter_mut().find(|p| p.address == addr_str) {
                                    p.trust_score = (p.trust_score + 0.05).min(1.0);
                                    p.is_active = true;
                                    p.registry_checksum = checksum;
                                    p.capability_bloom = peer_bloom;
                                    p.last_seen_secs = now_secs();
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
                                        // UDP answers are unauthenticated — never present
                                        // the host bearer token to these addresses.
                                        admission: PeerAdmission::Discovered,
                                        last_seen_secs: now_secs(),
                                    });
                                }
                            }
                        }

                        let _ = socket
                            .send_to(ping_msg.as_bytes(), format!("255.255.255.255:{}", port));
                        // Cluster-key handshake (VC-200-001): only nodes that
                        // can HMAC-sign a pong echoing this nonce may become
                        // Explicit roster members. The signed ping also goes
                        // directly to every Discovered peer's discovery port —
                        // LAN broadcast cannot cross subnets, and a
                        // gossip-learned member only earns Explicit by
                        // answering a handshake aimed at it.
                        if let Some((signed, nonce)) = crate::susi_config::cluster_key::signed_ping(
                            &local_caps,
                            registry_checksum,
                            &local_bloom.to_hex(),
                        ) {
                            pending_nonces.push_back(nonce);
                            while pending_nonces.len() > 4 {
                                pending_nonces.pop_front();
                            }
                            let _ = socket
                                .send_to(signed.as_bytes(), format!("255.255.255.255:{}", port));
                            let targets: Vec<String> = t_shared
                                .read()
                                .iter()
                                .filter(|p| matches!(p.admission, PeerAdmission::Discovered))
                                .filter_map(|p| {
                                    p.address.split(':').next().map(|h| {
                                        format!("{h}:{}", crate::susi_paths::ports::UDP_DISCOVERY)
                                    })
                                })
                                .take(32)
                                .collect();
                            for target in targets {
                                let _ = socket.send_to(signed.as_bytes(), &target);
                            }
                        }
                        // Also speak the daemon's LAN ping dialect so host discovery works.
                        let _ =
                            socket.send_to(b"SUSI_LAN_PING", format!("255.255.255.255:{}", port));
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            });

            shared
        });

        peers_lock.read().clone()
    }

    /// Text alone cannot earn trust. Explicit execution failures may lower rank;
    /// positive rewards require independent evidence not supplied by this API.
    fn rank_delta_for_output(output: &str) -> (f32, &'static str) {
        if susi_gawd_agents::accountability::is_failure(output) {
            (-0.05, "MISSION_FAILURE")
        } else {
            (0.0, "UNVERIFIED_OUTPUT")
        }
    }

    fn reported_success_ratio(logs: &[A2AMessage], fleet: &[GawdAgentInfo]) -> f32 {
        let names: std::collections::HashSet<_> =
            fleet.iter().map(|agent| agent.name.as_str()).collect();
        if names.is_empty() {
            return 0.0;
        }
        let successes = names
            .iter()
            .filter(|name| {
                logs.iter()
                    .rev()
                    .find(|log| log.sender == **name && log.action == "MISSION_FLUX")
                    .is_some_and(|log| susi_gawd_agents::accountability::is_usable(&log.payload))
            })
            .count();
        successes as f32 / names.len() as f32
    }

    /// VC-200-001 (roadmap.json) quorum-commit primitive: when a strict
    /// majority of the **pinned electorate** — the voter set snapshotted at
    /// broadcast time (local fleet names plus `PeerNode_<id>` keys for every
    /// Explicit peer actually dispatched, see step 3 of `supervise_mission`)
    /// — agrees on the same normalized output, commit that value directly
    /// rather than deferring to a single rank leader or LLM re-synthesis.
    ///
    /// Pinning the electorate is the Raft configuration-entry concept applied
    /// to a single consensus round: the quorum threshold is computed against
    /// *who was sent the mission*, not *who happened to respond*. That is
    /// what makes the vote safe under mid-vote churn — a peer that dies
    /// between dispatch and counting shrinks the response set instead of
    /// silently lowering the bar, and a blackboard entry from outside the
    /// pinned set can never inflate a "majority". Exact-match agreement after
    /// trimming/whitespace/case normalization is deliberately strict (no
    /// semantic similarity) so a "majority" can't be claimed from outputs
    /// that merely look similar.
    ///
    /// Still intentionally not a full Raft/Paxos protocol: the electorate is
    /// pinned per mission rather than via replicated configuration log
    /// entries, there is no leader election, and committed values are not
    /// replicated back to voters — so a vote cannot be recovered if the
    /// coordinator itself dies mid-round.
    fn quorum_majority(
        electorate: &std::collections::BTreeSet<String>,
        valid_outputs: &[(String, String)],
    ) -> Option<(String, usize)> {
        if electorate.len() < 2 {
            return None;
        }
        let mut counts: std::collections::HashMap<String, (usize, &str)> =
            std::collections::HashMap::new();
        for (voter, output) in valid_outputs {
            // Churn guard: only pinned members vote. A peer admitted after
            // the broadcast (or a blackboard key no mission dispatched) can
            // never tilt the count.
            if !electorate.contains(voter) {
                continue;
            }
            let trimmed = output.trim();
            let normalized = trimmed
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            // Representative is the trimmed (but case-preserved) text, not the
            // raw first-seen string, so a majority commit never surfaces
            // stray leading/trailing whitespace just because whichever
            // agent/peer happened to be inserted first had some.
            let entry = counts.entry(normalized).or_insert((0, trimmed));
            entry.0 += 1;
        }
        // Threshold is against the electorate, not the respondents: with 5
        // dispatched voters, 2 agreeing respondents are a plurality, not a
        // quorum.
        let quorum_threshold = electorate.len() / 2 + 1;
        counts
            .into_values()
            .filter(|(count, _)| *count >= quorum_threshold)
            .max_by_key(|(count, _)| *count)
            .map(|(tally, representative)| (representative.to_string(), tally))
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
            Arc::new(susi_gawd_agents::agents::HighDensityContextStore::new(1024));

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

        // 3. Broadcast the goal only to Explicitly admitted peers. UDP-
        // Discovered peers must not feed the mission blackboard / quorum —
        // a spoofed LAN pong would otherwise skew QUORUM_COMMIT without the
        // host bearer.
        let cluster_nodes = Self::rank_peers_for_goal(goal);
        // Pin this mission's electorate BEFORE dispatch (Raft-style
        // configuration entry for one consensus round): local fleet agent
        // names + the blackboard keys of every Explicit peer actually
        // dispatched. quorum_majority counts votes only from this set and
        // thresholds against its full size, so peer churn mid-vote shrinks
        // the response set instead of corrupting the quorum bar.
        let mut electorate: std::collections::BTreeSet<String> =
            fleet_info.iter().map(|i| i.name.clone()).collect();
        let dispatched_peers: Vec<&ClusterPeerNode> = cluster_nodes
            .iter()
            .filter(|n| n.is_active && matches!(n.admission, PeerAdmission::Explicit))
            .collect();
        for node in &dispatched_peers {
            electorate.insert(format!("PeerNode_{}", node.node_id));
        }
        let active_peers_count = dispatched_peers.len();
        if active_peers_count > 0 {
            eprintln!("- [Distributed Swarm] Broadcasting mission intent to {} explicitly admitted peer nodes...", active_peers_count);
            let _ = std::io::stdout().flush();
            for node in &dispatched_peers {
                let addr = node.address.clone();
                let node_id = node.node_id.clone();
                let g = goal.to_string();
                let bb = Arc::clone(&blackboard);
                rayon::spawn(move || {
                    let remote_res = Self::dispatch_peer_task(&addr, "reason", &g);
                    // Only genuine peer outputs may vote: transport
                    // failures ("unreachable") and tool-level refusals
                    // ("[A2A Error") are excluded — an error string is
                    // not an opinion and must never reach quorum.
                    if !remote_res.contains("unreachable") && !remote_res.starts_with("[A2A Error")
                    {
                        bb.insert(format!("PeerNode_{}", node_id), remote_res);
                    }
                });
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

        if let Some(veto) = a2a_logs
            .iter()
            .find(|log| log.payload.contains("[GOVERNANCE_BLOCK]"))
        {
            let payload = veto.payload.clone();
            a2a_logs.push(A2AMessage {
                sender: "ConsensusMaster".into(),
                recipient: "SUSI-Master".into(),
                action: "GOVERNANCE_BLOCK".into(),
                payload,
            });
            return (a2a_logs, fleet_info);
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
                    if susi_gawd_agents::accountability::is_usable(output) {
                        weighted_wisdom.push_str(&format!(
                            "[AGENT: {} (Rank: {:.2})] {}\n",
                            agent_name, info.rank, output
                        ));
                    }

                    // Empirical Expertise Ranking: reward success, penalize failure.
                    let (delta, source) = Self::rank_delta_for_output(output);
                    if delta != 0.0 {
                        susi_gawd_agents::agents::AgentMetaRegistry::global()
                            .update_rank(agent_name, delta, source);
                    }
                }
            }

            let is_query = GawdAgentFleet::is_meta_or_simple_query(goal);
            let is_direct_synthesis = is_query;

            let valid_outputs: Vec<(String, String)> = blackboard
                .iter()
                .filter_map(|r| {
                    let agent_name = r.key().clone();
                    let output = r.value().trim().to_string();
                    if susi_gawd_agents::accountability::is_usable(&output) {
                        Some((agent_name, output))
                    } else {
                        None
                    }
                })
                .collect();

            // Consensus Hardening: quorum-commit first (strict majority of
            // the electorate pinned at broadcast), then direct pass-through,
            // then rank-leader, then LLM re-synthesis.
            let (synthesized, convergence_action) = if is_direct_synthesis
                || valid_outputs.len() <= 1
            {
                let out = if valid_outputs.len() == 1 {
                    valid_outputs[0].1.clone()
                } else {
                    weighted_wisdom.clone()
                };
                (out, "STATE_CONVERGENCE")
            } else if let Some((quorum_output, tally)) =
                Self::quorum_majority(&electorate, &valid_outputs)
            {
                eprintln!(
                    "- [Consensus Master] Quorum reached: majority of {} pinned voters agree on the same output.",
                    electorate.len()
                );
                let _ = std::io::stdout().flush();
                // VC-200-001 log replication: seal the decision with the
                // cluster key, persist locally, and push it to every peer
                // that voted — a coordinator crash no longer loses the
                // committed value, and each voter holds an auditable copy.
                if let Some(record) = Self::elect_leader(&cluster_nodes).and_then(|leader| {
                    // Term semantics (VC-200-001): claim leadership before
                    // sealing — a leader transition bumps the persisted
                    // cluster term, and the record stamps the current term
                    // so receivers can reject stale-term coordinators. A
                    // node with no verified electorate (elect_leader →
                    // None) seals nothing: a commit record with a vacant
                    // leader field is not a quorum decision.
                    crate::susi_core::commit_log::claim_leadership(&leader);
                    let our_id = crate::susi_config::cluster_key::wire_node_id();
                    crate::susi_core::commit_log::CommitRecord::seal(
                        crate::susi_core::commit_log::CommitInput {
                            coordinator: &our_id,
                            leader: &leader,
                            electorate: electorate.iter().cloned().collect(),
                            tally,
                            quorum_threshold: electorate.len() / 2 + 1,
                            value: &quorum_output,
                        },
                    )
                }) {
                    if let Err(e) = crate::susi_core::commit_log::append(&record) {
                        eprintln!("- [Consensus Master] commit ledger append failed: {e}");
                    }
                    for node in &dispatched_peers {
                        let addr = node.address.clone();
                        let body = serde_json::to_string(&record).unwrap_or_default();
                        rayon::spawn(move || {
                            // Best-effort: an unreachable voter just misses
                            // this entry — the ledger is a recovery aid, not
                            // the commit itself. A tool-level refusal
                            // (stale term, chain divergence) is consensus-
                            // relevant though — log it, never swallow it.
                            let res = Self::dispatch_peer_task(&addr, "commit_record", &body);
                            if res.starts_with("[A2A Error") || res.contains("unreachable") {
                                eprintln!(
                                    "- [Consensus Master] commit push to {addr} failed: {res}"
                                );
                            }
                        });
                    }
                }
                (quorum_output, "QUORUM_COMMIT")
            } else if let Some(leader_output) =
                Self::dominant_rank_leader(&valid_outputs, &fleet_info)
            {
                (leader_output.to_string(), "STATE_CONVERGENCE")
            } else {
                let prompts = crate::susi_sandbox::manager::SusiPrompts::load_global();
                let consensus_prompt = prompts
                    .consensus_wisdom_prompt()
                    .replace("{goal}", goal)
                    .replace("{wisdom}", &weighted_wisdom);
                eprintln!(
                    "- [Consensus Master] Synthesizing swarm wisdom across {} active agents...",
                    valid_outputs.len()
                );
                let _ = std::io::stdout().flush();
                let out = crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_stream(
                    &consensus_prompt,
                    workspace,
                    &|token| {
                        print!("{}", token);
                        let _ = std::io::stdout().flush();
                    },
                );
                (out, "STATE_CONVERGENCE")
            };

            // Measures reported execution outcomes, never factual accuracy.
            let agent_success_ratio = Self::reported_success_ratio(&a2a_logs, &fleet_info);

            let final_payload = format!(
                "{}\n\n[AGENT_SUCCESS_RATIO: {:.2}]",
                synthesized, agent_success_ratio
            );

            a2a_logs.push(A2AMessage {
                sender: "ConsensusMaster".into(),
                recipient: "SUSI-Master".into(),
                action: convergence_action.into(),
                payload: final_payload,
            });
        }

        // Glass-box: persist the live mission blackboard (Tier S USP).
        let _ = blackboard.persist_inspectable(workspace);

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
                let _ = crate::host_hooks::hooks().audit_distillation_state(&ws);
            });
        }

        (a2a_logs, fleet_info)
    }

    pub fn gather_weighted_wisdom(
        interactions: &[A2AMessage],
        _agents: &[GawdAgentInfo],
    ) -> String {
        if let Some(veto) = interactions
            .iter()
            .find(|log| log.payload.contains("[GOVERNANCE_BLOCK]"))
        {
            return veto.payload.clone();
        }
        // Prioritize ConsensusMaster and AdminAgent results over other agents'
        let mut consensus_result = None;
        let mut admin_result = None;
        let mut wisdom = Vec::new();

        for msg in interactions {
            if !susi_gawd_agents::accountability::is_usable(&msg.payload) {
                continue;
            }
            if msg.sender == "ConsensusMaster" {
                consensus_result = Some(msg.payload.clone());
            } else if msg.sender == "AdminAgent" {
                admin_result = Some(msg.payload.clone());
            }
            wisdom.push(format!("[{}]: {}", msg.sender, msg.payload));
        }

        consensus_result.or(admin_result).unwrap_or_else(|| {
            if wisdom.is_empty() {
                "No valid wisdom gathered from swarm.".to_string()
            } else {
                wisdom.join("\n")
            }
        })
    }

    /// Deterministic leader election over the verified roster (bully
    /// algorithm): the active `Local`/`Explicit` node with the highest
    /// trust score wins, ties broken by lexicographic `node_id`. Every
    /// member computing this over the same roster converges on the same
    /// leader without an election round-trip — appropriate for a cluster
    /// whose membership is already cluster-key authenticated. `Discovered`
    /// and inactive nodes can never lead.
    ///
    /// The leader is the canonical coordinator for cluster-level state:
    /// commit records stamp who the coordinator believed the leader was,
    /// so receivers auditing `~/.susi/commit_log.jsonl` can flag decisions
    /// that came from a non-leader as anomalies worth investigating.
    pub fn elect_leader(nodes: &[ClusterPeerNode]) -> Option<String> {
        // Staleness is checked here as well as in the scout sweep — a peer
        // that died between sweeps must not lead a mission's quorum round.
        let now = now_secs();
        nodes
            .iter()
            .filter(|n| {
                n.is_active
                    && !n.is_stale(now)
                    && matches!(n.admission, PeerAdmission::Local | PeerAdmission::Explicit)
            })
            .max_by(|a, b| {
                a.trust_score
                    .partial_cmp(&b.trust_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.node_id.cmp(&b.node_id))
            })
            .map(|n| n.node_id.clone())
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

    /// Whether outbound calls to `addr` may attach this host's bearer token.
    /// Only roster peers admitted as Local or Explicit qualify — never
    /// Discovered peers, and never an arbitrary loopback port (a local
    /// listener could otherwise steal the token).
    fn peer_allows_host_token(addr: &str) -> bool {
        let addr = addr.trim();
        Self::list_cluster_nodes().iter().any(|n| {
            n.address == addr
                && matches!(n.admission, PeerAdmission::Local | PeerAdmission::Explicit)
        })
    }

    /// Invoke a governed tool on a verified peer over the MCP Streamable
    /// HTTP channel (`susi_core::mcp_client` runs the full session
    /// handshake — a bare POST to `/` is a 404 and was silently swallowed
    /// by the old implementation).
    pub fn dispatch_peer_task(addr: &str, tool_name: &str, arg: &str) -> String {
        let arg_val = serde_json::from_str(arg).unwrap_or(serde_json::json!(arg));

        // Only Local / Explicit peers may receive the host bearer. UDP-
        // discovered peers stay unauthenticated on purpose: a fleet that
        // needs mutual auth must admit peers via an explicit allowlist
        // (PeerAdmission::Explicit), not via an open LAN ping.
        let bearer = if Self::peer_allows_host_token(addr) {
            let token = crate::susi_sandbox::manager::SusiConfig::load_global()
                .unwrap_or_default()
                .api_auth_token();
            (!token.is_empty()).then_some(token)
        } else {
            None
        };

        match crate::susi_core::mcp_client::call_tool(addr, tool_name, &arg_val, bearer.as_deref())
        {
            Ok(result) => {
                let text = result
                    .pointer("/content/0/text")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| result.to_string());
                // MCP transport success ≠ tool success: `isError` marks a
                // tool-level refusal (bad args, stale term, chain
                // divergence). It must be labeled as an error — a Flux-
                // labeled error would be inserted as a peer output and
                // could vote in quorum.
                if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
                    format!("[A2A Error ({})]: {}", addr, text.trim())
                } else {
                    format!("[A2A Flux ({})]: {}", addr, text.trim())
                }
            }
            Err(e) => format!("[A2A Fallback]: Node '{}' unreachable ({e}).", addr),
        }
    }

    /// Pull one peer's commit ledger and append any records we're
    /// missing — the periodic anti-entropy half of replication. Paginates
    /// like `commits sync`; each record is re-verified before append,
    /// and member records apply their roster delta on append. History
    /// fills bypass the term gate (terms gate new writes, not the log's
    /// past).
    fn sync_commit_ledger_from(addr: &str) {
        let token = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .api_auth_token();
        let bearer = (!token.is_empty()).then_some(token);
        let mut held: std::collections::HashSet<String> = crate::susi_core::commit_log::load()
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .collect();
        let mut offset = 0usize;
        // The full remote set is retained for the push half of the
        // exchange — records we hold that the peer lacks get pushed
        // back, so convergence doesn't depend on the peer also running
        // a sweep (mixed-version clusters still heal).
        let mut their_records: Vec<crate::susi_core::commit_log::CommitRecord> = Vec::new();
        loop {
            let Ok(result) = crate::susi_core::mcp_client::call_tool(
                addr,
                "commit_log_fetch",
                &serde_json::json!({ "limit": 1000, "offset": offset }),
                bearer.as_deref(),
            ) else {
                return;
            };
            if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
                return;
            }
            let Some(text) = result.pointer("/content/0/text").and_then(|t| t.as_str()) else {
                return;
            };
            let Ok(records) =
                serde_json::from_str::<Vec<crate::susi_core::commit_log::CommitRecord>>(text)
            else {
                return;
            };
            let page = records.len();
            offset += page;
            their_records.extend(records);
            if page < 1000 {
                break;
            }
        }
        let theirs: std::collections::HashSet<String> = their_records
            .iter()
            .filter_map(|r| serde_json::to_string(r).ok())
            .collect();
        for r in &their_records {
            if !r.verify() {
                continue;
            }
            let key = serde_json::to_string(&r).unwrap_or_default();
            if held.contains(&key) {
                continue;
            }
            if crate::susi_core::commit_log::append(r).is_ok() {
                held.insert(key);
            }
        }
        // Symmetric repair: push our records the peer lacks through
        // `commit_record` — their receive path re-runs signature, term,
        // sequence, and chain gates, so a rejection is the protocol's
        // gate working, not a sync failure.
        for r in crate::susi_core::commit_log::load() {
            let key = serde_json::to_string(&r).unwrap_or_default();
            if theirs.contains(&key) {
                continue;
            }
            // The tool's args ARE the record — commit_record deserializes
            // the argument object directly into CommitRecord.
            let Ok(args) = serde_json::to_value(&r) else {
                continue;
            };
            let _ = crate::susi_core::mcp_client::call_tool(
                addr,
                "commit_record",
                &args,
                bearer.as_deref(),
            );
        }
    }

    pub fn broadcast_lock_request(resource_id: &str) -> bool {
        let nodes = Self::list_cluster_nodes();
        use rayon::prelude::*;

        let target_nodes: Vec<_> = nodes
            .into_iter()
            .filter(|n| !matches!(n.admission, PeerAdmission::Local))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_aspiration_23_universal_swarm_operation() {
        use susi_gawd_agents::agents::{
            GawdAgent, HighDensityContextStore, SafetyAgent, SecurityAgent,
        };
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

    fn electorate(names: &[&str]) -> std::collections::BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_quorum_majority_commits_on_agreement() {
        let outputs = vec![
            ("AgentA".to_string(), "  The answer is 42.  ".to_string()),
            ("PeerNode_1".to_string(), "the answer is 42.".to_string()),
            (
                "AgentB".to_string(),
                "something totally different".to_string(),
            ),
        ];
        let voters = electorate(&["AgentA", "PeerNode_1", "AgentB"]);
        let quorum = SusiSupervisor::quorum_majority(&voters, &outputs);
        assert_eq!(quorum, Some(("The answer is 42.".to_string(), 2)));
    }

    #[test]
    fn test_quorum_majority_none_without_majority() {
        let outputs = vec![
            ("AgentA".to_string(), "answer one".to_string()),
            ("AgentB".to_string(), "answer two".to_string()),
            ("AgentC".to_string(), "answer three".to_string()),
        ];
        let voters = electorate(&["AgentA", "AgentB", "AgentC"]);
        assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);
    }

    #[test]
    fn test_quorum_majority_requires_at_least_two_outputs() {
        let outputs = vec![("AgentA".to_string(), "solo answer".to_string())];
        let voters = electorate(&["AgentA"]);
        assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);
    }

    #[test]
    fn test_quorum_majority_thresholds_against_pinned_electorate_not_respondents() {
        // 5 voters were dispatched; 2 responded and agree. Under the old
        // respondent-threshold rule that claimed a "quorum" of 40% of the
        // voter set — the mid-vote-churn hole. Pinned electorate: 2 < 3.
        let outputs = vec![
            ("AgentA".to_string(), "agreed".to_string()),
            ("PeerNode_1".to_string(), "agreed".to_string()),
        ];
        let voters = electorate(&["AgentA", "AgentB", "AgentC", "PeerNode_1", "PeerNode_2"]);
        assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);

        // 3 of the same 5 agree → real quorum.
        let mut three = outputs.clone();
        three.push(("AgentB".to_string(), "agreed".to_string()));
        assert_eq!(
            SusiSupervisor::quorum_majority(&voters, &three),
            Some(("agreed".to_string(), 3))
        );
    }

    #[test]
    fn test_quorum_majority_ignores_votes_outside_the_pinned_electorate() {
        // A peer verified AFTER the broadcast was never dispatched; its
        // blackboard entry (or any stray key) must not tilt the count.
        let outputs = vec![
            ("AgentA".to_string(), "late-joiner answer".to_string()),
            (
                "PeerNode_late".to_string(),
                "late-joiner answer".to_string(),
            ),
            ("AgentB".to_string(), "other".to_string()),
        ];
        let voters = electorate(&["AgentA", "AgentB"]);
        assert_eq!(SusiSupervisor::quorum_majority(&voters, &outputs), None);
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

    fn peer_node(id: &str, trust: f32, active: bool, admission: PeerAdmission) -> ClusterPeerNode {
        ClusterPeerNode {
            node_id: id.into(),
            address: format!("10.0.0.1:9{id:0>3}"),
            node_type: "PEER".into(),
            is_active: active,
            capabilities: vec!["CORE".into()],
            registry_checksum: 0,
            latency_ms: 0,
            uptime_secs: 0,
            trust_score: trust,
            capability_bloom: CapabilityBloom::default(),
            admission,
            last_seen_secs: now_secs(),
        }
    }

    #[test]
    fn test_elect_leader_picks_highest_trust_verified_node() {
        let roster = vec![
            peer_node("n1", 0.9, true, PeerAdmission::Explicit),
            peer_node("n2", 0.6, true, PeerAdmission::Explicit),
            peer_node("susi-local-master", 1.0, true, PeerAdmission::Local),
        ];
        assert_eq!(
            SusiSupervisor::elect_leader(&roster),
            Some("susi-local-master".to_string())
        );
    }

    #[test]
    fn test_elect_leader_deterministic_tie_break_and_exclusions() {
        // Equal trust: lexicographically greatest node_id wins — every
        // member computing over the same roster must converge.
        let roster = vec![
            peer_node("peer-b", 0.8, true, PeerAdmission::Explicit),
            peer_node("peer-a", 0.8, true, PeerAdmission::Explicit),
        ];
        assert_eq!(
            SusiSupervisor::elect_leader(&roster),
            Some("peer-b".to_string())
        );
        // Discovered and inactive nodes can never lead, however trusted.
        let untrusted = vec![
            peer_node("disc", 1.0, true, PeerAdmission::Discovered),
            peer_node("dead", 1.0, false, PeerAdmission::Explicit),
            peer_node("ok", 0.1, true, PeerAdmission::Explicit),
        ];
        assert_eq!(
            SusiSupervisor::elect_leader(&untrusted),
            Some("ok".to_string())
        );
        assert_eq!(SusiSupervisor::elect_leader(&[]), None);
    }

    #[test]
    fn test_elect_leader_skips_stale_peers() {
        // A peer whose last pong predates PEER_STALE_SECS cannot lead even
        // if its persisted is_active flag was never swept.
        let mut stale = peer_node("stale", 1.0, true, PeerAdmission::Explicit);
        stale.last_seen_secs = now_secs().saturating_sub(PEER_STALE_SECS + 1);
        let fresh = peer_node("fresh", 0.1, true, PeerAdmission::Explicit);
        assert_eq!(
            SusiSupervisor::elect_leader(&[stale.clone(), fresh]),
            Some("fresh".to_string())
        );
        // A roster of only stale non-local nodes elects nobody.
        assert_eq!(SusiSupervisor::elect_leader(&[stale]), None);
        // The local node is never stale — loopback is always live.
        let mut local = peer_node("local", 0.0, true, PeerAdmission::Local);
        local.last_seen_secs = 0;
        assert!(!local.is_stale(now_secs()));
        assert_eq!(
            SusiSupervisor::elect_leader(&[local]),
            Some("local".to_string())
        );
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
            admission: PeerAdmission::Discovered,
            last_seen_secs: now_secs(),
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
            admission: PeerAdmission::Discovered,
            last_seen_secs: now_secs(),
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
    fn discovered_peers_never_receive_host_bearer() {
        // Local master is rostered as PeerAdmission::Local on the GMCP port.
        let local = format!("127.0.0.1:{}", crate::susi_paths::ports::GMCP);
        assert!(SusiSupervisor::peer_allows_host_token(&local));
        // Arbitrary loopback ports are not automatic trust — a local listener
        // must not steal the host bearer just by binding nearby.
        assert!(!SusiSupervisor::peer_allows_host_token("127.0.0.1:19999"));
        assert!(!SusiSupervisor::peer_allows_host_token("localhost:9093"));
        // Any non-roster / Discovered address must be denied.
        assert!(!SusiSupervisor::peer_allows_host_token("10.0.0.99:9093"));
        assert!(!SusiSupervisor::peer_allows_host_token("192.168.1.50:9090"));
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
                if susi_gawd_agents::accountability::is_usable(&output) {
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
    fn test_unverified_text_does_not_increase_rank() {
        let (delta, source) = SusiSupervisor::rank_delta_for_output("Hardware Saturated: 8 CPUs.");
        assert_eq!(delta, 0.0);
        assert_eq!(source, "UNVERIFIED_OUTPUT");
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

    #[test]
    fn peer_scout_never_steals_host_contract_udp_port() {
        // While the host-contract discovery port is held (as the daemon would),
        // listing cluster nodes must still succeed — scouts bind ephemeral ports.
        // If the live daemon already owns 9092, that is the same precondition.
        let _holder = std::net::UdpSocket::bind(format!(
            "127.0.0.1:{}",
            crate::susi_paths::ports::UDP_DISCOVERY
        ))
        .ok();
        let nodes = SusiSupervisor::list_cluster_nodes();
        assert!(
            nodes
                .iter()
                .any(|n| matches!(n.admission, PeerAdmission::Local)),
            "local master must remain discoverable without binding 9092"
        );
    }
}
