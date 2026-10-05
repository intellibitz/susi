//! Mastery verification for VC-201-001: a versioned RSI evaluation corpus
//! with input hashes, held-out splits and recorded-seed replay.
//!
//! The cited tests show the structures hold the fields. The distinguishing
//! properties verified here are that hashes are *verified* (a tampered
//! corpus is refused at the deserialization boundary and by every eval
//! entry point, not carried silently), that held-out membership is
//! enforced by the type system (a mislabeled split cannot deserialize, so
//! it cannot leak into the train side), and that replay is bound to the
//! revision it claims.

use std::collections::BTreeSet;

use crate::eval_contamination::detect_in_corpus;
use crate::eval_separation::{judge, CandidateArtifact, HeldOutSuite, PromotionGate};
use crate::rsi_corpus::{
    make_fixture, CorpusIntegrityError, FixtureClass, FixtureSpec, FixtureSplit, RsiCorpus,
    RSI_CORPUS_SCHEMA,
};

fn fx(id: &str, input: &str, split: FixtureSplit, seed: u64) -> crate::rsi_corpus::CorpusFixture {
    make_fixture(FixtureSpec {
        id,
        class: FixtureClass::Coding,
        input,
        split,
        seed,
        evaluator_expected: Some("rubric".into()),
    })
}

fn corpus(revision: &str, fixtures: Vec<crate::rsi_corpus::CorpusFixture>) -> RsiCorpus {
    RsiCorpus {
        schema_version: RSI_CORPUS_SCHEMA.into(),
        revision: revision.into(),
        fixtures,
        promotion_spec: None,
    }
}

/// A fixture whose input changed while its hash stayed stale is detected:
/// `verify_integrity` names the mismatch, and `from_json` — the
/// deserialization boundary — refuses the tampered document outright, so
/// no accessor can serve it downstream.
#[test]
fn vc_201_001_mastery_tampered_input_hash_is_rejected() {
    let mut f = fx("c1", "fn main() {}", FixtureSplit::HeldOut, 7);
    f.input = "fn main() { malicious() }".into(); // input changed, hash stale
    let c = corpus("rev-1", vec![f]);
    assert_eq!(
        c.verify_integrity(),
        Err(CorpusIntegrityError::InputHashMismatch {
            fixture_id: "c1".into(),
            declared: c.fixtures[0].input_hash.clone(),
            computed: RsiCorpus::hash_input(&c.fixtures[0].input),
        })
    );
    let json = serde_json::to_string(&c).unwrap();
    assert!(
        matches!(
            RsiCorpus::from_json(&json),
            Err(CorpusIntegrityError::InputHashMismatch { .. })
        ),
        "the load boundary refuses a tampered corpus"
    );
    // A tampered corpus cannot be replayed either.
    assert!(c.replay_seeds("rev-1").is_err());
}

/// `split` is typed: a document labeling a fixture `held-out` cannot
/// deserialize at all, so a mislabeled held-out fixture has no path to
/// the train side. The correctly labeled document loads and `held_out`
/// finds exactly its held-out fixture.
#[test]
fn vc_201_001_mastery_mislabeled_split_cannot_deserialize() {
    let good = corpus(
        "rev-1",
        vec![
            fx("good", "a", FixtureSplit::HeldOut, 1),
            fx("trn", "b", FixtureSplit::Train, 2),
        ],
    );
    let json = serde_json::to_string(&good).unwrap();
    let mislabeled = json.replace("\"held_out\"", "\"held-out\"");
    assert_ne!(json, mislabeled, "the fixture reached the document");
    assert!(
        matches!(
            RsiCorpus::from_json(&mislabeled),
            Err(CorpusIntegrityError::MalformedDocument(_))
        ),
        "an unrecognized split label fails deserialization"
    );
    let loaded = RsiCorpus::from_json(&json).unwrap();
    assert_eq!(loaded.held_out().len(), 1);
    assert_eq!(loaded.held_out()[0].id, "good");
    assert_eq!(loaded.train().len(), 1);
    assert!(loaded.held_out_ids().contains("good"));
}

/// Replay is bound to the revision: the seeds for 'the same revision'
/// come only from the corpus carrying that revision. A wrong revision is
/// a `RevisionMismatch`, so replaying rev-A is distinguishable from
/// replaying rev-B.
#[test]
fn vc_201_001_mastery_replay_is_bound_to_revision() {
    let a = corpus("rev-A", vec![fx("x", "i", FixtureSplit::Train, 1)]);
    let b = corpus("rev-B", vec![fx("x", "i", FixtureSplit::Train, 1)]);
    assert_eq!(a.replay_seeds("rev-A").unwrap().get("x"), Some(&1));
    assert_eq!(
        a.replay_seeds("rev-B"),
        Err(CorpusIntegrityError::RevisionMismatch {
            corpus_revision: "rev-A".into(),
            requested: "rev-B".into(),
        })
    );
    assert!(matches!(
        b.replay_seeds("rev-A"),
        Err(CorpusIntegrityError::RevisionMismatch { .. })
    ));
}

/// The corpus is consumed on the production eval path: the held-out
/// split feeds the judge's suite, held-out ids feed contamination
/// detection, and both verify integrity before consuming — a tampered
/// corpus cannot seed an evaluation or a contamination verdict.
#[test]
fn vc_201_001_mastery_corpus_feeds_the_eval_path() {
    let c = corpus(
        "rev-5",
        vec![
            fx("h1", "hidden eval input", FixtureSplit::HeldOut, 11),
            fx("t1", "training input", FixtureSplit::Train, 12),
        ],
    );

    // Corpus → HeldOutSuite → judge: expected signals are the evaluator's.
    let suite = HeldOutSuite::from_corpus(&c).unwrap();
    assert_eq!(suite.inputs, vec!["hidden eval input".to_string()]);
    assert!(suite.expected.contains("rubric"));
    let candidate = CandidateArtifact {
        id: "cand".into(),
        actual_output: BTreeSet::from(["rubric".to_string()]),
        expected_results: BTreeSet::from(["rubric".to_string()]),
    };
    assert_eq!(judge(&candidate, &suite, false), PromotionGate::Pass);

    // Corpus → contamination detection: a held-out id seen on the train
    // side is reported; a clean access set is not.
    let report =
        detect_in_corpus(&c, &BTreeSet::from(["h1".to_string()]), &BTreeSet::new()).unwrap();
    assert!(report.contaminated);
    let clean = detect_in_corpus(&c, &BTreeSet::new(), &BTreeSet::new()).unwrap();
    assert!(!clean.contaminated);

    // Integrity gates both entry points.
    let mut tampered = c.clone();
    tampered.fixtures[0].input = "swapped".into();
    assert!(HeldOutSuite::from_corpus(&tampered).is_err());
    assert!(detect_in_corpus(&tampered, &BTreeSet::new(), &BTreeSet::new()).is_err());
}

/// What still holds: the candidate view strips evaluator_expected while
/// keeping seeds, and held_out() selects the typed label.
#[test]
fn vc_201_001_mastery_candidate_view_hides_evaluator_data() {
    let c = corpus("rev-2", vec![fx("o1", "plan", FixtureSplit::Train, 99)]);
    let view = c.candidate_view();
    assert!(view.fixtures[0].evaluator_expected.is_none());
    assert_eq!(view.replay_seeds("rev-2").unwrap().get("o1"), Some(&99));
    assert_eq!(view.held_out().len(), 0);
}
