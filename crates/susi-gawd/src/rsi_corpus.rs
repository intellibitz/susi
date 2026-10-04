//! Versioned RSI evaluation corpus (VC-201-001).
//!
//! Coding, repair, orchestration, and local/cloud ops fixtures with input
//! hashes and held-out splits. Replay uses recorded seeds; evaluator-only
//! fields never appear in the candidate view.
//!
//! Integrity is enforced at the boundaries: [`RsiCorpus::from_json`] parses
//! *and* verifies (schema, revision, unique fixture ids, per-fixture input
//! hash), `split` is the typed [`FixtureSplit`] so a mislabeled held-out
//! fixture fails deserialization instead of escaping into the train side,
//! and [`RsiCorpus::replay_seeds`] is bound to the revision — replaying
//! "the same revision" requires naming it, and a wrong revision is an
//! error rather than an indistinguishable plan.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Corpus schema this binary understands.
pub const RSI_CORPUS_SCHEMA: &str = "susi.rsi_corpus/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureClass {
    Coding,
    Repair,
    Orchestration,
    LocalOps,
    CloudOps,
}

/// Evaluation split. Typed so a label like `held-out` cannot deserialize:
/// a fixture is either training data or part of the evaluator's held-out
/// suite — there is no third string that silently joins the train side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureSplit {
    Train,
    HeldOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusFixture {
    pub id: String,
    pub class: FixtureClass,
    pub input: String,
    /// SHA-256 hex of `input` (canonical UTF-8 bytes). Verified against
    /// `input` by [`RsiCorpus::verify_integrity`]; [`RsiCorpus::from_json`]
    /// refuses a corpus where it is stale.
    pub input_hash: String,
    pub split: FixtureSplit,
    /// Seed recorded for deterministic replay.
    pub seed: u64,
    /// Evaluator-only expected signal — never shown to the candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluator_expected: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RsiCorpus {
    pub schema_version: String,
    pub revision: String,
    pub fixtures: Vec<CorpusFixture>,
}

/// Why a corpus — or a replay request against it — is untrustworthy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorpusIntegrityError {
    /// The document is not a well-formed `RsiCorpus` (covers an
    /// unrecognized `split` label or `class`).
    MalformedDocument(String),
    /// `schema_version` names a schema this binary does not understand.
    UnsupportedSchema {
        expected: String,
        found: String,
    },
    /// Replays and diffs are meaningless without a revision.
    EmptyRevision,
    EmptyFixtureId,
    DuplicateFixtureId(String),
    /// The stored `input_hash` does not match the stored `input` — the
    /// fixture was corrupted or tampered with after hashing.
    InputHashMismatch {
        fixture_id: String,
        declared: String,
        computed: String,
    },
    /// The caller asked to replay a revision this corpus is not.
    RevisionMismatch {
        corpus_revision: String,
        requested: String,
    },
}

impl fmt::Display for CorpusIntegrityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorpusIntegrityError::MalformedDocument(e) => {
                write!(f, "malformed corpus document: {e}")
            }
            CorpusIntegrityError::UnsupportedSchema { expected, found } => {
                write!(f, "unsupported corpus schema {found:?} (expected {expected:?})")
            }
            CorpusIntegrityError::EmptyRevision => write!(f, "corpus revision is empty"),
            CorpusIntegrityError::EmptyFixtureId => {
                write!(f, "corpus fixture id is empty")
            }
            CorpusIntegrityError::DuplicateFixtureId(id) => {
                write!(f, "duplicate corpus fixture id {id:?}")
            }
            CorpusIntegrityError::InputHashMismatch {
                fixture_id,
                declared,
                computed,
            } => write!(
                f,
                "fixture {fixture_id:?} input_hash {declared} does not match input (computed {computed})"
            ),
            CorpusIntegrityError::RevisionMismatch {
                corpus_revision,
                requested,
            } => write!(
                f,
                "cannot replay revision {requested:?} against corpus revision {corpus_revision:?}"
            ),
        }
    }
}

impl std::error::Error for CorpusIntegrityError {}

impl RsiCorpus {
    #[must_use]
    pub fn hash_input(input: &str) -> String {
        hex::encode(Sha256::digest(input.as_bytes()))
    }

    /// The deserialization boundary: parse a corpus document and verify
    /// its integrity before anything can consume it. A document that
    /// parses but fails verification is refused here, so downstream code
    /// never sees a tampered corpus from a load.
    pub fn from_json(json: &str) -> Result<Self, CorpusIntegrityError> {
        let corpus: RsiCorpus = serde_json::from_str(json)
            .map_err(|e| CorpusIntegrityError::MalformedDocument(e.to_string()))?;
        corpus.verify_integrity()?;
        Ok(corpus)
    }

    /// Whole-corpus integrity check: understood schema, named revision,
    /// unique non-empty fixture ids, and every `input_hash` recomputed
    /// against its `input`. Structs built by hand (rather than through
    /// [`RsiCorpus::from_json`]) can be verified with this directly; the
    /// evaluation entry points call it before consuming the corpus.
    pub fn verify_integrity(&self) -> Result<(), CorpusIntegrityError> {
        if self.schema_version != RSI_CORPUS_SCHEMA {
            return Err(CorpusIntegrityError::UnsupportedSchema {
                expected: RSI_CORPUS_SCHEMA.into(),
                found: self.schema_version.clone(),
            });
        }
        if self.revision.trim().is_empty() {
            return Err(CorpusIntegrityError::EmptyRevision);
        }
        let mut seen = BTreeSet::new();
        for f in &self.fixtures {
            if f.id.is_empty() {
                return Err(CorpusIntegrityError::EmptyFixtureId);
            }
            if !seen.insert(f.id.as_str()) {
                return Err(CorpusIntegrityError::DuplicateFixtureId(f.id.clone()));
            }
            let computed = Self::hash_input(&f.input);
            if f.input_hash != computed {
                return Err(CorpusIntegrityError::InputHashMismatch {
                    fixture_id: f.id.clone(),
                    declared: f.input_hash.clone(),
                    computed,
                });
            }
        }
        Ok(())
    }

    /// Candidate-facing view: strips `evaluator_expected`.
    #[must_use]
    pub fn candidate_view(&self) -> RsiCorpus {
        let fixtures = self
            .fixtures
            .iter()
            .map(|f| CorpusFixture {
                evaluator_expected: None,
                ..f.clone()
            })
            .collect();
        RsiCorpus {
            schema_version: self.schema_version.clone(),
            revision: self.revision.clone(),
            fixtures,
        }
    }

    /// Evaluator-side fixtures.
    #[must_use]
    pub fn held_out(&self) -> Vec<&CorpusFixture> {
        self.fixtures
            .iter()
            .filter(|f| f.split == FixtureSplit::HeldOut)
            .collect()
    }

    /// Training-side fixtures — everything the evaluator must keep
    /// separate from `held_out`.
    #[must_use]
    pub fn train(&self) -> Vec<&CorpusFixture> {
        self.fixtures
            .iter()
            .filter(|f| f.split == FixtureSplit::Train)
            .collect()
    }

    /// Ids of the held-out fixtures, for contamination detection.
    #[must_use]
    pub fn held_out_ids(&self) -> BTreeSet<String> {
        self.held_out().iter().map(|f| f.id.clone()).collect()
    }

    /// Replay plan for one named revision: fixture id → seed (no
    /// evaluator data). The corpus must verify and `revision` must equal
    /// the corpus's own — "replay the same revision" is thereby
    /// distinguishable from replaying any other, and a tampered corpus
    /// cannot be replayed at all.
    pub fn replay_seeds(
        &self,
        revision: &str,
    ) -> Result<BTreeMap<String, u64>, CorpusIntegrityError> {
        if revision != self.revision {
            return Err(CorpusIntegrityError::RevisionMismatch {
                corpus_revision: self.revision.clone(),
                requested: revision.into(),
            });
        }
        self.verify_integrity()?;
        Ok(self
            .fixtures
            .iter()
            .map(|f| (f.id.clone(), f.seed))
            .collect())
    }
}

/// Inputs for [`make_fixture`].
pub struct FixtureSpec<'a> {
    pub id: &'a str,
    pub class: FixtureClass,
    pub input: &'a str,
    pub split: FixtureSplit,
    pub seed: u64,
    pub evaluator_expected: Option<String>,
}

/// Build a fixture, computing `input_hash`.
#[must_use]
pub fn make_fixture(spec: FixtureSpec<'_>) -> CorpusFixture {
    CorpusFixture {
        id: spec.id.to_string(),
        class: spec.class,
        input: spec.input.to_string(),
        input_hash: RsiCorpus::hash_input(spec.input),
        split: spec.split,
        seed: spec.seed,
        evaluator_expected: spec.evaluator_expected,
    }
}
