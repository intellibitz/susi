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

/// Fixed: "bounded overlap". After a rotation, `cluster.key.prev`
/// persists on disk (pre-rotation history must stay byte-verifiable),
/// but `prev_key_at` now refuses to return it once its recorded
/// overlap window has elapsed — the retired key is no longer usable
/// through this path at any age, only within its bound.
#[test]
fn vc_201_077_mastery_prev_key_never_expires() {
    let dir = tmp("overlap");
    let old: [u8; 32] = [0x11; 32];
    let new: [u8; 32] = [0x22; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    cluster_key::stage_key_to(&new, &dir.join("cluster.key.next"));
    let fp = cluster_key::key_fingerprint(&new);
    assert!(cluster_key::activate_staged_key_at(&fp, &dir));
    let prev_bytes = std::fs::read_to_string(dir.join("cluster.key.prev")).unwrap();
    assert_eq!(prev_bytes.trim(), hex::encode(old));
    // Within the overlap window the retired key is still usable...
    assert_eq!(cluster_key::prev_key_at(&dir, 0), Some(old));
    // ...but once the recorded window has elapsed, it is not — bounded,
    // not indefinite, overlap.
    assert_eq!(cluster_key::prev_key_at(&dir, u64::MAX), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fixed: "expired credentials cannot trigger plaintext or
/// unauthenticated fallback". A staged key now records when it was
/// staged, and activation refuses a key that has waited past the
/// staging bound — a credential staged long ago cannot activate on
/// fingerprint match alone.
#[test]
fn vc_201_077_mastery_expired_credential_concept_absent() {
    let dir = tmp("expiry");
    let old: [u8; 32] = [0x33; 32];
    let ancient: [u8; 32] = [0x44; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    cluster_key::stage_key_to(&ancient, &dir.join("cluster.key.next"));
    let fp = cluster_key::key_fingerprint(&ancient);
    // Back-date the staged-at record to simulate "staged a year ago".
    std::fs::write(
        format!("{}.staged_at", dir.join("cluster.key.next").display()),
        "0",
    )
    .unwrap();
    assert!(
        !cluster_key::activate_staged_key_at(&fp, &dir),
        "a staged key past the age bound must not activate regardless of fingerprint match"
    );
    // A freshly staged key, right now, still activates normally.
    cluster_key::stage_key_to(&ancient, &dir.join("cluster.key.next"));
    assert!(cluster_key::activate_staged_key_at(&fp, &dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fixed: interrupted rotation self-heals. A torn (corrupt) staged file
/// still refuses activation, but `activate_staged_key_at` now clears it
/// as a side effect, so the next legitimate `stage_key_to` is never
/// blocked by leftover garbage from a crash mid-stage — no outside
/// repair is needed.
#[test]
fn vc_201_077_mastery_torn_stage_wedges_until_outside_repair() {
    let dir = tmp("torn");
    let old: [u8; 32] = [0x55; 32];
    let fresh: [u8; 32] = [0x66; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    // Simulate a crash mid-stage: truncated/corrupt staged file.
    std::fs::write(dir.join("cluster.key.next"), "deadbeef").unwrap();
    assert!(!cluster_key::activate_staged_key_at("anything", &dir));
    assert!(
        !dir.join("cluster.key.next").exists(),
        "a torn staged file must be cleared, not left to wedge future stages"
    );
    // A fresh, legitimate stage now proceeds cleanly with no outside repair.
    cluster_key::stage_key_to(&fresh, &dir.join("cluster.key.next"));
    let fp = cluster_key::key_fingerprint(&fresh);
    assert!(cluster_key::activate_staged_key_at(&fp, &dir));
    let _ = std::fs::remove_dir_all(&dir);
}

/// New: explicit revocation drops the retired key before its overlap
/// window elapses, regardless of how much of the window remained.
#[test]
fn vc_201_077_mastery_explicit_revocation() {
    let dir = tmp("revoke");
    let old: [u8; 32] = [0x77; 32];
    let new: [u8; 32] = [0x88; 32];
    std::fs::write(dir.join("cluster.key"), hex::encode(old)).unwrap();
    cluster_key::stage_key_to(&new, &dir.join("cluster.key.next"));
    let fp = cluster_key::key_fingerprint(&new);
    assert!(cluster_key::activate_staged_key_at(&fp, &dir));
    assert_eq!(cluster_key::prev_key_at(&dir, 0), Some(old));
    assert!(cluster_key::revoke_prev_key_at(&dir));
    assert_eq!(
        cluster_key::prev_key_at(&dir, 0),
        None,
        "a revoked key must be unusable immediately, not just after its overlap window"
    );
    assert!(!dir.join("cluster.key.prev").exists());
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
