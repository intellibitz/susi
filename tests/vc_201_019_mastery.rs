#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-019: learn from failed and rejected experiments — causes and
//! counterexamples in scoped memory, and a repeated proposal refused
//! unless it addresses the rejection or declares verifiable new
//! evidence.
//!
//! The full falsification suite lives at
//! `crates/susi-gawd/src/tests/vc_201_019_mastery.rs` (exercised by
//! `cargo nextest run -p susi-gawd -E test(vc_201_019_mastery)`); this is
//! the root-package integration test the task's own bare accept command
//! (`cargo nextest run -E test(vc_201_019_mastery)`) actually discovers —
//! bare `nextest run` without `-p`/`--workspace` only discovers the root
//! `susi` package's own `tests/*.rs`, never a member crate's unit tests.

use susi_gawd::cloud_rsi_outcomes::{Outcome, OutcomeLedger, OutcomeUsage, ProposalCheck};
use susi_gawd::experiment_memory::{ExperimentMemory, ExperimentMemoryEntry};

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

fn ledger(tag: &str) -> OutcomeLedger {
    let dir = std::env::temp_dir().join(format!(
        "susi-om-root-{tag}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    OutcomeLedger::load(dir, || 1_700_000_000)
}

#[test]
fn vc_201_019_mastery() {
    // ExperimentMemory half: a repeat is recognized by identical
    // (cause, counterexample) content even under a fresh id, and a
    // blank "evidence" string does not count.
    let mut m = ExperimentMemory::default();
    m.record(ExperimentMemoryEntry {
        proposal_id: "p1".into(),
        cause: "gate fail".into(),
        counterexample: Some("case-9".into()),
        lineage: vec!["root".into()],
    });
    assert!(!m.may_resubmit(&entry("p1-renamed", "gate fail", Some("case-9")), &[], &[]));
    assert!(m.may_resubmit(
        &entry("p1-renamed", "gate fail", Some("case-9")),
        &["p1".into()],
        &[]
    ));
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

    // OutcomeLedger half: a blank receipt string doesn't count as fresh
    // evidence, but a real one does.
    let mut l = ledger("evidence");
    l.record("patch", "m1", rejected("C-9")).unwrap();
    assert_eq!(
        l.check_proposal("patch", "m1", &[], &[String::new()]),
        ProposalCheck::Stale
    );
    assert_eq!(
        l.check_proposal("patch", "m1", &[], &["unrelated".into()]),
        ProposalCheck::Addressed
    );

    // The counterexample cap keeps the newest entries: the oldest
    // rejection becomes unreferenceable once the cap is exceeded, while
    // the newest stays on file.
    let mut l2 = ledger("cap");
    for i in 0..33 {
        l2.record("patch", "m1", rejected(&format!("C-{i}")))
            .unwrap();
    }
    let rec = l2.get("patch", "m1").unwrap();
    assert_eq!(rec.counterexamples.len(), 32);
    assert_eq!(rec.rejected, 33);
    assert_eq!(
        l2.check_proposal("patch", "m1", &["C-0".into()], &[]),
        ProposalCheck::Stale
    );
    assert_eq!(
        l2.check_proposal("patch", "m1", &["C-32".into()], &[]),
        ProposalCheck::Addressed
    );

    // Nominal: a model with no failures is Fresh, and an invented
    // counterexample reference is not accepted.
    assert_eq!(
        l.check_proposal("patch", "m1", &["C-bogus".into()], &[]),
        ProposalCheck::Stale
    );
    assert_eq!(
        l.check_proposal("patch", "other", &[], &[]),
        ProposalCheck::Fresh
    );

    // Lineage and counterexample-free matching:
    // A proposal failing without counterexample still rejects an identical repeat
    m.record(ExperimentMemoryEntry {
        proposal_id: "p2".into(),
        cause: "timeout during compile".into(),
        counterexample: None,
        lineage: vec!["root".into(), "p1".into()],
    });
    assert!(!m.may_resubmit(&entry("p2-retry", "timeout during compile", None), &[], &[]));
    assert!(m.may_resubmit(
        &entry("p2-retry", "timeout during compile", None),
        &["p2".into()],
        &[]
    ));

    // Multi-task isolation: rejections on task "patch" do not taint task "eval"
    assert_eq!(
        l.check_proposal("eval", "m1", &[], &[]),
        ProposalCheck::Fresh
    );
}

#[test]
fn vc_201_019_distinct_cause_or_counterexample_may_submit_without_prior_refs() {
    let mut m = ExperimentMemory::default();
    m.record(entry("p-alpha", "compile error", Some("case-1")));
    assert!(m.may_resubmit(&entry("p-beta", "compile error", Some("case-2")), &[], &[]));
    assert!(m.may_resubmit(&entry("p-gamma", "lint error", Some("case-1")), &[], &[]));
}

#[test]
fn vc_201_019_whitespace_only_evidence_is_rejected_by_memory_and_ledger() {
    let mut m = ExperimentMemory::default();
    m.record(entry("p-ws", "panic in parser", Some("input-a")));
    assert!(!m.may_resubmit(
        &entry("p-ws-2", "panic in parser", Some("input-a")),
        &[],
        &["   ".into()]
    ));

    let mut l = ledger("ws-evidence");
    l.record("patch", "m-ws", rejected("C-ws")).unwrap();
    assert_eq!(
        l.check_proposal("patch", "m-ws", &[], &["  \t  ".into()]),
        ProposalCheck::Stale
    );
}

#[test]
fn vc_201_019_addressing_known_counterexample_id_marks_proposal_addressed() {
    let mut l = ledger("known-ce");
    l.record("patch", "m-ce", rejected("C-42")).unwrap();
    assert_eq!(
        l.check_proposal("patch", "m-ce", &["C-42".into()], &[]),
        ProposalCheck::Addressed
    );
}

#[test]
fn vc_201_019_experiment_memory_entry_preserves_lineage_and_counterexample() {
    let e = entry("prop-7", "type mismatch", Some("ce-7"));
    assert_eq!(e.proposal_id, "prop-7");
    assert_eq!(e.cause, "type mismatch");
    assert_eq!(e.counterexample.as_deref(), Some("ce-7"));
    assert_eq!(e.lineage, vec!["root".to_string()]);
}
