#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Integration test for per-member signing keys (Ed25519 `node.key`):
//! every sealed record carries `member_sig` over its HMAC signature,
//! and once a coordinator's pubkey is bound in the roster the
//! attribution gate refuses records that don't verify under it —
//! a stolen `cluster.key` alone can no longer mint records as a
//! bound member.
//!
//! Runs against a hermetic HOME so the host's real `node.key` /
//! `cluster.key` are never touched. `commit_log::ENV_LOCK` serializes
//! against other env-seamed tests.

use ed25519_dalek::{Signer, SigningKey};
use std::path::PathBuf;
use susi_config::cluster_key;
use susi_core::commit_log::{self, CommitInput, CommitRecord};

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
            "susi_msig_it_{}_{}",
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

fn decision_as(coordinator: &str, value: &str) -> CommitRecord {
    CommitRecord::seal(CommitInput {
        coordinator,
        leader: coordinator,
        electorate: vec!["A".into(), "B".into()],
        tally: 2,
        quorum_threshold: 2,
        value,
    })
    .expect("seal must succeed while a cluster.key exists")
}

/// The subject-side half of the v3 handshake: `(pubkey, subject_sig)`
/// for `node_id`, signed under a foreign member key — matching
/// `member_sign`'s `susi-member-v1:` wrap of `bind_payload`.
fn attest(node_id: &str, sk: &SigningKey) -> (String, String) {
    let pk = hex::encode(sk.verifying_key().to_bytes());
    let msg = format!("susi-member-v1:{}", cluster_key::bind_payload(node_id, &pk));
    (pk, hex::encode(sk.sign(msg.as_bytes()).to_bytes()))
}

/// Write a peers.json with one explicit member row; `pubkey`/`bound_at`
/// model the binding state the attribution gate reads.
fn write_roster(dir: &std::path::Path, node_id: &str, pubkey: &str, bound_at: u64) {
    let mut row = serde_json::json!({
        "node_id": node_id,
        "address": "10.0.0.9:9090",
        "node_type": "PEER",
        "admission": "explicit",
    });
    if !pubkey.is_empty() {
        row["pubkey"] = serde_json::json!(pubkey);
        row["key_bound_at"] = serde_json::json!(bound_at);
    }
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string_pretty(&vec![row]).unwrap(),
    )
    .unwrap();
}

#[test]
fn sealed_records_carry_member_sig_and_bound_coordinators_are_enforced() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xAAu8; 32])).unwrap();

    // node.key generates on first use — self is always self-bound.
    let self_pk = cluster_key::node_pubkey_hex().expect("node key must generate");
    assert_eq!(self_pk.len(), 64);
    let self_id = cluster_key::wire_node_id();

    // A self-sealed record carries member_sig and verifies under our
    // own pubkey — then appends (self is bound with bound_at = 0).
    let rec = decision_as(&self_id, "v1");
    assert!(!rec.member_sig.is_empty());
    assert!(cluster_key::member_verify(
        &self_pk,
        &rec.signature,
        &rec.member_sig
    ));
    assert!(commit_log::attribution_valid_at(&rec, &dir));
    commit_log::append(&rec).unwrap();

    // A record attributed to a foreign coordinator, sealed by us:
    // signature (HMAC) is valid — we hold cluster.key — but member_sig
    // is OUR key's, not the foreign member's.
    let forged = decision_as("node-v", "v2");
    assert!(forged.verify(), "cluster-key signature is genuine");

    // Unbound roster: the record appends on HMAC alone — the pre-PKI
    // ceiling, documented as the residual window.
    write_roster(&dir, "node-v", "", 0);
    commit_log::append(&forged).unwrap();

    // Now bind node-v's real pubkey (not ours — any other 32-byte key).
    let victim_pk = hex::encode([0x42u8; 32]);
    write_roster(&dir, "node-v", &victim_pk, 0);
    assert!(!commit_log::attribution_valid_at(&forged, &dir));

    // The same record arriving post-binding is refused: valid HMAC is
    // no longer enough to mint records as a bound member.
    let rebound = decision_as("node-v", "v3");
    assert!(
        commit_log::append(&rebound).is_err(),
        "bound coordinator with a foreign member_sig must refuse"
    );

    // Unsigned records are equally refused once bound.
    let mut unsigned = decision_as("node-v", "v4");
    unsigned.member_sig.clear();
    assert!(commit_log::append(&unsigned).is_err());

    // Pre-binding history: a record committed before bound_at is
    // accepted — old members' pre-PKI ledgers must still converge.
    let early = decision_as("node-v", "v5");
    write_roster(&dir, "node-v", &victim_pk, early.committed_at + 1000);
    assert!(commit_log::attribution_valid_at(&early, &dir));
    commit_log::append(&early).unwrap();
}

#[test]
fn seq_floor_binding_refuses_backdated_forgeries() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xBBu8; 32])).unwrap();

    let victim_pk = hex::encode([0x42u8; 32]);

    // Roster row bound with a seq floor of 2 — the member's frontier
    // when its key was committed.
    let mut row = serde_json::json!({
        "node_id": "node-v",
        "address": "10.0.0.9:9090",
        "node_type": "PEER",
        "admission": "explicit",
        "pubkey": victim_pk,
        "key_bound_at": 1_000,
        "key_bound_seq": 2,
    });
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string_pretty(&vec![row.clone()]).unwrap(),
    )
    .unwrap();

    // A *new* record (seq 3 > floor 2) with a forged pre-binding
    // committed_at — the timestamp exemption can't launder it anymore.
    let mut backdated = decision_as("node-v", "forged-new");
    backdated.seq = 3;
    backdated.committed_at = 500; // < bound_at — backdated
    backdated.member_sig.clear();
    assert!(!commit_log::attribution_valid_at(&backdated, &dir));

    // A record inside the floor WITH a post-binding timestamp is also
    // refused — genuine pre-binding history never has a newer ts.
    let mut rewrapped = decision_as("node-v", "rewrapped-old");
    rewrapped.seq = 2;
    rewrapped.committed_at = 2_000; // >= bound_at
    rewrapped.member_sig.clear();
    assert!(!commit_log::attribution_valid_at(&rewrapped, &dir));

    // Genuine pre-binding history — seq inside floor, old timestamp —
    // still converges unsigned.
    let mut honest = decision_as("node-v", "honest-old");
    honest.seq = 1;
    honest.committed_at = 500;
    honest.member_sig.clear();
    assert!(commit_log::attribution_valid_at(&honest, &dir));

    // Legacy bindings without key_bound_seq keep the timestamp rule.
    row["key_bound_seq"] = serde_json::Value::Null;
    row.as_object_mut().unwrap().remove("key_bound_seq");
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string_pretty(&vec![row]).unwrap(),
    )
    .unwrap();
    assert!(commit_log::attribution_valid_at(&honest, &dir));
    assert!(!commit_log::attribution_valid_at(&rewrapped, &dir));
}

#[test]
fn member_add_commits_the_subjects_pubkey_binding() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xBBu8; 32])).unwrap();
    let self_id = cluster_key::wire_node_id();

    // Seal a member_add carrying the subject's attested pubkey — the
    // ledger half of key binding. The subject_sig is the subject's own
    // signature over `susi-bind-v1:{id}:{pk}` (the v3-pong attestation),
    // without which intake refuses the binding.
    let sk_m = SigningKey::from_bytes(&[0x77u8; 32]);
    let (subject_pk, subject_sig) = attest("node-m", &sk_m);
    let mut add = CommitRecord::seal_member(
        &self_id,
        &self_id,
        commit_log::KIND_MEMBER_ADD,
        "node-m@10.0.0.9:9090",
        vec![self_id.clone()],
        &subject_pk,
    )
    .expect("seal_member");
    add.subject_sig = subject_sig.clone();
    assert_eq!(add.member_pubkey, subject_pk);
    commit_log::append(&add).unwrap();

    // The applied roster row binds the key at the record's committed_at.
    let peers: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
    let row = peers
        .iter()
        .find(|p| p.get("node_id").and_then(|v| v.as_str()) == Some("node-m"))
        .expect("member row applied");
    assert_eq!(
        row.get("pubkey").and_then(|v| v.as_str()),
        Some(subject_pk.as_str())
    );
    assert_eq!(
        row.get("key_bound_at").and_then(|v| v.as_u64()),
        Some(add.committed_at)
    );
    assert_eq!(
        row.get("bind_sig").and_then(|v| v.as_str()),
        Some(subject_sig.as_str()),
        "the subject attestation must persist on the row for re-proposals"
    );

    // And from now on, records as node-m must carry node-m's signature —
    // ours (self) is a different key and refuses.
    let forged = decision_as("node-m", "x");
    assert!(
        commit_log::append(&forged).is_err(),
        "post-binding record as node-m with our member_sig must refuse"
    );

    // First-write-wins: a second member_add claiming a different key
    // does not re-bind (re-binding needs remove + re-add). It still
    // carries a valid attestation for its claimed key — the refusal is
    // on the bind, not the record.
    let sk_r = SigningKey::from_bytes(&[0x99u8; 32]);
    let (rebind_pk, rebind_sig) = attest("node-m", &sk_r);
    let mut rebind = CommitRecord::seal_member(
        &self_id,
        &self_id,
        commit_log::KIND_MEMBER_ADD,
        "node-m@10.0.0.9:9090",
        vec![self_id.clone()],
        &rebind_pk,
    )
    .expect("seal_member rebind");
    rebind.subject_sig = rebind_sig;
    commit_log::append(&rebind).unwrap();
    let peers: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
    let row = peers
        .iter()
        .find(|p| p.get("node_id").and_then(|v| v.as_str()) == Some("node-m"))
        .unwrap();
    assert_eq!(
        row.get("pubkey").and_then(|v| v.as_str()),
        Some(subject_pk.as_str()),
        "binding must be first-write-wins"
    );
}

#[test]
fn member_add_pubkey_requires_subject_attestation() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new();
    let dir = home.config();
    std::fs::write(dir.join("cluster.key"), hex::encode([0xCCu8; 32])).unwrap();
    let self_id = cluster_key::wire_node_id();
    let sk_v = SigningKey::from_bytes(&[0x77u8; 32]);
    let (pk_v, sig_v) = attest("node-v", &sk_v);

    let seal_add = |pubkey: &str| {
        CommitRecord::seal_member(
            &self_id,
            &self_id,
            commit_log::KIND_MEMBER_ADD,
            "node-v@10.0.0.9:9090",
            vec![self_id.clone()],
            pubkey,
        )
        .expect("seal_member")
    };

    // No attestation at all — the proposer-asserted binding the gate
    // exists to refuse.
    let unattested = seal_add(&pk_v);
    assert!(
        commit_log::append(&unattested).is_err(),
        "member_add with a pubkey but no subject_sig must refuse"
    );

    // A signature that doesn't verify — wrong key material entirely.
    let mut wrong_key = seal_add(&pk_v);
    let (_pk_x, sig_x) = attest("node-v", &SigningKey::from_bytes(&[0x88u8; 32]));
    wrong_key.subject_sig = sig_x;
    assert!(commit_log::append(&wrong_key).is_err());

    // A valid signature over a *different* node id — attestation is
    // bound to the claimed identity, not just the key.
    let mut wrong_id = seal_add(&pk_v);
    let (_p, sig_id) = attest("node-other", &sk_v);
    wrong_id.subject_sig = sig_id;
    assert!(commit_log::append(&wrong_id).is_err());

    // Tampered attestation bytes.
    let mut tampered = seal_add(&pk_v);
    tampered.subject_sig = format!("00{}", &sig_v[2..]);
    assert!(commit_log::append(&tampered).is_err());

    // The genuine attestation commits and binds.
    let mut good = seal_add(&pk_v);
    good.subject_sig = sig_v;
    commit_log::append(&good).unwrap();
    let peers: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
    let row = peers
        .iter()
        .find(|p| p.get("node_id").and_then(|v| v.as_str()) == Some("node-v"))
        .expect("member row applied");
    assert_eq!(
        row.get("pubkey").and_then(|v| v.as_str()),
        Some(pk_v.as_str())
    );

    // A member_add with no pubkey claim needs no attestation — the
    // unbound legacy path stays open.
    let unbound = CommitRecord::seal_member(
        &self_id,
        &self_id,
        commit_log::KIND_MEMBER_ADD,
        "node-u@10.0.0.7:9090",
        vec![self_id.clone()],
        "",
    )
    .expect("seal_member");
    commit_log::append(&unbound).unwrap();
}
