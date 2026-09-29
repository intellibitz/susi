//! Versioned RSI evaluation corpus (VC-201-001).
//!
//! Coding, repair, orchestration, and local/cloud ops fixtures with input
//! hashes and held-out splits. Replay uses recorded seeds; evaluator-only
//! fields never appear in the candidate view.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusFixture {
    pub id: String,
    pub class: FixtureClass,
    pub input: String,
    /// SHA-256 hex of `input` (canonical UTF-8 bytes).
    pub input_hash: String,
    /// Split label: `train` | `held_out`.
    pub split: String,
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

impl RsiCorpus {
    #[must_use]
    pub fn hash_input(input: &str) -> String {
        hex::encode(Sha256::digest(input.as_bytes()))
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

    #[must_use]
    pub fn held_out(&self) -> Vec<&CorpusFixture> {
        self.fixtures
            .iter()
            .filter(|f| f.split == "held_out")
            .collect()
    }

    /// Replay plan for a revision: fixture id → seed (no evaluator data).
    #[must_use]
    pub fn replay_seeds(&self) -> BTreeMap<String, u64> {
        self.fixtures
            .iter()
            .map(|f| (f.id.clone(), f.seed))
            .collect()
    }
}

/// Inputs for [`make_fixture`].
pub struct FixtureSpec<'a> {
    pub id: &'a str,
    pub class: FixtureClass,
    pub input: &'a str,
    pub split: &'a str,
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
        split: spec.split.to_string(),
        seed: spec.seed,
        evaluator_expected: spec.evaluator_expected,
    }
}
