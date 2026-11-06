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

#[test]
fn vc_201_019_mastery_whitespace_only_receipts_remain_stale_in_both_stores() {
    let mut m = ExperimentMemory::default();
    let e = entry("p-ws", "lint failure", Some("clippy-1"));
    m.record(e.clone());
    assert!(!m.may_resubmit(&e, &[], &[" \t\n ".into()]));

    let mut l = ledger();
    l.record("patch", "m-ws", rejected("C-ws")).unwrap();
    assert_eq!(
        l.check_proposal("patch", "m-ws", &[], &[" \t\n ".into()]),
        ProposalCheck::Stale
    );
}

fn mastery_ws(tag: &str) -> std::path::PathBuf {
    let root =
        std::env::temp_dir().join(format!("susi-vc019-mastery-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// `save`/`load` persist `.susi/experiment_memory.json`; a missing or
/// corrupt file loads as empty memory rather than panicking.
#[test]
fn vc_201_019_mastery_experiment_memory_save_load_roundtrip() {
    let ws = mastery_ws("roundtrip");
    assert_eq!(ExperimentMemory::load(&ws), ExperimentMemory::default());

    let mut m = ExperimentMemory::default();
    m.record(entry("p-saved", "gate fail", Some("case-1")));
    m.save(&ws).unwrap();
    assert_eq!(ExperimentMemory::load(&ws), m);

    std::fs::write(
        ws.join(".susi").join("experiment_memory.json"),
        b"{not json",
    )
    .unwrap();
    assert_eq!(ExperimentMemory::load(&ws), ExperimentMemory::default());

    let _ = std::fs::remove_dir_all(&ws);
}

/// Production wiring: `evaluate_and_record` persists every rejected run
/// into the workspace's experiment memory (cause = verdict,
/// counterexample = contamination reason, lineage = corpus revision),
/// passes are left out, and a renamed resubmission repeating the same
/// recorded failure with no fresh output is refused before it is judged
/// again.
#[test]
fn vc_201_019_mastery_evaluate_and_record_persists_rejections_and_refuses_repeats() {
    use crate::eval_contamination::record_access;
    use crate::eval_separation::{evaluate_and_record, CandidateArtifact, EvalRunInput};
    use crate::experiment_lifecycle::ExperimentState;
    use crate::experiment_memory::check_resubmission_in_workspace;
    use crate::rsi_corpus::{
        make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
    };
    use std::collections::BTreeSet;

    let ws = mastery_ws("eval");
    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-mem".into(),
        fixtures: vec![make_fixture(FixtureSpec {
            id: "h1",
            class: FixtureClass::Coding,
            input: "held-out input",
            split: FixtureSplit::HeldOut,
            seed: 1,
            evaluator_expected: Some("expected-output".into()),
        })],
        promotion_spec: None,
    };
    let run = |id: &str, output: &[&str]| EvalRunInput {
        candidate: CandidateArtifact {
            id: id.into(),
            actual_output: output.iter().map(|s| s.to_string()).collect(),
            expected_results: BTreeSet::new(),
        },
        candidate_can_write: false,
        training_access: BTreeSet::new(),
        memory_access: BTreeSet::new(),
        measured: None,
    };

    // A pass leaves memory untouched.
    let ev = evaluate_and_record(&ws, &corpus, &run("cand-pass", &["expected-output"])).unwrap();
    assert_eq!(ev.experiment_state, Some(ExperimentState::Evaluated));
    assert!(ExperimentMemory::load(&ws).by_proposal.is_empty());

    // A rejected run is durably remembered.
    let ev = evaluate_and_record(&ws, &corpus, &run("cand-empty", &[])).unwrap();
    assert_eq!(ev.experiment_state, Some(ExperimentState::Rejected));
    let mem = ExperimentMemory::load(&ws);
    let recorded = &mem.by_proposal["cand-empty"];
    assert_eq!(recorded.cause, "FailWrongOutput");
    assert_eq!(recorded.counterexample, None);
    assert_eq!(recorded.lineage, vec!["rev-mem".to_string()]);

    // The same empty-output failure under a new id is a repeat with no
    // fresh evidence: refused, and not recorded as new evidence.
    let refused = evaluate_and_record(&ws, &corpus, &run("cand-empty-renamed", &[]));
    assert!(
        refused.is_err(),
        "a renamed repeat must not get a clean slate"
    );
    assert!(!ExperimentMemory::load(&ws)
        .by_proposal
        .contains_key("cand-empty-renamed"));
    // Fresh output is fresh evidence: the renamed run is judged again.
    let ev = evaluate_and_record(&ws, &corpus, &run("cand-revised", &["other"])).unwrap();
    assert_eq!(ev.experiment_state, Some(ExperimentState::Rejected));

    // A contaminated rejection records the overlap as its counterexample.
    record_access(&ws, "training", "h1").unwrap();
    let ev = evaluate_and_record(&ws, &corpus, &run("cand-leak", &["expected-output"])).unwrap();
    assert_eq!(ev.experiment_state, Some(ExperimentState::Rejected));
    let leak = &ExperimentMemory::load(&ws).by_proposal["cand-leak"];
    assert_eq!(leak.cause, "FailContaminated");
    let overlap = leak.counterexample.clone().unwrap();
    assert!(overlap.starts_with("held-out overlap: ") && overlap.contains("h1"));

    // The workspace helper sees the same durable memory.
    let repeat = entry("cand-leak", "FailContaminated", Some(&overlap));
    assert!(!check_resubmission_in_workspace(&ws, &repeat, &[], &[]));
    assert!(check_resubmission_in_workspace(
        &ws,
        &repeat,
        &["cand-leak".into()],
        &[]
    ));

    let _ = std::fs::remove_dir_all(&ws);
}

/// Production wiring in cloud proposal validation: a proposal whose
/// title names a remembered rejection is refused unless its evidence
/// cites the prior id or a supplied failure receipt.
#[test]
fn vc_201_019_mastery_cloud_proposal_check_consults_experiment_memory() {
    use crate::cloud_rsi::{check_proposal_memory, Proposal, RejectReason, RsiEvidence};

    let mut m = ExperimentMemory::default();
    m.record(entry("Cache parser output", "FailWrongOutput", None));
    let evidence = RsiEvidence {
        failure_receipts: vec!["T-9: parser cache miss storm".into()],
        user_feedback: vec![],
        roadmap_gaps: vec!["VC-201-019".into()],
        repo_facts: vec!["bloat audit: 3 dead modules".into()],
    };
    let proposal = |evidence: &[&str]| Proposal {
        title: "Cache parser output".into(),
        behavior: "memoize parse trees".into(),
        benefit: "faster".into(),
        risks: vec![],
        acceptance: vec!["cargo".into(), "test".into()],
        evidence: evidence.iter().map(|s| s.to_string()).collect(),
        roadmap: "VC-201-019".into(),
    };

    assert_eq!(
        check_proposal_memory(&m, &proposal(&["VC-201-019"]), &evidence),
        Err(RejectReason::PriorRejection("Cache parser output".into())),
        "a roadmap citation alone does not reopen a recorded rejection"
    );
    assert_eq!(
        check_proposal_memory(&m, &proposal(&["T-9: parser cache miss storm"]), &evidence),
        Ok(()),
        "a supplied failure receipt is verifiable new evidence"
    );
    assert_eq!(
        check_proposal_memory(&m, &proposal(&["Cache parser output"]), &evidence),
        Ok(()),
        "citing the prior proposal id addresses it"
    );
    let mut novel = proposal(&[]);
    novel.title = "Something new".into();
    novel.behavior = "different change".into();
    assert_eq!(check_proposal_memory(&m, &novel, &evidence), Ok(()));
}
