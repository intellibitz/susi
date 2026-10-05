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
