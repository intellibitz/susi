//! VC-200-001: member→key binding is accepted only after a bound-majority
//! check on the *committed ledger* — the proposer-attested `committed_at`
//! can no longer shrink the required electorate to zero.

use crate::commit_log::{
    endorsement_payload, endorsements_satisfied_at, endorsements_satisfied_at_tip, CommitRecord,
    MemberEndorsement, ENV_LOCK,
};
use ed25519_dalek::{Signer, SigningKey};
use std::collections::BTreeMap;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn pubkey(k: &SigningKey) -> String {
    hex::encode(k.verifying_key().to_bytes())
}

fn sign(k: &SigningKey, payload: &str) -> String {
    // member_verify checks `susi-member-v1:{payload}` — same domain prefix.
    let msg = format!("susi-member-v1:{payload}");
    hex::encode(k.sign(msg.as_bytes()).to_bytes())
}

/// A roster dir with `peers.json` binding each member to its pubkey.
fn roster_dir(tag: &str, bound: &BTreeMap<&str, SigningKey>) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("vc2001-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let rows: Vec<serde_json::Value> = bound
        .iter()
        .map(|(id, k)| {
            serde_json::json!({
                "node_id": id,
                "admission": "explicit",
                "address": format!("10.0.0.{id}:9190"),
                "pubkey": pubkey(k),
                "key_bound_at": 100u64,
                "key_bound_seq": 5u64,
            })
        })
        .collect();
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string(&rows).unwrap(),
    )
    .unwrap();
    dir
}

fn binding_record(coordinator: &str, sig_key: &SigningKey) -> CommitRecord {
    let mut r = CommitRecord {
        epoch: "e".into(),
        coordinator: coordinator.into(),
        electorate: vec!["a".into(), "b".into(), "c".into()],
        tally: 0,
        quorum_threshold: 0,
        value_hash: "h".into(),
        value: "d@10.0.0.4:9190".into(),
        committed_at: 1, // backdated — predates every roster binding
        seq: 9,
        leader: coordinator.into(),
        term: 2,
        prev_epoch: String::new(),
        kind: crate::commit_log::KIND_MEMBER_ADD.into(),
        signature: "deadbeef".into(),
        member_sig: String::new(),
        member_pubkey: "00".repeat(32),
        subject_sig: String::new(),
        endorsements: Vec::new(),
    };
    r.member_sig = sign(sig_key, &r.signature);
    r
}

#[test]
fn vc_200_001_backdated_record_still_needs_bound_majority() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bound = BTreeMap::from([("a", key(1)), ("b", key(2)), ("c", key(3))]);
    let dir = roster_dir("backdate", &bound);
    // Leader `a` alone: its member_sig counts as its own endorsement —
    // 1 of 3 bound is below majority.
    let rec = binding_record("a", &bound["a"]);
    // The history view sees the window the proposer claimed: zero bound
    // at committed_at=1 → requirement 0. That is exactly the hole.
    assert!(
        endorsements_satisfied_at(&rec, &dir),
        "committed_at-relative check is fooled by backdating"
    );
    // The intake view reads bound state off the committed roster —
    // the same record must carry a real bound-majority.
    assert!(
        !endorsements_satisfied_at_tip(&rec, &dir),
        "backdating must not shrink the committed-ledger electorate"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vc_200_001_binding_accepted_with_bound_majority_endorsements() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bound = BTreeMap::from([("a", key(1)), ("b", key(2)), ("c", key(3))]);
    let dir = roster_dir("quorum", &bound);
    let mut rec = binding_record("a", &bound["a"]);
    // b endorses: a's member_sig + b's endorsement = 2 of 3 bound.
    rec.endorsements.push(MemberEndorsement {
        node: "b".into(),
        sig: sign(&bound["b"], &endorsement_payload(&rec.signature)),
    });
    assert!(
        endorsements_satisfied_at_tip(&rec, &dir),
        "coordinator + one bound endorser is a majority of three"
    );
    // A *non-bound* endorser cannot be counted even with a valid signature.
    let mut rec2 = rec.clone();
    rec2.endorsements[0].node = "unbound-member".into();
    assert!(!endorsements_satisfied_at_tip(&rec2, &dir));
    // A forgery signed by the wrong bound key fails verification.
    let mut rec3 = rec.clone();
    rec3.endorsements[0].sig = sign(&bound["c"], &endorsement_payload(&rec3.signature));
    assert!(!endorsements_satisfied_at_tip(&rec3, &dir));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vc_200_001_no_bound_members_still_admits_legacy_records() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Pre-PKI roster: rows carry no pubkey — the ceiling this replaces.
    let dir = std::env::temp_dir().join(format!("vc2001-pre-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string(&serde_json::json!([
            {"node_id": "a", "admission": "explicit", "address": "10.0.0.1:9190"},
            {"node_id": "b", "admission": "explicit", "address": "10.0.0.2:9190"},
        ]))
        .unwrap(),
    )
    .unwrap();
    let rec = binding_record("a", &key(1));
    assert!(
        endorsements_satisfied_at_tip(&rec, &dir),
        "unbound electorate keeps the bounded pre-PKI window"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vc_200_001_overlapping_deltas_serialize_through_majority() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let bound = BTreeMap::from([("a", key(1)), ("b", key(2)), ("c", key(3))]);
    let dir = roster_dir("overlap", &bound);
    // Two leaders race divergent deltas; only the one carrying a bound
    // majority may be accepted.
    let rec_a = binding_record("a", &bound["a"]);
    let mut rec_b = binding_record("b", &bound["b"]);
    rec_b.value = "e@10.0.0.5:9190".into();
    rec_b.endorsements.push(MemberEndorsement {
        node: "a".into(),
        sig: sign(&bound["a"], &endorsement_payload(&rec_b.signature)),
    });
    assert!(!endorsements_satisfied_at_tip(&rec_a, &dir));
    assert!(endorsements_satisfied_at_tip(&rec_b, &dir));
    let _ = std::fs::remove_dir_all(&dir);
}
