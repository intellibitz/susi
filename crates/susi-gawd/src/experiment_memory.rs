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

const MEMORY_LOCK: &str = "experiment_memory";

fn memory_path(workspace: &std::path::Path) -> std::path::PathBuf {
    workspace.join(".susi").join("experiment_memory.json")
}

impl ExperimentMemory {
    /// The workspace's durable experiment memory at
    /// `workspace/.susi/experiment_memory.json`. A missing or unparseable
    /// file is an empty memory — never a panic.
    #[must_use]
    pub fn load(workspace: &std::path::Path) -> Self {
        std::fs::read(memory_path(workspace))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write_unlocked(&self, workspace: &std::path::Path) -> crate::susi_error::EaiResult<()> {
        use crate::susi_error::EaiError;
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| {
            EaiError::internal(format!("experiment memory serialization failed: {e}"))
        })?;
        crate::susi_config::atomic_replace_file(&memory_path(workspace), &bytes)
            .map_err(|e| EaiError::filesystem(format!("experiment memory write failed: {e}")))
    }

    fn lock(
        workspace: &std::path::Path,
    ) -> crate::susi_error::EaiResult<crate::susi_config::file_lock::FileLock> {
        let susi_dir = workspace.join(".susi");
        crate::susi_config::file_lock::FileLock::acquire(&susi_dir, MEMORY_LOCK).ok_or_else(|| {
            crate::susi_error::EaiError::filesystem(
                "experiment memory lock acquisition failed".to_string(),
            )
        })
    }

    /// Persist this memory atomically under the `experiment_memory` lock.
    pub fn save(&self, workspace: &std::path::Path) -> crate::susi_error::EaiResult<()> {
        let _lock = Self::lock(workspace)?;
        self.write_unlocked(workspace)
    }

    /// Load, record `entry` and save under one lock hold, so two
    /// concurrent rejections cannot lose each other's entry the way a
    /// separate load → record → save would.
    pub fn record_in_workspace(
        workspace: &std::path::Path,
        entry: ExperimentMemoryEntry,
    ) -> crate::susi_error::EaiResult<()> {
        let _lock = Self::lock(workspace)?;
        let mut memory = Self::load(workspace);
        memory.record(entry);
        memory.write_unlocked(workspace)
    }

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

/// [`ExperimentMemory::may_resubmit`] against the workspace's durable
/// memory — the check a proposal validator runs when it only has a
/// workspace, not a loaded memory.
#[must_use]
pub fn check_resubmission_in_workspace(
    workspace: &std::path::Path,
    candidate: &ExperimentMemoryEntry,
    addressed: &[String],
    fresh_receipts: &[String],
) -> bool {
    ExperimentMemory::load(workspace).may_resubmit(candidate, addressed, fresh_receipts)
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

    #[test]
    fn experiment_memory_and_entry_serde_json_roundtrip() {
        let mut mem = ExperimentMemory::default();
        mem.record(ExperimentMemoryEntry {
            proposal_id: "prop-serde".to_string(),
            cause: "timeout".to_string(),
            counterexample: Some("case-9".to_string()),
            lineage: vec!["p-0".to_string()],
        });
        let encoded = serde_json::to_string(&mem).expect("serialize ExperimentMemory");
        let decoded: ExperimentMemory =
            serde_json::from_str(&encoded).expect("deserialize ExperimentMemory");
        assert_eq!(decoded, mem);
    }
}
