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

    /// Repeated proposals must address prior rejection or declare new evidence.
    #[must_use]
    pub fn may_resubmit(
        &self,
        proposal_id: &str,
        addresses_prior: bool,
        new_evidence: bool,
    ) -> bool {
        match self.by_proposal.get(proposal_id) {
            None => true,
            Some(_) => addresses_prior || new_evidence,
        }
    }
}
