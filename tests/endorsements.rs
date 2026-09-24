#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Integration test for the endorsement gate (joint-consensus half):
//! once a majority of an electorate's members have bound pubkeys, a
//! privileged record (member/rekey kind) is admissible only when a
//! majority of the members bound at `committed_at` signed
//! `susi-endorse-v1:{signature}` — a lone leader can no longer rewrite
//! the roster. Pre-binding history still passes (nothing bound yet),
//! and foreign/duplicate/mis-scoped endorsements never count.

use ed25519_dalek::{Signer, SigningKey};
use std::path::PathBuf;
use susi_config::cluster_key;
use susi_core::commit_log::{self, CommitRecord, MemberEndorsement};

/// Env-seamed HOME/XDG; restores prior values on drop.
struct HomeGuard {
    tmp: PathBuf,
    prev_home: Option<std::ffi::OsString>,
    prev_userprofile: Option<std::ffi::OsString>,
    prev_xdg: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn new() -> Self {
        let tmp = std::env::temp_dir().join(format!(
            "susi_end_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".susi")).unwrap();
        let g = Self {
            prev_home: std::env::var_os("HOME"),
            prev_userprofile: std::env::var_os("USERPROFILE"),
            prev_xdg: std::env::var_os("XDG_CONFIG_HOME"),
            tmp,
        };
        std::env::set_var("HOME", &g.tmp);
        std::env::set_var("USERPROFILE", &g.tmp);
        std::env::set_var("XDG_CONFIG_HOME", g.tmp.join("xdg"));
        g
    }
    fn config(&self) -> PathBuf {
        self.tmp.join(".susi")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match &self.prev_userprofile {
            Some(v) => std::env::set_var("USERPROFILE", v),
            None => std::env::remove_var("USERPROFILE"),
        }
        match &self.prev_xdg {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn write_roster(dir: &std::path::Path, members: &[(&str, &str, &str)]) {
    let rows: Vec<serde_json::Value> = members
        .iter()
        .map(|(id, addr, pk)| {
            serde_json::json!({
                "node_id": id,
                "address": addr,
                "node_type": "PEER",
                "admission": "explicit",
                "pubkey": pk,
                "key_bound_at": 0u64,
            })
        })
        .collect();
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string_pretty(&rows).unwrap(),
    )
    .unwrap();
}

/// Sign `susi-endorse-v1:{record.signature}` under a foreign member key —
/// matching `member_sign`, which wraps the payload in `susi-member-v1:`.
fn endorse(record: &CommitRecord, node: &str, sk: &SigningKey) -> MemberEndorsement {
    let payload = commit_log::endorsement_payload(&record.signature);
    let msg = format!("susi-member-v1:{payload}");
    MemberEndorsement {
        node: node.to_string(),
        sig: hex::encode(sk.sign(msg.as_bytes()).to_bytes()),
    }
}

#[test]
fn bound_electorate_requires_endorsement_quorum() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xEEu8; 32])).unwrap();

    let self_pk = cluster_key::node_pubkey_hex().expect("node key must generate");
    let self_id = cluster_key::wire_node_id();

    // Two foreign member keys — the endorsements we cannot mint without
    // them, which is exactly what the gate proves.
    let sk_b = SigningKey::from_bytes(&[0x42u8; 32]);
    let pk_b = hex::encode(sk_b.verifying_key().to_bytes());
    let sk_c = SigningKey::from_bytes(&[0x43u8; 32]);
    let pk_c = hex::encode(sk_c.verifying_key().to_bytes());
    let sk_d = SigningKey::from_bytes(&[0x44u8; 32]);
    let sk_e = SigningKey::from_bytes(&[0x45u8; 32]);
    let pk_e = hex::encode(sk_e.verifying_key().to_bytes());

    write_roster(
        &dir,
        &[
            (&self_id, "10.0.0.1:9090", &self_pk),
            ("node-b", "10.0.0.2:9090", &pk_b),
            ("node-c", "10.0.0.3:9090", &pk_c),
            ("node-e", "10.0.0.5:9090", &pk_e),
        ],
    );

    let electorate = vec![self_id.clone(), "node-b".to_string(), "node-c".to_string()];
    let mut rec = CommitRecord::seal_member(
        &self_id,
        &self_id,
        commit_log::KIND_MEMBER_REMOVE,
        "node-x@10.0.0.9:9090",
        electorate,
        "",
    )
    .expect("seal_member");

    // A lone leader: member_sig is genuine but only one bound member
    // supports the delta — required majority of 3-bound is 2.
    assert!(commit_log::attribution_valid_at(&rec, &dir));
    assert!(!commit_log::endorsements_satisfied_at(&rec, &dir));
    assert!(
        commit_log::append(&rec).is_err(),
        "an unendorsed privileged record must refuse at intake"
    );

    // A non-electorate signer's endorsement never counts.
    rec.endorsements = vec![endorse(&rec, "node-d", &sk_d)];
    assert!(!commit_log::endorsements_satisfied_at(&rec, &dir));

    // One real endorsement crosses the bound majority.
    rec.endorsements = vec![endorse(&rec, "node-b", &sk_b)];
    assert!(commit_log::endorsements_satisfied_at(&rec, &dir));
    commit_log::append(&rec).unwrap();

    // Duplicates don't double-count: electorate of 4 bound needs 3
    // supporters — self + node-b twice is still two distinct members.
    let mut rec2 = CommitRecord::seal_member(
        &self_id,
        &self_id,
        commit_log::KIND_MEMBER_REMOVE,
        "node-y@10.0.0.9:9090",
        vec![
            self_id.clone(),
            "node-b".to_string(),
            "node-c".to_string(),
            "node-e".to_string(),
        ],
        "",
    )
    .expect("seal_member");
    let e = endorse(&rec2, "node-b", &sk_b);
    rec2.endorsements = vec![e.clone(), e];
    assert!(!commit_log::endorsements_satisfied_at(&rec2, &dir));

    // An endorsement minted for a DIFFERENT record doesn't transplant.
    rec2.endorsements = vec![endorse(&rec, "node-b", &sk_b)];
    assert!(!commit_log::endorsements_satisfied_at(&rec2, &dir));

    // Three distinct supporters satisfy the 4-member electorate.
    rec2.endorsements = vec![
        endorse(&rec2, "node-b", &sk_b),
        endorse(&rec2, "node-c", &sk_c),
    ];
    assert!(commit_log::endorsements_satisfied_at(&rec2, &dir));
}

#[test]
fn unbound_and_prebinding_electorates_stay_ungated() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xEFu8; 32])).unwrap();
    let self_id = cluster_key::wire_node_id();

    // Roster with NO bound keys — the pre-PKI ceiling: privileged
    // records pass on the cluster-key proof alone (as before i76).
    write_roster(
        &dir,
        &[
            ("node-b", "10.0.0.2:9090", ""),
            ("node-c", "10.0.0.3:9090", ""),
        ],
    );
    // peers.json rows without pubkey: write_roster emits "pubkey": "" —
    // bound_pubkey_at treats empty as unbound. Self isn't in this
    // record's electorate, so bound count = 0 → ungated.
    let rec = CommitRecord::seal_member(
        &self_id,
        &self_id,
        commit_log::KIND_MEMBER_ADD,
        "node-n@10.0.0.5:9090",
        vec!["node-b".to_string(), "node-c".to_string()],
        "",
    )
    .expect("seal_member");
    assert!(commit_log::endorsements_satisfied_at(&rec, &dir));
    commit_log::append(&rec).unwrap();
}
