//! Learn from failed and rejected experiments (VC-201-019).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentMemoryEntry {
    pub proposal_id: String,
    pub cause: String,
    pub counterexample: Option<String>,
    pub lineage: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExperimentMemory {
    pub by_proposal: BTreeMap<String, ExperimentMemoryEntry>,
}

impl ExperimentMemory {
    pub fn record(&mut self, entry: ExperimentMemoryEntry) {
        self.by_proposal.insert(entry.proposal_id.clone(), entry);
    }

    /// Find a prior entry for this proposal: by id first, then by
    /// identical (cause, counterexample) content under a different id —
    /// a resubmission does not get a clean slate just by renaming itself.
    fn prior(
        &self,
        proposal_id: &str,
        cause: &str,
        counterexample: Option<&str>,
    ) -> Option<&ExperimentMemoryEntry> {
        self.by_proposal.get(proposal_id).or_else(|| {
            self.by_proposal
                .values()
                .find(|e| e.cause == cause && e.counterexample.as_deref() == counterexample)
        })
    }

    /// A proposal repeating prior content — by id or by matching
    /// (cause, counterexample) under a new id — must either reference
    /// the prior entry it is repeating in `addressed`, or cite at least
    /// one non-blank fresh receipt; a caller-asserted boolean is no
    /// longer sufficient, since that accepted the identical proposal by
    /// construction.
    #[must_use]
    pub fn may_resubmit(
        &self,
        candidate: &ExperimentMemoryEntry,
        addressed: &[String],
        fresh_receipts: &[String],
    ) -> bool {
        let Some(prior) = self.prior(
            &candidate.proposal_id,
            &candidate.cause,
            candidate.counterexample.as_deref(),
        ) else {
            return true;
        };
        let addresses_prior = addressed.iter().any(|a| a == &prior.proposal_id);
        let has_fresh_evidence = fresh_receipts.iter().any(|r| !r.trim().is_empty());
        addresses_prior || has_fresh_evidence
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn novel_proposal_may_resubmit_without_addressed_ids_or_fresh_receipts() {
        let mem = ExperimentMemory::default();
        let candidate = ExperimentMemoryEntry {
            proposal_id: "prop-1".to_string(),
            cause: "latency regression".to_string(),
            counterexample: Some("case-a".to_string()),
            lineage: Vec::new(),
        };
        assert!(mem.may_resubmit(&candidate, &[], &[]));
    }

    #[test]
    fn repeat_proposal_by_id_requires_prior_id_in_addressed_or_non_blank_receipt() {
        let mut mem = ExperimentMemory::default();
        let entry = ExperimentMemoryEntry {
            proposal_id: "prop-1".to_string(),
            cause: "latency regression".to_string(),
            counterexample: Some("case-a".to_string()),
            lineage: vec!["root".to_string()],
        };
        mem.record(entry.clone());

        assert!(!mem.may_resubmit(&entry, &[], &[]));
        assert!(mem.may_resubmit(&entry, &["prop-1".to_string()], &[]));
        assert!(mem.may_resubmit(&entry, &[], &["receipt-123".to_string()]));
    }

    #[test]
    fn renamed_proposal_with_identical_cause_and_counterexample_is_detected_as_prior() {
        let mut mem = ExperimentMemory::default();
        mem.record(ExperimentMemoryEntry {
            proposal_id: "prop-original".to_string(),
            cause: "oom under burst".to_string(),
            counterexample: Some("trace-7".to_string()),
            lineage: Vec::new(),
        });

        let renamed = ExperimentMemoryEntry {
            proposal_id: "prop-renamed".to_string(),
            cause: "oom under burst".to_string(),
            counterexample: Some("trace-7".to_string()),
            lineage: Vec::new(),
        };
        assert!(!mem.may_resubmit(&renamed, &["prop-renamed".to_string()], &[]));
        assert!(mem.may_resubmit(&renamed, &["prop-original".to_string()], &[]));
    }

    #[test]
    fn blank_fresh_receipts_do_not_satisfy_resubmission_gate() {
        let mut mem = ExperimentMemory::default();
        let entry = ExperimentMemoryEntry {
            proposal_id: "prop-2".to_string(),
            cause: "deadlock".to_string(),
            counterexample: None,
            lineage: Vec::new(),
        };
        mem.record(entry.clone());
        assert!(!mem.may_resubmit(&entry, &[], &[String::new(), "   \t\n ".to_string()]));
    }
}
