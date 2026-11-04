//! Evaluation contamination detection (VC-201-009).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContaminationReport {
    pub contaminated: bool,
    pub reason: Option<String>,
}

/// Every string a corpus-aware access set resolves to for one accessed
/// id: the id itself, plus — when it names a known corpus fixture — that
/// fixture's content hash too. Comparing on hashes as well as ids is
/// what catches identical held-out content reachable under a different
/// id (a duplicated or re-exported fixture), not just a matching id.
fn resolve_identifiers(corpus: &crate::rsi_corpus::RsiCorpus, id: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    out.insert(id.to_string());
    if let Some(f) = corpus.fixtures.iter().find(|f| f.id == id) {
        out.insert(f.input_hash.clone());
    }
    out
}

/// Detect contamination of a corpus's held-out split. The corpus is
/// verified first: a corpus whose held-out membership or inputs cannot
/// be trusted cannot ground a contamination verdict, so integrity
/// failure is an error rather than a clean report.
///
/// Held-out fixtures are identified by id *and* content hash, and every
/// accessed id that names a known corpus fixture is resolved the same
/// way before comparing — so a held-out fixture's content reachable
/// under a different id (e.g. duplicated into the train split) is still
/// caught, not just a literal id match.
pub fn detect_in_corpus(
    corpus: &crate::rsi_corpus::RsiCorpus,
    training_access: &BTreeSet<String>,
    memory_access: &BTreeSet<String>,
) -> Result<ContaminationReport, crate::rsi_corpus::CorpusIntegrityError> {
    corpus.verify_integrity()?;
    let mut held_out: BTreeSet<String> = BTreeSet::new();
    for f in corpus.held_out() {
        held_out.insert(f.id.clone());
        held_out.insert(f.input_hash.clone());
    }
    let training_resolved: BTreeSet<String> = training_access
        .iter()
        .flat_map(|id| resolve_identifiers(corpus, id))
        .collect();
    let memory_resolved: BTreeSet<String> = memory_access
        .iter()
        .flat_map(|id| resolve_identifiers(corpus, id))
        .collect();
    Ok(detect(&held_out, &training_resolved, &memory_resolved))
}

/// A run is contaminated when held-out fixture ids appear in train/memory access sets.
#[must_use]
pub fn detect(
    held_out: &BTreeSet<String>,
    training_access: &BTreeSet<String>,
    memory_access: &BTreeSet<String>,
) -> ContaminationReport {
    let mut overlap: Vec<_> = held_out.intersection(training_access).cloned().collect();
    overlap.extend(held_out.intersection(memory_access).cloned());
    overlap.sort();
    overlap.dedup();
    if overlap.is_empty() {
        ContaminationReport {
            contaminated: false,
            reason: None,
        }
    } else {
        ContaminationReport {
            contaminated: true,
            reason: Some(format!("held-out overlap: {}", overlap.join(","))),
        }
    }
}

/// The only sanctioned way to use a [`ContaminationReport`]: a
/// contaminated run's evidence is refused outright rather than flowing
/// onward with a `contaminated` flag attached that a caller might not
/// check. Call this instead of inspecting `report.contaminated` directly.
pub fn exclude_if_contaminated<T>(report: &ContaminationReport, evidence: T) -> Result<T, String> {
    if report.contaminated {
        Err(report
            .reason
            .clone()
            .unwrap_or_else(|| "contaminated run excluded from improvement evidence".into()))
    } else {
        Ok(evidence)
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use crate::rsi_corpus::{
        CorpusFixture, FixtureClass, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
    };
    use sha2::{Digest, Sha256};

    fn hash(s: &str) -> String {
        hex::encode(Sha256::digest(s.as_bytes()))
    }

    #[test]
    fn detect_returns_clean_report_when_held_out_is_disjoint() {
        let held_out = BTreeSet::from(["hold-1".to_string(), "hold-2".to_string()]);
        let train = BTreeSet::from(["train-1".to_string()]);
        let mem = BTreeSet::from(["mem-1".to_string()]);
        let report = detect(&held_out, &train, &mem);
        assert!(!report.contaminated);
        assert_eq!(report.reason, None);
        assert_eq!(exclude_if_contaminated(&report, 42), Ok(42));
    }

    #[test]
    fn detect_reports_sorted_deduplicated_overlap_across_train_and_memory() {
        let held_out = BTreeSet::from(["h-b".to_string(), "h-a".to_string()]);
        let train = BTreeSet::from(["h-b".to_string(), "train-1".to_string()]);
        let mem = BTreeSet::from(["h-a".to_string(), "h-b".to_string()]);
        let report = detect(&held_out, &train, &mem);
        assert!(report.contaminated);
        assert_eq!(report.reason.as_deref(), Some("held-out overlap: h-a,h-b"));
        assert!(exclude_if_contaminated(&report, "score").is_err());
    }

    #[test]
    fn exclude_if_contaminated_uses_default_message_when_reason_is_none() {
        let report = ContaminationReport {
            contaminated: true,
            reason: None,
        };
        let err = exclude_if_contaminated(&report, 1).unwrap_err();
        assert_eq!(err, "contaminated run excluded from improvement evidence");
    }

    #[test]
    fn detect_in_corpus_catches_duplicated_held_out_content_under_different_fixture_id() {
        let secret_input = "held-out prompt body";
        let secret_hash = hash(secret_input);
        let corpus = RsiCorpus {
            schema_version: RSI_CORPUS_SCHEMA.to_string(),
            revision: "rev-1".to_string(),
            fixtures: vec![
                CorpusFixture {
                    id: "train-dup".to_string(),
                    class: FixtureClass::Coding,
                    input: secret_input.to_string(),
                    input_hash: secret_hash.clone(),
                    split: FixtureSplit::Train,
                    seed: 1,
                    evaluator_expected: None,
                },
                CorpusFixture {
                    id: "hold-orig".to_string(),
                    class: FixtureClass::Coding,
                    input: secret_input.to_string(),
                    input_hash: secret_hash,
                    split: FixtureSplit::HeldOut,
                    seed: 2,
                    evaluator_expected: None,
                },
            ],
            promotion_spec: None,
        };
        let train_access = BTreeSet::from(["train-dup".to_string()]);
        let report = detect_in_corpus(&corpus, &train_access, &BTreeSet::new()).unwrap();
        assert!(report.contaminated);
    }
}
