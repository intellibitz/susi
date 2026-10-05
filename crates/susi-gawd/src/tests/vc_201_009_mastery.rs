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
    }
}

/// Still true: `detect()` is a pure id-set comparison, and a real access
/// log that simply never records a held-out read is indistinguishable
/// from a clean run at this layer — tracking *access* (not a
/// caller-assembled list) has to live upstream of this function, at the
/// read path itself. This falsification stands as a documented residual
/// gap, not something `detect`/`detect_in_corpus` can close by
/// construction.
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
