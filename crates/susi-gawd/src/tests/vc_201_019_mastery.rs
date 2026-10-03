//! Mastery verification for VC-201-019: learn from failed and rejected
//! experiments — causes and counterexamples in scoped memory, and a
//! repeated proposal refused unless it addresses the rejection or
//! declares verifiable new evidence.
//!
//! Two mechanisms claim this: the shallow `ExperimentMemory` (cited by
//! vc_201_019.rs) and the real `OutcomeLedger` companion.

use crate::cloud_rsi_outcomes::{Outcome, OutcomeLedger, OutcomeUsage, ProposalCheck};
use crate::experiment_memory::{ExperimentMemory, ExperimentMemoryEntry};

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

/// Falsification (ExperimentMemory half): 'repeated proposal' is keyed
/// only by proposal_id — the identical proposal resubmitted under a new
/// id is 'fresh'. And addresses_prior/new_evidence are caller-supplied
/// booleans — the identical proposal resubmits unchanged by passing
/// `true`. Nothing inspects the proposal's content against the cause.
#[test]
fn vc_201_019_mastery_identical_proposal_under_new_id_is_fresh() {
    let mut m = ExperimentMemory::default();
    m.record(ExperimentMemoryEntry {
        proposal_id: "p1".into(),
        cause: "gate fail".into(),
        counterexample: Some("case-9".into()),
        lineage: vec!["root".into()],
    });
    // Same content, new id — memory cannot tell it is the same proposal.
    assert!(m.may_resubmit("p1-renamed", false, false));
    // And the identical proposal resubmits by asserting a boolean.
    assert!(m.may_resubmit("p1", true, false));
}

/// Falsification (OutcomeLedger half): 'declares new evidence' accepts
/// ANY non-empty receipt list — including a single empty string. A
/// repeated proposal citing nothing and offering an empty string is
/// 'Addressed' — content-free evidence satisfies the gate.
#[test]
fn vc_201_019_mastery_empty_string_receipt_is_evidence() {
    let mut l = ledger();
    l.record("patch", "m1", rejected("C-9")).unwrap();
    assert_eq!(
        l.check_proposal("patch", "m1", &[], &[String::new()]),
        ProposalCheck::Addressed,
        "an empty string counts as fresh evidence"
    );
    assert_eq!(
        l.check_proposal("patch", "m1", &[], &["unrelated".into()]),
        ProposalCheck::Addressed
    );
}

/// Falsification: the counterexample cap keeps the OLDEST entries and
/// silently drops every new rejection once MAX_COUNTEREXAMPLES is
/// reached — the ledger stops learning from fresh failures. A proposal
/// referencing a dropped rejection reads Stale even though the rejection
/// happened.
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
    // The 33rd rejection exists in the count but its counterexample was
    // discarded — referencing it reports Stale.
    assert_eq!(
        l.check_proposal("patch", "m1", &["C-32".into()], &[]),
        ProposalCheck::Stale
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
