//! Mastery verification for VC-201-009: evaluation contamination
//! detection — a contaminated run is excluded from improvement evidence
//! with an inspectable reason.
//!
//! The cited tests show id-set overlap flags a reason. The
//! distinguishing properties are that detection actually tracks *access*
//! (not a caller-assembled list that can simply omit the read), that
//! contamination by content — same fixture under another id — is caught,
//! and that 'excluded from improvement evidence' is enforced somewhere,
//! not just flagged in a report nobody consumes.

use crate::eval_contamination::detect;
use std::collections::BTreeSet;

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// Falsification: detection compares caller-supplied id sets. If the
/// access log simply doesn't record the held-out read — the realistic
/// contamination mode, since 'tracking' is entirely on the caller's
/// word — the run reports clean. There is no tracking mechanism to
/// contradict a missing entry.
#[test]
fn vc_201_009_mastery_unrecorded_access_is_undetectable() {
    let held = set(&["h1"]);
    // The candidate read h1, but the training/memory access sets the
    // caller assembles omit it.
    let r = detect(&held, &set(&["t1"]), &set(&["m1"]));
    assert!(
        !r.contaminated,
        "an unrecorded held-out read reports clean — detection trusts the caller's bookkeeping"
    );
}

/// Falsification: overlap is by fixture *id* only. The identical held-out
/// content present in training under a different id — a deduplicated
/// copy, a re-exported fixture — reports clean.
#[test]
fn vc_201_009_mastery_content_contamination_under_another_id_is_clean() {
    let held = set(&["h1", "h2"]);
    // 'h1-copy' is byte-identical content to h1 under a fresh id.
    let r = detect(&held, &set(&["h1-copy"]), &set(&[]));
    assert!(
        !r.contaminated,
        "content-level contamination under a different id reports clean"
    );
}

/// Falsification: 'excluded from improvement evidence' is a flag nothing
/// consumes — detect() returns a report and there is no gate, no caller,
/// no exclusion path. A contaminated run's evidence flows onward with a
/// `contaminated` boolean attached.
#[test]
fn vc_201_009_mastery_exclusion_is_a_flag_not_an_exclusion() {
    let held = set(&["h1"]);
    let r = detect(&held, &set(&["h1"]), &set(&[]));
    // The report exists — but excluding the run is somebody else's job,
    // and nobody's job exists: detect() has no production caller.
    assert!(r.contaminated);
    assert_eq!(r.reason.as_deref(), Some("held-out overlap: h1"));
}

/// What does hold: a genuine id overlap in training or memory flags with
/// a reason naming the ids, and disjoint sets report clean.
#[test]
fn vc_201_009_mastery_id_overlap_flags_with_reason() {
    let held = set(&["h1", "h2"]);
    let r = detect(&held, &set(&["t1"]), &set(&["h2"]));
    assert!(r.contaminated);
    assert!(r.reason.as_deref().unwrap().contains("h2"));
    let clean = detect(&held, &set(&["t1"]), &set(&["m1"]));
    assert!(!clean.contaminated);
    assert!(clean.reason.is_none());
}
