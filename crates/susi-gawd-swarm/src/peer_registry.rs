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
    susi_paths::SusiDirs::config_dir().join("peers.json")
}

/// Persisted verified peers — filtered to `Explicit` on read so a
/// hand-edited registry cannot smuggle in unverified admission.
pub fn load_persisted_peers() -> Vec<ClusterPeerNode> {
    let path = registry_path();
    let text = match std::fs::read_to_string(&path) {
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
    if !matches!(node.admission, PeerAdmission::Explicit) {
        return;
    }
    let path = registry_path();
    let mut nodes = load_persisted_peers();
    if let Some(existing) = nodes.iter_mut().find(|n| n.address == node.address) {
        *existing = node.clone();
    } else {
        nodes.push(node.clone());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = susi_config::atomic_write_json_pretty(&path, &nodes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::amas::CapabilityBloom;

    fn temp_home() -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let tmp = std::env::temp_dir().join(format!(
            "susi_peers_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(tmp.join(".susi"));
        unsafe {
            std::env::set_var("HOME", &tmp);
            std::env::set_var("USERPROFILE", &tmp);
            std::env::set_var("XDG_CONFIG_HOME", tmp.join("xdg"));
        }
        (guard, tmp)
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
        }
    }

    #[test]
    fn persists_only_explicit_peers_and_rehydrates_them() {
        let (_guard, tmp) = temp_home();
        persist_verified_peer(&node("10.0.0.1:9093", PeerAdmission::Explicit));
        // A Discovered peer must never be written or rehydrated.
        persist_verified_peer(&node("10.0.0.9:9093", PeerAdmission::Discovered));
        let loaded = load_persisted_peers();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].address, "10.0.0.1:9093");
        assert!(matches!(loaded[0].admission, PeerAdmission::Explicit));

        // A hand-edited registry that downgrades admission is dropped on read.
        let mut forged = node("10.0.0.2:9093", PeerAdmission::Explicit);
        forged.admission = PeerAdmission::Discovered;
        std::fs::write(
            registry_path(),
            serde_json::to_string(&vec![forged]).unwrap(),
        )
        .unwrap();
        assert!(load_persisted_peers().is_empty());

        drop(_guard);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
