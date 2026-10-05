//! Tests for RSI evaluation corpus (`vc_201_001_*`).

use crate::rsi_corpus::{
    make_fixture, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
};

#[test]
fn vc_201_001_input_hash_and_held_out_split() {
    let f = make_fixture(FixtureSpec {
        id: "c1",
        class: FixtureClass::Coding,
        input: "fn main() {}",
        split: FixtureSplit::HeldOut,
        seed: 7,
        evaluator_expected: Some("compiles".into()),
    });
    assert_eq!(f.input_hash, RsiCorpus::hash_input("fn main() {}"));
    assert_eq!(f.split, FixtureSplit::HeldOut);
    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-1".into(),
        fixtures: vec![
            f,
            make_fixture(FixtureSpec {
                id: "r1",
                class: FixtureClass::Repair,
                input: "fix the borrow",
                split: FixtureSplit::Train,
                seed: 1,
                evaluator_expected: Some("patch".into()),
            }),
        ],
        promotion_spec: None,
    };
    assert_eq!(corpus.held_out().len(), 1);
    assert_eq!(corpus.held_out()[0].id, "c1");
    assert_eq!(corpus.train().len(), 1);
    assert_eq!(corpus.train()[0].id, "r1");
    assert!(corpus.verify_integrity().is_ok());
}

#[test]
fn vc_201_001_candidate_view_hides_evaluator_expected() {
    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-2".into(),
        fixtures: vec![make_fixture(FixtureSpec {
            id: "o1",
            class: FixtureClass::Orchestration,
            input: "plan a swarm",
            split: FixtureSplit::Train,
            seed: 99,
            evaluator_expected: Some("secret-rubric".into()),
        })],
        promotion_spec: None,
    };
    let view = corpus.candidate_view();
    assert!(view.fixtures[0].evaluator_expected.is_none());
    assert!(corpus.fixtures[0].evaluator_expected.is_some());
    let seeds = view.replay_seeds("rev-2").unwrap();
    assert_eq!(seeds.get("o1"), Some(&99));
}

#[test]
fn vc_201_001_replay_seeds_cover_all_classes() {
    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-3".into(),
        fixtures: vec![
            make_fixture(FixtureSpec {
                id: "a",
                class: FixtureClass::LocalOps,
                input: "status",
                split: FixtureSplit::Train,
                seed: 1,
                evaluator_expected: None,
            }),
            make_fixture(FixtureSpec {
                id: "b",
                class: FixtureClass::CloudOps,
                input: "scout",
                split: FixtureSplit::HeldOut,
                seed: 2,
                evaluator_expected: None,
            }),
        ],
        promotion_spec: None,
    };
    let seeds = corpus.replay_seeds("rev-3").unwrap();
    assert_eq!(seeds.len(), 2);
}

#[test]
fn vc_201_001_from_json_round_trip_verified() {
    let corpus = RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: "rev-4".into(),
        fixtures: vec![make_fixture(FixtureSpec {
            id: "j1",
            class: FixtureClass::Coding,
            input: "impl Display",
            split: FixtureSplit::HeldOut,
            seed: 3,
            evaluator_expected: Some("prints".into()),
        })],
        promotion_spec: None,
    };
    let json = serde_json::to_string(&corpus).unwrap();
    let loaded = RsiCorpus::from_json(&json).unwrap();
    assert_eq!(loaded, corpus);
    assert_eq!(loaded.held_out_ids().iter().next().unwrap(), "j1");
}
