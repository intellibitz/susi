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
    // Residual, by construction: `detect` is a pure id-set comparison —
    // a real access log that never records a held-out read is
    // indistinguishable from a clean run at this layer. Real access
    // tracking has to live upstream, at the read path itself.
    let held = set(&["h1"]);
    let unrecorded = detect(&held, &set(&["t1"]), &set(&["m1"]));
    assert!(!unrecorded.contaminated);

    // Fixed: content contamination under a different id is caught via
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
        ],
        promotion_spec: None,
    };
    let report = detect_in_corpus(&c, &set(&["h1-copy"]), &set(&[])).unwrap();
    assert!(
        report.contaminated,
        "identical held-out content reachable under a different id must be caught"
    );

    // Fixed: exclude_if_contaminated is a real gate, not an unconsumed flag.
    let overlap = detect(&held, &set(&["h1"]), &set(&[]));
    assert!(overlap.contaminated);
    exclude_if_contaminated(&overlap, "evidence-payload")
        .expect_err("a contaminated report must refuse the evidence, not pass it through");
    let clean = detect(&held, &set(&[]), &set(&[]));
    assert_eq!(
        exclude_if_contaminated(&clean, "evidence-payload"),
        Ok("evidence-payload")
    );
}
