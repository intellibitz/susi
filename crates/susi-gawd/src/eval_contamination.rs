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
