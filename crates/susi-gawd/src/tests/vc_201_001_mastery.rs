//! Mastery verification for VC-201-001: a versioned RSI evaluation corpus
//! with input hashes, held-out splits and recorded-seed replay.
//!
//! The cited tests show the structures hold the fields. The distinguishing
//! properties are that hashes are *verified* (a tampered corpus is
//! detected, not carried silently), that held-out membership is enforced
//! (a mislabeled split cannot leak an evaluator fixture into the train
//! side), and that replay is bound to the revision it claims.

use crate::rsi_corpus::{make_fixture, FixtureClass, FixtureSpec, RsiCorpus, RSI_CORPUS_SCHEMA};

fn fx(id: &str, input: &str, split: &str, seed: u64) -> crate::rsi_corpus::CorpusFixture {
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
    }
}

/// Falsification: nothing verifies `input_hash`. A corpus deserialized with
/// a hash that does not match its input is accepted wholesale — there is
/// no integrity check on the corpus at all, so a corrupted or tampered
/// fixture is indistinguishable from an honest one.
#[test]
fn vc_201_001_mastery_tampered_input_hash_is_accepted() {
    let mut f = fx("c1", "fn main() {}", "held_out", 7);
    f.input = "fn main() { malicious() }".into(); // input changed, hash stale
    let c = corpus("rev-1", vec![f]);
    // No validation API exists; every accessor happily serves the tampered
    // fixture as if its hash still proved the input.
    assert_eq!(c.held_out().len(), 1);
    assert_ne!(
        c.fixtures[0].input_hash,
        RsiCorpus::hash_input(&c.fixtures[0].input),
        "the stored hash does not match the stored input — undetected"
    );
}

/// Falsification: `split` is a free-form string. A held-out fixture
/// mislabeled `held-out` (a single character off) vanishes from
/// `held_out()` — it would be replayed on the train side, contaminating
/// the held-out split the evaluator believes is isolated.
#[test]
fn vc_201_001_mastery_mislabeled_split_escapes_held_out() {
    let c = corpus(
        "rev-1",
        vec![
            fx("good", "a", "held_out", 1),
            fx("typo", "b", "held-out", 2),
        ],
    );
    assert_eq!(c.held_out().len(), 1);
    assert!(
        !c.held_out().iter().any(|f| f.id == "typo"),
        "a mislabeled held-out fixture is not held out"
    );
}

/// Falsification: replay is not bound to the revision. `replay_seeds`
/// ignores `revision` entirely, so replaying 'the same revision' cannot
/// be told apart from replaying a different one — the version field is
/// decorative.
#[test]
fn vc_201_001_mastery_replay_ignores_revision() {
    let a = corpus("rev-A", vec![fx("x", "i", "train", 1)]);
    let b = corpus("rev-B", vec![fx("x", "i", "train", 1)]);
    assert_eq!(
        a.replay_seeds(),
        b.replay_seeds(),
        "two different revisions produce identical replay plans"
    );
}

/// What does hold: the candidate view strips evaluator_expected while
/// keeping seeds, and held_out() selects the exact label.
#[test]
fn vc_201_001_mastery_candidate_view_hides_evaluator_data() {
    let c = corpus("rev-2", vec![fx("o1", "plan", "train", 99)]);
    let view = c.candidate_view();
    assert!(view.fixtures[0].evaluator_expected.is_none());
    assert_eq!(view.replay_seeds().get("o1"), Some(&99));
    assert_eq!(view.held_out().len(), 0);
}
