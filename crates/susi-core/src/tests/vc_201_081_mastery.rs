//! Mastery verification for VC-201-081: durable memory carrying
//! workspace, receipt, revision, classification, expiry, and owner, with
//! reads rejecting expired or unauthorized records.

use crate::memory_provenance::{MemoryRecord, Provenance};

fn rec(id: &str, source: &str, recorded: u64, retention: u64) -> MemoryRecord {
    MemoryRecord {
        id: id.into(),
        body: "note".into(),
        provenance: Provenance {
            source: source.into(),
            recorded_unix: recorded,
            retention_secs: retention,
        },
    }
}

/// Falsification: 'distinguish evidence from model-generated assertions'
/// — `source` is a caller-declared string. A model assertion can declare
/// itself 'user' or 'receipt' and reads cannot distinguish; nothing
/// requires a receipt to carry evidence of its claim.
#[test]
fn vc_201_081_mastery_self_declared_provenance() {
    let forged = rec("m1", "user-verified-evidence", 100, 50);
    assert_eq!(forged.authoritative_source(), "user-verified-evidence");
    // The declared source is whatever the writer typed — a model
    // assertion claiming user provenance is indistinguishable.
}

/// Falsification: the claimed fields do not exist — Provenance has only
/// source/recorded_unix/retention_secs. There is no workspace, source
/// receipt, source revision, data classification, or owner to attach.
/// (Structural: constructing Provenance can only set those three fields.)
#[test]
fn vc_201_081_mastery_claimed_fields_absent() {
    let p = Provenance {
        source: "x".into(),
        recorded_unix: 0,
        retention_secs: 1,
    };
    // Compiling this struct literal is the proof: workspace, receipt,
    // revision, classification and owner cannot be attached — the claim
    // names five fields that do not exist.
    assert_eq!(p.retention_secs, 1);
}

/// Falsification: 'reads reject expired or unauthorized records' — there
/// is no read path at all. expired() is a predicate nobody is obliged to
/// call; a caller can read and use an expired record freely, and there is
/// no authorization check (no owner field exists to authorize against).
#[test]
fn vc_201_081_mastery_expired_records_are_readable() {
    let r = rec("m1", "user", 100, 50);
    assert!(r.expired(200));
    // Nothing stands between an expired record and a reader: body is a
    // public field, expired() is advisory.
    assert_eq!(r.body, "note");
}

/// Falsification: retention_secs == 0 makes a record born-expired —
/// including at its own recorded instant — silently discarding any
/// write whose retention was left unset.
#[test]
fn vc_201_081_mastery_zero_retention_is_born_expired() {
    let r = rec("m1", "user", 100, 0);
    assert!(r.expired(100), "recorded at 100, expired at 100");
}

/// What holds: the expiry arithmetic is correct — before the deadline
/// the record is live, at/past it, expired.
#[test]
fn vc_201_081_mastery_expiry_math_holds() {
    let r = rec("m1", "user", 100, 50);
    assert!(!r.expired(149));
    assert!(r.expired(150));
    // And a future-dated record is not yet expired.
    let f = rec("m2", "user", 1_000_000, 50);
    assert!(!f.expired(200));
}
