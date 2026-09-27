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

/// True when `node_id` or `address` matches an operator-evicted member in
/// `peers_banned.json`. A banned peer's signed pong verifies
/// cryptographically but is dropped at admission. Fails closed: an
/// unreadable or corrupt ban list counts as banned, so damage to the file
/// can never readmit evicted members (it used to parse as an empty list).
pub fn is_banned(node_id: &str, address: &str) -> bool {
    crate::susi_config::cluster_key::member_banned(|row| {
        crate::susi_config::cluster_key::ban_row_matches(row, node_id, address)
    })
}

/// Persisted verified peers — filtered to `Explicit` on read so a
/// hand-edited registry cannot smuggle in unverified admission.
pub fn load_persisted_peers() -> Vec<ClusterPeerNode> {
    load_persisted_peers_from(&registry_path())
}

/// Path-seamed loader — tests exercise the real filter logic against a
/// temp file without mutating process env (which races parallel tests
/// resolving the cluster key through the same variables).
pub fn load_persisted_peers_from(path: &std::path::Path) -> Vec<ClusterPeerNode> {
    // Rows are parsed one by one: a single row another writer shaped
    // differently used to fail the whole array and hide every peer.
    crate::susi_config::cluster_key::read_json_rows_strict(path)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|row| serde_json::from_value::<ClusterPeerNode>(row).ok())
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
pub fn persist_verified_peer_to(node: &ClusterPeerNode, path: &std::path::Path) {
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
    // Edit JSON rows, not typed nodes: rewriting from a typed view dropped
    // every row that failed to type-parse, and a damaged file read as
    // empty was rewritten with this one peer, erasing the roster.
    let mut rows = match crate::susi_config::cluster_key::read_json_rows_strict(path) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!(
                "[peers] not persisting {}: roster unreadable ({e})",
                node.address
            );
            return;
        }
    };
    let Ok(fresh) = serde_json::to_value(node) else {
        return;
    };
    let field = |row: &serde_json::Value, key: &str| {
        row.get(key).and_then(|v| v.as_str()).map(str::to_string)
    };
    // Only verified rows are ever kept on disk (see module docs); the
    // same node re-homed to a new address keeps exactly one slot.
    rows.retain(|row| {
        field(row, "admission").as_deref() == Some("explicit")
            && (field(row, "address").as_deref() == Some(node.address.as_str())
                || field(row, "node_id").as_deref() != Some(node.node_id.as_str()))
    });
    if let Some(existing) = rows
        .iter_mut()
        .find(|row| field(row, "address").as_deref() == Some(node.address.as_str()))
    {
        *existing = fresh;
    } else {
        rows.push(fresh);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = crate::susi_config::atomic_write_json_pretty(path, &rows);
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
            pubkey: String::new(),
            key_bound_at: 0,
            bind_sig: String::new(),
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
    fn damaged_or_foreign_roster_rows_are_never_erased() {
        let reg = temp_registry();
        // A row another writer shaped without typed fields must survive a
        // persist, and must not hide the typed peers beside it.
        std::fs::write(
            &reg,
            r#"[{"node_id":"cli-row","address":"10.0.0.7:9093","admission":"explicit"}]"#,
        )
        .unwrap();
        persist_verified_peer_to(&node("10.0.0.8:9093", PeerAdmission::Explicit), &reg);
        let raw: Vec<serde_json::Value> =
            serde_json::from_str(&std::fs::read_to_string(&reg).unwrap()).unwrap();
        assert_eq!(raw.len(), 2);
        assert_eq!(load_persisted_peers_from(&reg).len(), 1);

        // A damaged roster is left alone rather than rewritten with one row.
        std::fs::write(&reg, "[{\"node_id\":").unwrap();
        persist_verified_peer_to(&node("10.0.0.9:9093", PeerAdmission::Explicit), &reg);
        assert_eq!(std::fs::read_to_string(&reg).unwrap(), "[{\"node_id\":");
        let _ = std::fs::remove_file(&reg);
    }

    #[test]
    fn damaged_ban_list_fails_closed() {
        use crate::susi_config::cluster_key::{ban_row_matches, member_banned_at};
        let dir = std::env::temp_dir().join(format!(
            "susi_bans_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let check = |id: &str, addr: &str| member_banned_at(&dir, |r| ban_row_matches(r, id, addr));
        // No ban list: nobody is banned.
        assert!(!check("n-a", "10.0.0.1:9093"));
        std::fs::write(
            dir.join("peers_banned.json"),
            r#"[{"node_id":"n-a","address":"10.0.0.1:9093","banned_at":1}]"#,
        )
        .unwrap();
        assert!(check("n-a", "elsewhere:1"));
        assert!(check("other", "10.0.0.1:9093"));
        assert!(!check("other", "elsewhere:1"));
        // A torn/corrupt list must not read as empty.
        std::fs::write(dir.join("peers_banned.json"), r#"[{"node_id":"n-a""#).unwrap();
        assert!(check("other", "elsewhere:1"));
        let _ = std::fs::remove_dir_all(&dir);
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
