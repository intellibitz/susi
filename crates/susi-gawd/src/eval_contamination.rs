//! Evaluation contamination detection (VC-201-009).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContaminationReport {
    pub contaminated: bool,
    pub reason: Option<String>,
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
