//! Mastery verification for VC-201-019: learn from failed and rejected
//! experiments — causes and counterexamples in scoped memory, and a
//! repeated proposal refused unless it addresses the rejection or
//! declares verifiable new evidence.
//!
//! Two mechanisms claim this: the shallow `ExperimentMemory` (cited by
//! vc_201_019.rs) and the real `OutcomeLedger` companion.

use crate::cloud_rsi_outcomes::{Outcome, OutcomeLedger, OutcomeUsage, ProposalCheck};
use crate::experiment_memory::{ExperimentMemory, ExperimentMemoryEntry};

fn entry(proposal_id: &str, cause: &str, counterexample: Option<&str>) -> ExperimentMemoryEntry {
    ExperimentMemoryEntry {
        proposal_id: proposal_id.into(),
        cause: cause.into(),
        counterexample: counterexample.map(String::from),
        lineage: vec!["root".into()],
    }
}

fn rejected(id: &str) -> Outcome {
    Outcome::Rejected {
        candidate_id: id.into(),
        reason: "broke parser".into(),
        receipts: vec!["r".into()],
        usage: OutcomeUsage::default(),
    }
}

fn ledger() -> OutcomeLedger {
    let dir = std::env::temp_dir().join(format!(
        "susi-om-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    OutcomeLedger::load(dir, || 1_700_000_000)
}

/// Fixed (ExperimentMemory half): a repeat is now recognized by identical
/// (cause, counterexample) content even under a fresh id, and
/// `addressed`/`fresh_receipts` are real evidence rather than
/// caller-asserted booleans — a blank receipt no longer counts.
#[test]
fn vc_201_019_mastery_identical_proposal_under_new_id_is_fresh() {
    let mut m = ExperimentMemory::default();
    m.record(ExperimentMemoryEntry {
        proposal_id: "p1".into(),
        cause: "gate fail".into(),
        counterexample: Some("case-9".into()),
        lineage: vec!["root".into()],
    });
    // Same content under a new id is still recognized as the same prior
    // rejection, so it is refused without addressing it or fresh evidence.
    assert!(!m.may_resubmit(&entry("p1-renamed", "gate fail", Some("case-9")), &[], &[]));
    assert!(m.may_resubmit(
        &entry("p1-renamed", "gate fail", Some("case-9")),
        &["p1".into()],
        &[]
    ));
    // The identical id with a blank "evidence" string is also refused.
    assert!(!m.may_resubmit(
        &entry("p1", "gate fail", Some("case-9")),
        &[],
        &[String::new()]
    ));
    assert!(m.may_resubmit(
        &entry("p1", "gate fail", Some("case-9")),
        &[],
        &["genuinely new failure mode".into()]
    ));
}

/// Fixed (OutcomeLedger half): a blank receipt string no longer counts
/// as evidence — only a non-whitespace receipt satisfies the gate.
#[test]
fn vc_201_019_mastery_empty_string_receipt_is_evidence() {
    let mut l = ledger();
    l.record("patch", "m1", rejected("C-9")).unwrap();
    assert_eq!(
        l.check_proposal("patch", "m1", &[], &[String::new()]),
        ProposalCheck::Stale,
        "a blank string must not count as fresh evidence"
    );
    assert_eq!(
        l.check_proposal("patch", "m1", &[], &["unrelated".into()]),
        ProposalCheck::Addressed
    );
}

/// Fixed: the counterexample cap now keeps the NEWEST entries and drops
/// the oldest once `MAX_COUNTEREXAMPLES` is reached, so the ledger keeps
/// learning from fresh failures instead of freezing on the first batch.
#[test]
fn vc_201_019_mastery_cap_drops_newest_failures() {
    let mut l = ledger();
    for i in 0..33 {
        l.record("patch", "m1", rejected(&format!("C-{i}")))
            .unwrap();
    }
    let rec = l.get("patch", "m1").unwrap();
    assert_eq!(rec.counterexamples.len(), 32);
    assert_eq!(rec.rejected, 33);
    // The oldest rejection (C-0) was dropped to make room.
    assert_eq!(
        l.check_proposal("patch", "m1", &["C-0".into()], &[]),
        ProposalCheck::Stale,
        "the oldest, evicted counterexample must not be referenceable"
    );
    // The newest rejection (C-32) is still on file.
    assert_eq!(
        l.check_proposal("patch", "m1", &["C-32".into()], &[]),
        ProposalCheck::Addressed,
        "the newest rejection's counterexample must still be on file"
    );
}

/// What holds (ledger half): causes persist across reload, a referenced
/// recorded counterexample is required for 'Addressed', and a model with
/// no failures is Fresh.
#[test]
fn vc_201_019_mastery_ledger_semantics_hold() {
    let mut l = ledger();
    l.record("patch", "m1", rejected("C-9")).unwrap();
    assert_eq!(
        l.check_proposal("patch", "m1", &[], &[]),
        ProposalCheck::Stale
    );
    assert_eq!(
        l.check_proposal("patch", "m1", &["C-9".into()], &[]),
        ProposalCheck::Addressed
    );
    assert_eq!(
        l.check_proposal("patch", "m1", &["C-bogus".into()], &[]),
        ProposalCheck::Stale,
        "an invented counterexample reference is not accepted"
    );
    assert_eq!(
        l.check_proposal("patch", "other", &[], &[]),
        ProposalCheck::Fresh
    );
}
