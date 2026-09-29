//! Tests for rekey schedule policy (`rekey_schedule_*`).

use crate::cluster_key::{activate_staged_key_at, generate_key, key_fingerprint, stage_key_to};
use crate::rekey_schedule::{evaluate_rekey_policy, RekeyPolicy, RekeyReason};
use std::fs;
use std::path::PathBuf;

#[test]
fn rekey_schedule_not_due_when_interval_not_elapsed() {
    let policy = RekeyPolicy {
        interval_secs: 3600,
        rotate_on_member_removed: true,
    };
    let d = evaluate_rekey_policy(&policy, 1000, 0, Some(2000));
    assert!(!d.should_rekey);
    assert_eq!(d.reason, RekeyReason::NotDue);
}

#[test]
fn rekey_schedule_due_after_interval() {
    let policy = RekeyPolicy {
        interval_secs: 3600,
        rotate_on_member_removed: false,
    };
    let d = evaluate_rekey_policy(&policy, 1000, 0, Some(1000 + 3600));
    assert!(d.should_rekey);
    assert_eq!(d.reason, RekeyReason::ScheduleDue);
}

#[test]
fn rekey_schedule_member_removed_triggers_even_before_interval() {
    let policy = RekeyPolicy::default();
    let d = evaluate_rekey_policy(&policy, 1_000_000, 1, Some(1_000_001));
    assert!(d.should_rekey);
    assert_eq!(d.reason, RekeyReason::MemberRemoved);
}

#[test]
fn rekey_schedule_both_triggers_combine() {
    let policy = RekeyPolicy {
        interval_secs: 10,
        rotate_on_member_removed: true,
    };
    let d = evaluate_rekey_policy(&policy, 0, 2, Some(100));
    assert!(d.should_rekey);
    assert_eq!(d.reason, RekeyReason::ScheduleAndMemberRemoved);
}

#[test]
fn rekey_schedule_offline_peer_tolerated_via_existing_protocol() {
    // Policy says rekey; staging + activation still works when a peer is
    // offline because the staged key is inert until fingerprint commit.
    let dir = std::env::temp_dir().join(format!("susi_rekey_sched_{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let current = generate_key().unwrap();
    fs::write(dir.join("cluster.key"), hex::encode(current)).unwrap();
    let next = generate_key().unwrap();
    let fp = key_fingerprint(&next);
    assert!(stage_key_to(&next, &dir.join("cluster.key.next")));
    // Offline peer: we do not wait — activate when ledger would commit.
    assert!(activate_staged_key_at(&fp, &dir));
    let policy = RekeyPolicy {
        interval_secs: 1,
        rotate_on_member_removed: true,
    };
    let d = evaluate_rekey_policy(&policy, 0, 1, Some(10));
    assert!(d.should_rekey);
    let _ = fs::remove_dir_all(&dir);
    let _: PathBuf = dir; // silence unused if optimized
}
