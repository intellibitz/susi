#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! VC-201-009: track evaluation-data access and overlaps between
//! training, memory, and held-out fixtures; a contaminated run is
//! excluded from improvement evidence with an inspectable reason.
//!
//! The full falsification suite lives at
//! `crates/susi-gawd/src/tests/vc_201_009_mastery.rs` (exercised by
//! `cargo nextest run -p susi-gawd -E test(vc_201_009_mastery)`); this is
//! the root-package integration test the task's own bare accept command
//! (`cargo nextest run -E test(vc_201_009_mastery)`) actually discovers —
//! bare `nextest run` without `-p`/`--workspace` only discovers the root
//! `susi` package's own `tests/*.rs`, never a member crate's unit tests.

use std::collections::BTreeSet;
use susi_gawd::eval_contamination::{detect, detect_in_corpus, exclude_if_contaminated};
use susi_gawd::rsi_corpus::{
    make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
};

fn set(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

#[test]
fn vc_201_009_mastery() {
    // 1. Residual documentation: unrecorded access is undetectable at this layer
    let held = set(&["h1"]);
    let unrecorded = detect(&held, &set(&["t1"]), &set(&["m1"]));
    assert!(!unrecorded.contaminated);
    assert_eq!(unrecorded.reason, None);

    // 2. Fixed: content contamination under a different id is caught via
    // detect_in_corpus's hash resolution, not just a literal id match.
    let c = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-1".into(),
        fixtures: vec![
            make_fixture(FixtureSpec {
                id: "h1",
                class: FixtureClass::Coding,
                input: "the secret held-out input",
                split: FixtureSplit::HeldOut,
                seed: 1,
                evaluator_expected: None,
            }),
            make_fixture(FixtureSpec {
                id: "h1-copy",
                class: FixtureClass::Coding,
                input: "the secret held-out input",
                split: FixtureSplit::Train,
                seed: 1,
                evaluator_expected: None,
            }),
            make_fixture(FixtureSpec {
                id: "h2",
                class: FixtureClass::Coding,
                input: "another secret held-out input",
                split: FixtureSplit::HeldOut,
                seed: 2,
                evaluator_expected: None,
            }),
        ],
        promotion_spec: None,
    };
    let report = detect_in_corpus(&c, &set(&["h1-copy"]), &set(&[])).unwrap();
    assert!(
        report.contaminated,
        "identical held-out content reachable under a different id must be caught"
    );

    // 3. Multiple overlaps in training and memory are sorted and deduplicated in the reason
    let multi_overlap = detect(
        &set(&["h1", "h2", "h3"]),
        &set(&["h3", "h1"]),
        &set(&["h1", "h2"]),
    );
    assert!(multi_overlap.contaminated);
    assert_eq!(
        multi_overlap.reason.as_deref(),
        Some("held-out overlap: h1,h2,h3")
    );

    // 4. Memory-only contamination is caught
    let memory_overlap = detect(&held, &set(&["safe_train"]), &set(&["h1"]));
    assert!(memory_overlap.contaminated);
    assert_eq!(
        memory_overlap.reason.as_deref(),
        Some("held-out overlap: h1")
    );

    // 5. Fixed: exclude_if_contaminated is a real gate, not an unconsumed flag.
    let overlap = detect(&held, &set(&["h1"]), &set(&[]));
    assert!(overlap.contaminated);
    let err = exclude_if_contaminated(&overlap, "evidence-payload")
        .expect_err("a contaminated report must refuse the evidence, not pass it through");
    assert!(err.contains("held-out overlap: h1"));

    // 6. Clean runs pass the gate unmodified
    let clean = detect(&held, &set(&["safe1"]), &set(&["safe2"]));
    assert!(!clean.contaminated);
    assert_eq!(clean.reason, None);
    assert_eq!(
        exclude_if_contaminated(&clean, "evidence-payload"),
        Ok("evidence-payload")
    );

    // 7. Corpus integrity verification is enforced prior to contamination analysis
    let mut corrupted_corpus = c;
    corrupted_corpus.fixtures[0].input_hash = "tampered_hash".into();
    let integrity_err = detect_in_corpus(&corrupted_corpus, &set(&["h1-copy"]), &set(&[]));
    assert!(
        integrity_err.is_err(),
        "corrupted corpus must fail integrity check rather than report clean"
    );
}
