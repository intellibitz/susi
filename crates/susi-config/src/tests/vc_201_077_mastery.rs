//! Mastery checks for VC-201-077: roll credentials and transport
//! certificates with bounded overlap and explicit revocation; interrupted
//! rotation recovers and expired credentials cannot trigger plaintext or
//! unauthenticated fallback.
//!
//! Every test name starts `vc_201_077_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-config vc_201_077`.

use crate::cluster_key;
use crate::rekey_schedule::{
    evaluate_rekey_policy, RekeyPolicy, RekeyReason, RekeyScheduleDecision,
};

fn tmp(suffix: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("vc077-{suffix}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// "Roll credentials ... with bounded overlap and explicit revocation" —
/// `evaluate_rekey_policy` produces a *decision*; nothing acts on it, it has
/// no production callers, and the decision carries no overlap bound,
/// revocation, or certificate surface.
#[test]
fn vc_201_077_mastery_decision_has_no_overlap_revocation_or_cert_surface() {
    let policy = RekeyPolicy {
        interval_secs: 60,
        rotate_on_member_removed: true,
    };
    let d: RekeyScheduleDecision = evaluate_rekey_policy(&policy, 0, 5, Some(10_000));
    assert!(d.should_rekey);
    let json = serde_json::to_value(&d).unwrap();
    let s = json.to_string();
    // No overlap window, no revocation token, no certificate — the decision
    // is schedule bookkeeping only.
    assert!(!s.contains("overlap"));
    assert!(!s.contains("revoke"));
    assert!(!s.contains("cert"));
    let pj = serde_json::to_value(&policy).unwrap().to_string();
    assert!(!pj.contains("cert"));
}

/// "Bounded overlap": the previous key is retained indefinitely. After a
/// rotation, `cluster.key.prev` persists with no expiry, no bound, and no
/// scheduled deletion — the overlap is unbounded.
#[test]
fn vc_201_077_mastery_prev_key_never_expires() {
    let dir = tmp("overlap");
    let old: [u8; 32] = [0x11; 32];
    let new: [u8; 32] = [0x22; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    cluster_key::stage_key_to(&new, &dir.join("cluster.key.next"));
    let fp = cluster_key::key_fingerprint(&new);
    assert!(cluster_key::activate_staged_key_at(&fp, &dir));
    // The rotated-out key is still on disk — and nothing in the API carries
    // a not-after bound. Reading it later, at any age, works identically.
    let prev_bytes = std::fs::read_to_string(dir.join("cluster.key.prev")).unwrap();
    assert_eq!(prev_bytes.trim(), hex::encode(old));
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Expired credentials cannot trigger plaintext or unauthenticated
/// fallback": keys carry no expiry at all — a staged key activates on
/// fingerprint match alone regardless of age. There is no `not_after`, no
/// expiry field, no refusal for a stale credential.
#[test]
fn vc_201_077_mastery_expired_credential_concept_absent() {
    let dir = tmp("expiry");
    let old: [u8; 32] = [0x33; 32];
    let ancient: [u8; 32] = [0x44; 32]; // imagine: staged a year ago
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    cluster_key::stage_key_to(&ancient, &dir.join("cluster.key.next"));
    let fp = cluster_key::key_fingerprint(&ancient);
    // No expiry is consulted — the stale staged key activates.
    assert!(cluster_key::activate_staged_key_at(&fp, &dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Interrupted rotation: a torn stage (corrupt staged file) is a permanent
/// wedge — `activate_staged_key_at` returns false forever and nothing
/// recovers or clears it; the next `stage_key_to` *overwrites* it, but no
/// recovery path exists inside the protocol itself.
#[test]
fn vc_201_077_mastery_torn_stage_wedges_until_outside_repair() {
    let dir = tmp("torn");
    let old: [u8; 32] = [0x55; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    // Simulate a crash mid-stage: truncated/corrupt staged file.
    std::fs::write(dir.join("cluster.key.next"), "deadbeef").unwrap();
    // Every subsequent activation attempt fails — nothing self-heals it.
    assert!(!cluster_key::activate_staged_key_at("anything", &dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Holds: a wrong fingerprint refuses activation (staged key stays inert).
#[test]
fn vc_201_077_mastery_wrong_fingerprint_refuses_holds() {
    let dir = tmp("fp");
    let old: [u8; 32] = [0x66; 32];
    let new: [u8; 32] = [0x77; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    cluster_key::stage_key_to(&new, &dir.join("cluster.key.next"));
    assert!(!cluster_key::activate_staged_key_at(
        "wrong-fingerprint",
        &dir
    ));
    assert!(dir.join("cluster.key.next").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Holds: both triggers reported honestly by the schedule.
#[test]
fn vc_201_077_mastery_triggers_report_holds() {
    let p = RekeyPolicy {
        interval_secs: 10,
        rotate_on_member_removed: true,
    };
    let d = evaluate_rekey_policy(&p, 0, 3, Some(100));
    assert_eq!(d.reason, RekeyReason::ScheduleAndMemberRemoved);
    let d2 = evaluate_rekey_policy(&p, 95, 0, Some(100));
    assert!(!d2.should_rekey);
}
