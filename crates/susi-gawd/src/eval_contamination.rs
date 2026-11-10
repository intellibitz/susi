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

/// One durable read recorded at the fixture read path — the access log
/// `evaluate_and_record` merges into every run's footprint, so a held-out
/// read is caught even when the caller's `EvalRunInput` omits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRecord {
    /// `training` or `memory`.
    pub split: String,
    /// The fixture id (or content hash) that was read.
    pub id: String,
}

/// Split label for reads made while producing training material.
pub const ACCESS_SPLIT_TRAINING: &str = "training";
/// Split label for reads served from agent memory.
pub const ACCESS_SPLIT_MEMORY: &str = "memory";

fn access_log_path(workspace: &std::path::Path) -> std::path::PathBuf {
    workspace.join(".susi").join("eval_access_log.jsonl")
}

/// Append one fixture read to `workspace/.susi/eval_access_log.jsonl`
/// under the `eval_access` file lock. `split` must be `training` or
/// `memory`: an unknown split would be silently ignored on load, which is
/// exactly the unrecorded read this log exists to prevent.
pub fn record_access(
    workspace: &std::path::Path,
    split: &str,
    fixture_id: &str,
) -> crate::susi_error::EaiResult<()> {
    use crate::susi_error::EaiError;
    use std::io::Write;
    if split != ACCESS_SPLIT_TRAINING && split != ACCESS_SPLIT_MEMORY {
        return Err(EaiError::governance(format!(
            "eval access split must be training or memory, got {split:?}"
        )));
    }
    if fixture_id.trim().is_empty() {
        return Err(EaiError::governance(
            "eval access fixture id must be nonblank".to_string(),
        ));
    }
    let susi_dir = workspace.join(".susi");
    std::fs::create_dir_all(&susi_dir)
        .map_err(|e| EaiError::filesystem(format!("eval access dir create failed: {e}")))?;
    let mut line = serde_json::to_vec(&AccessRecord {
        split: split.to_string(),
        id: fixture_id.to_string(),
    })
    .map_err(|e| EaiError::internal(format!("eval access serialization failed: {e}")))?;
    line.push(b'\n');
    let _lock = crate::susi_config::file_lock::FileLock::acquire(&susi_dir, "eval_access")
        .ok_or_else(|| EaiError::filesystem("eval access lock acquisition failed".to_string()))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(access_log_path(workspace))
        .map_err(|e| EaiError::filesystem(format!("eval access log open failed: {e}")))?;
    file.write_all(&line)
        .map_err(|e| EaiError::filesystem(format!("eval access log write failed: {e}")))
}

/// Every read recorded in the workspace access log, as
/// `(training_ids, memory_ids)`. A missing log is an empty footprint;
/// unparseable lines (a torn final append) are skipped.
#[must_use]
pub fn load_recorded_access(workspace: &std::path::Path) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut training = BTreeSet::new();
    let mut memory = BTreeSet::new();
    let Ok(content) = std::fs::read_to_string(access_log_path(workspace)) else {
        return (training, memory);
    };
    for rec in content
        .lines()
        .filter_map(|l| serde_json::from_str::<AccessRecord>(l).ok())
    {
        match rec.split.as_str() {
            ACCESS_SPLIT_TRAINING => {
                training.insert(rec.id);
            }
            ACCESS_SPLIT_MEMORY => {
                memory.insert(rec.id);
            }
            _ => {}
        }
    }
    (training, memory)
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use crate::rsi_corpus::{
        CorpusFixture, FixtureClass, FixtureSplit, RsiCorpus, RSI_CORPUS_SCHEMA,
    };
    use sha2::{Digest, Sha256};

    fn hash(s: &str) -> String {
        susi_paths::hex::encode(Sha256::digest(s.as_bytes()))
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

    #[test]
    fn detect_in_corpus_rejects_tampered_corpus_and_contamination_report_serde_roundtrips() {
        let mut corpus = RsiCorpus {
            schema_version: RSI_CORPUS_SCHEMA.to_string(),
            revision: "rev-2".to_string(),
            fixtures: vec![CorpusFixture {
                id: "hold-1".to_string(),
                class: FixtureClass::Coding,
                input: "tampered body".to_string(),
                input_hash: hash("original body"),
                split: FixtureSplit::HeldOut,
                seed: 1,
                evaluator_expected: None,
            }],
            promotion_spec: None,
        };
        assert!(detect_in_corpus(&corpus, &BTreeSet::new(), &BTreeSet::new()).is_err());

        corpus.fixtures[0].input_hash = hash("tampered body");
        let clean = detect_in_corpus(&corpus, &BTreeSet::new(), &BTreeSet::new()).unwrap();
        assert!(!clean.contaminated);
        let json = serde_json::to_string(&clean).unwrap();
        let back: ContaminationReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, clean);
    }
}
