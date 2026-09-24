//! Persistent verified-peer registry (`~/.susi/peers.json`).
//!
//! Only `Explicit` peers (cluster-key-verified) are ever persisted or
//! rehydrated — a `Discovered` entry written to disk would let a spoofed
//! pong survive restarts. Loading merges into the live roster in
//! `amas::SusiSupervisor::list_cluster_nodes`; saving happens on each
//! successful signed handshake.

use std::path::PathBuf;

use crate::amas::{ClusterPeerNode, PeerAdmission};

fn registry_path() -> PathBuf {
    crate::susi_paths::SusiDirs::config_dir().join("peers.json")
}

fn banned_path() -> PathBuf {
    crate::susi_paths::SusiDirs::config_dir().join("peers_banned.json")
}

/// Operator-evicted members. A banned peer's signed pong verifies
/// cryptographically but is dropped at admission — removal without a ban
/// list would let the evicted node re-verify on its next handshake.
/// The file format is structural JSON (`peers_banned.json`) that
/// `susi peers` reads/writes without a Cargo edge into this crate.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BannedPeer {
    pub node_id: String,
    pub address: String,
    pub banned_at: u64,
}

pub fn load_banned_peers() -> Vec<BannedPeer> {
    load_banned_peers_from(&banned_path())
}

/// Path-seamed loader — same rationale as `load_persisted_peers_from`.
pub fn load_banned_peers_from(path: &PathBuf) -> Vec<BannedPeer> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// True when `node_id` or `address` matches a banned member — the
/// signed-pong handler drops admissions for banned peers even though
/// the cryptographic handshake itself succeeds.
pub fn is_banned(node_id: &str, address: &str) -> bool {
    is_banned_in(&load_banned_peers(), node_id, address)
}

/// Pure membership check — the rule `susi peers remove` and this module
/// agree on: either field matching blocks admission.
pub fn is_banned_in(banned: &[BannedPeer], node_id: &str, address: &str) -> bool {
    banned
        .iter()
        .any(|b| b.node_id == node_id || b.address == address)
}

/// Evict `node`: record the ban and drop the roster entry. Banning is
/// what `susi peers remove` writes structurally; this function keeps the
/// swarm-side view consistent when called from inside the daemon.
pub fn ban_peer(node_id: &str, address: &str) {
    ban_peer_at(node_id, address, &banned_path(), &registry_path());
}

/// Path-seamed variant of `ban_peer`.
pub fn ban_peer_at(node_id: &str, address: &str, banned_path: &PathBuf, registry_path: &PathBuf) {
    // One lock across the ban+roster pair — a concurrent committed
    // member_add apply or scout persist must not interleave.
    let _lock = banned_path
        .parent()
        .and_then(|dir| crate::susi_core::commit_log::FileLock::acquire(dir, "peers"));
    let mut banned = load_banned_peers_from(banned_path);
    if !is_banned_in(&banned, node_id, address) {
        banned.push(BannedPeer {
            node_id: node_id.to_string(),
            address: address.to_string(),
            banned_at: crate::amas::now_secs(),
        });
        let _ = crate::susi_config::atomic_write_json_pretty(banned_path, &banned);
    }
    // Drop the roster entry so the ban takes effect immediately, not on
    // the next restart.
    let kept: Vec<_> = load_persisted_peers_from(registry_path)
        .into_iter()
        .filter(|n| n.node_id != node_id && n.address != address)
        .collect();
    let _ = crate::susi_config::atomic_write_json_pretty(registry_path, &kept);
}

/// Persisted verified peers — filtered to `Explicit` on read so a
/// hand-edited registry cannot smuggle in unverified admission.
pub fn load_persisted_peers() -> Vec<ClusterPeerNode> {
    load_persisted_peers_from(&registry_path())
}

/// Path-seamed loader — tests exercise the real filter logic against a
/// temp file without mutating process env (which races parallel tests
/// resolving the cluster key through the same variables).
pub fn load_persisted_peers_from(path: &PathBuf) -> Vec<ClusterPeerNode> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let nodes: Vec<ClusterPeerNode> = serde_json::from_str(&text).unwrap_or_default();
    nodes
        .into_iter()
        .filter(|n| matches!(n.admission, PeerAdmission::Explicit))
        .collect()
}

/// Insert or update `node` in the persisted registry. Only `Explicit`
/// peers are written; the call is a no-op otherwise.
pub fn persist_verified_peer(node: &ClusterPeerNode) {
    persist_verified_peer_to(node, &registry_path());
}

/// Path-seamed variant of `persist_verified_peer` — same rationale as
/// `load_persisted_peers_from`.
pub fn persist_verified_peer_to(node: &ClusterPeerNode, path: &PathBuf) {
    if !matches!(node.admission, PeerAdmission::Explicit) {
        return;
    }
    // Serialize the roster read-modify-write across processes — the CLI
    // (`peers add/remove`), committed-membership applies, and this scout
    // admission all mutate peers.json; an unlocked load→write clobbers.
    // Skipping on lock failure converges anyway — the peer re-verifies
    // on its next signed handshake.
    let _lock = path
        .parent()
        .and_then(|dir| crate::susi_core::commit_log::FileLock::acquire(dir, "peers"));
    let mut nodes = load_persisted_peers_from(path);
    // Same node re-homed to a new address: drop stale rows keyed by the
    // same node_id so one node occupies exactly one persisted slot.
    nodes.retain(|n| n.address == node.address || n.node_id != node.node_id);
    if let Some(existing) = nodes.iter_mut().find(|n| n.address == node.address) {
        *existing = node.clone();
    } else {
        nodes.push(node.clone());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::susi_config::atomic_write_json_pretty(path, &nodes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amas::CapabilityBloom;

    fn temp_registry() -> PathBuf {
        std::env::temp_dir().join(format!(
            "susi_peers_{}_{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn node(addr: &str, admission: PeerAdmission) -> ClusterPeerNode {
        ClusterPeerNode {
            node_id: format!("n-{addr}"),
            address: addr.to_string(),
            node_type: "PEER".into(),
            is_active: true,
            capabilities: vec!["CORE".into()],
            registry_checksum: 0,
            latency_ms: 0,
            uptime_secs: 0,
            trust_score: 0.8,
            capability_bloom: CapabilityBloom::default(),
            admission,
            last_seen_secs: crate::amas::now_secs(),
        }
    }

    #[test]
    fn persists_only_explicit_peers_and_rehydrates_them() {
        let path = temp_registry();
        persist_verified_peer_to(&node("10.0.0.1:9093", PeerAdmission::Explicit), &path);
        // A Discovered peer must never be written or rehydrated.
        persist_verified_peer_to(&node("10.0.0.9:9093", PeerAdmission::Discovered), &path);
        let loaded = load_persisted_peers_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].address, "10.0.0.1:9093");
        assert!(matches!(loaded[0].admission, PeerAdmission::Explicit));

        // A hand-edited registry that downgrades admission is dropped on read.
        let mut forged = node("10.0.0.2:9093", PeerAdmission::Explicit);
        forged.admission = PeerAdmission::Discovered;
        std::fs::write(&path, serde_json::to_string(&vec![forged]).unwrap()).unwrap();
        assert!(load_persisted_peers_from(&path).is_empty());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ban_peer_records_ban_and_drops_roster_entry() {
        let reg = temp_registry();
        let ban = temp_registry();
        let peer = node("10.0.0.5:9093", PeerAdmission::Explicit);
        persist_verified_peer_to(&peer, &reg);
        assert_eq!(load_persisted_peers_from(&reg).len(), 1);

        ban_peer_at(&peer.node_id, &peer.address, &ban, &reg);
        let banned = load_banned_peers_from(&ban);
        assert_eq!(banned.len(), 1);
        assert!(is_banned_in(&banned, &peer.node_id, "unrelated:1"));
        assert!(is_banned_in(&banned, "unrelated", &peer.address));
        assert!(!is_banned_in(&banned, "unrelated", "unrelated:1"));
        // The roster entry is dropped immediately — eviction is not deferred.
        assert!(load_persisted_peers_from(&reg).is_empty());

        // Banning twice does not duplicate the entry.
        ban_peer_at(&peer.node_id, &peer.address, &ban, &reg);
        assert_eq!(load_banned_peers_from(&ban).len(), 1);

        let _ = std::fs::remove_file(&reg);
        let _ = std::fs::remove_file(&ban);
    }

    #[test]
    fn rehomed_node_keeps_one_slot_and_verified_id_overwrites() {
        let reg = temp_registry();
        let mut old = node("10.0.0.6:9093", PeerAdmission::Explicit);
        old.node_id = "susi-node-rehome".to_string();
        persist_verified_peer_to(&old, &reg);

        // Same node_id verifies at a new address — the stale row goes,
        // one slot remains, keyed under the new address.
        let mut moved = node("192.168.1.20:9093", PeerAdmission::Explicit);
        moved.node_id = "susi-node-rehome".to_string();
        persist_verified_peer_to(&moved, &reg);
        let loaded = load_persisted_peers_from(&reg);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].address, "192.168.1.20:9093");
        assert_eq!(loaded[0].node_id, "susi-node-rehome");

        // An entry persisted under a synthetic LAN-discovery id takes the
        // verified node_id on the same-address upsert.
        let mut synthetic = node("192.168.1.20:9093", PeerAdmission::Explicit);
        synthetic.node_id = "susi-peer-192.168.1.20".to_string();
        persist_verified_peer_to(&synthetic, &reg);
        let mut verified = node("192.168.1.20:9093", PeerAdmission::Explicit);
        verified.node_id = "susi-node-verified".to_string();
        persist_verified_peer_to(&verified, &reg);
        let loaded = load_persisted_peers_from(&reg);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].node_id, "susi-node-verified");

        let _ = std::fs::remove_file(&reg);
    }
}
