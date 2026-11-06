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

use crate::eval_contamination::{detect, detect_in_corpus, exclude_if_contaminated};
use crate::rsi_corpus::{make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus};
use std::collections::BTreeSet;

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn fx(id: &str, input: &str, split: FixtureSplit) -> crate::rsi_corpus::CorpusFixture {
    make_fixture(FixtureSpec {
        id,
        class: FixtureClass::Coding,
        input,
        split,
        seed: 1,
        evaluator_expected: None,
    })
}

fn corpus(fixtures: Vec<crate::rsi_corpus::CorpusFixture>) -> RsiCorpus {
    RsiCorpus {
        schema_version: crate::rsi_corpus::RSI_CORPUS_SCHEMA.into(),
        revision: "rev-1".into(),
        fixtures,
        promotion_spec: None,
    }
}

/// Still true at this layer: `detect()` is a pure id-set comparison, so
/// a read nobody recorded is indistinguishable from a clean run here.
/// Access tracking lives upstream: `eval_contamination::record_access`
/// logs reads to `.susi/eval_access_log.jsonl`, and `evaluate_and_record`
/// merges that log into every run (see
/// `vc_201_009_mastery_evaluate_and_record_catches_disk_logged_held_out_read`).
/// A read path that never calls `record_access` is still unseen.
#[test]
fn vc_201_009_mastery_unrecorded_access_is_undetectable() {
    let held = set(&["h1"]);
    let r = detect(&held, &set(&["t1"]), &set(&["m1"]));
    assert!(
        !r.contaminated,
        "an unrecorded held-out read reports clean — detection trusts the caller's bookkeeping"
    );
}

/// Fixed: `detect_in_corpus` resolves every accessed id that names a
/// known corpus fixture to its content hash before comparing, so
/// identical held-out content reachable under a different id (a fixture
/// duplicated into the train split) is now caught — not just a literal
/// id match.
#[test]
fn vc_201_009_mastery_content_contamination_under_another_id_is_clean() {
    let c = corpus(vec![
        fx("h1", "the secret held-out input", FixtureSplit::HeldOut),
        fx("h2", "another held-out input", FixtureSplit::HeldOut),
        // Same content as h1, duplicated into train under a fresh id.
        fx("h1-copy", "the secret held-out input", FixtureSplit::Train),
    ]);
    let r = detect_in_corpus(&c, &set(&["h1-copy"]), &set(&[])).unwrap();
    assert!(
        r.contaminated,
        "identical held-out content reachable under a different id must be caught"
    );
}

/// Fixed: `exclude_if_contaminated` is a real gate — a contaminated
/// report refuses the evidence outright instead of letting it flow
/// onward with a boolean nobody is required to check.
#[test]
fn vc_201_009_mastery_exclusion_is_a_flag_not_an_exclusion() {
    let held = set(&["h1"]);
    let r = detect(&held, &set(&["h1"]), &set(&[]));
    assert!(r.contaminated);
    assert_eq!(r.reason.as_deref(), Some("held-out overlap: h1"));

    let excluded = exclude_if_contaminated(&r, "evidence-payload")
        .expect_err("a contaminated report must refuse the evidence, not pass it through");
    assert!(excluded.contains("h1"));

    let clean = detect(&held, &set(&[]), &set(&[]));
    assert_eq!(
        exclude_if_contaminated(&clean, "evidence-payload"),
        Ok("evidence-payload")
    );
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

#[test]
fn vc_201_009_mastery_clean_corpus_passes_detect_in_corpus_and_exclusion_gate() {
    let c = corpus(vec![
        fx("h1", "unique held-out input", FixtureSplit::HeldOut),
        fx("t1", "distinct training input", FixtureSplit::Train),
    ]);
    let report = detect_in_corpus(&c, &set(&["t1"]), &set(&[])).unwrap();
    assert!(!report.contaminated);
    assert!(report.reason.is_none());
    assert_eq!(exclude_if_contaminated(&report, 99_u32), Ok(99_u32));
}

fn mastery_ws(tag: &str) -> std::path::PathBuf {
    let root =
        std::env::temp_dir().join(format!("susi-vc009-mastery-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn held_out_corpus() -> RsiCorpus {
    corpus(vec![
        make_fixture(FixtureSpec {
            id: "h1",
            class: FixtureClass::Coding,
            input: "held-out eval input",
            split: FixtureSplit::HeldOut,
            seed: 1,
            evaluator_expected: Some("rubric".into()),
        }),
        fx("t1", "training input", FixtureSplit::Train),
    ])
}

fn clean_run(id: &str) -> crate::eval_separation::EvalRunInput {
    crate::eval_separation::EvalRunInput {
        candidate: crate::eval_separation::CandidateArtifact {
            id: id.into(),
            actual_output: set(&["rubric"]),
            expected_results: set(&["rubric"]),
        },
        candidate_can_write: false,
        // The caller declares a clean footprint — the omission the
        // workspace access log exists to catch.
        training_access: set(&["t1"]),
        memory_access: BTreeSet::new(),
        measured: None,
    }
}

/// Production wiring: a held-out read recorded at the read path lands in
/// `.susi/eval_access_log.jsonl`, and `evaluate_and_record` merges that
/// log into the run's footprint — so a caller whose `EvalRunInput`
/// omits the read is still judged contaminated, durably.
#[test]
fn vc_201_009_mastery_evaluate_and_record_catches_disk_logged_held_out_read() {
    use crate::eval_contamination::{load_recorded_access, record_access};
    use crate::eval_separation::{evaluate, evaluate_and_record, read_all, EvalVerdict};

    let ws = mastery_ws("logged");
    let c = held_out_corpus();

    // Before any logged read, the caller-declared run is clean.
    let before = evaluate_and_record(&ws, &c, &clean_run("cand-before")).unwrap();
    assert_eq!(before.verdict, EvalVerdict::Pass);

    // A memory read of the held-out fixture is recorded on disk, and an
    // unknown split is refused rather than silently dropped.
    record_access(&ws, "memory", "h1").unwrap();
    record_access(&ws, "training", "t1").unwrap();
    assert!(record_access(&ws, "scratch", "h1").is_err());
    assert!(record_access(&ws, "training", "  ").is_err());
    let (train, mem) = load_recorded_access(&ws);
    assert_eq!(train, set(&["t1"]));
    assert_eq!(mem, set(&["h1"]));

    // The same caller-declared footprint is clean on the pure path ...
    assert_eq!(
        evaluate(&c, &clean_run("cand-after")).unwrap().verdict,
        EvalVerdict::Pass
    );
    // ... but the production path merges the access log and refuses it.
    let after = evaluate_and_record(&ws, &c, &clean_run("cand-after")).unwrap();
    assert_eq!(after.verdict, EvalVerdict::FailContaminated);
    assert!(after
        .contamination
        .reason
        .as_deref()
        .unwrap()
        .contains("h1"));
    let recorded = read_all(&ws);
    assert_eq!(recorded.len(), 2);
    assert_eq!(recorded[1].verdict, EvalVerdict::FailContaminated);

    let _ = std::fs::remove_dir_all(&ws);
}

/// A missing or torn access log is an empty footprint, never a panic.
#[test]
fn vc_201_009_mastery_access_log_tolerates_missing_and_torn_lines() {
    use crate::eval_contamination::{load_recorded_access, record_access};

    let ws = mastery_ws("torn");
    let (train, mem) = load_recorded_access(&ws);
    assert!(train.is_empty() && mem.is_empty());

    record_access(&ws, "training", "t1").unwrap();
    let log = ws.join(".susi").join("eval_access_log.jsonl");
    let mut bytes = std::fs::read(&log).unwrap();
    bytes.extend_from_slice(b"{\"split\":\"memory\",\"id\":\"h");
    std::fs::write(&log, bytes).unwrap();
    let (train, mem) = load_recorded_access(&ws);
    assert_eq!(train, set(&["t1"]));
    assert!(mem.is_empty());

    let _ = std::fs::remove_dir_all(&ws);
}
