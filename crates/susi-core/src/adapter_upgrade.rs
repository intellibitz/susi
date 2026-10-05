//! Sandbox and roll back adapter upgrades (VC-201-089).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterRevision {
    pub id: String,
    pub version: u32,
}

/// Named contract-check outcomes a real sandbox run produced — not a bare
/// boolean the caller asserts. An empty set is not evidence of anything
/// passing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractResults {
    pub checks: Vec<(String, bool)>,
}

impl ContractResults {
    #[must_use]
    pub fn all_pass(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|(_, ok)| *ok)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchVerdict {
    Switched,
    /// Nothing was staged — distinct from a contract or version failure,
    /// so an operator can tell "no candidate" from "checks failed".
    NoCandidateStaged,
    RejectedContractFailure,
    /// The staged revision's version was not newer than the active one.
    RejectedDowngrade,
}

#[derive(Debug, Default)]
pub struct AdapterUpgrade {
    active: Option<AdapterRevision>,
    staged: Option<AdapterRevision>,
    /// Session id → owning adapter revision id.
    sessions: BTreeMap<String, String>,
}

impl AdapterUpgrade {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// First-boot only: install a revision as active with nothing already
    /// running. Refuses to replace an already-installed active revision —
    /// every later update must go through `stage` + `switch_if_contracts_pass`,
    /// so a revision can never become live by bypassing the sandbox.
    pub fn install_active(&mut self, rev: AdapterRevision) -> Result<(), String> {
        if let Some(active) = &self.active {
            return Err(format!(
                "an active revision ({}) is already installed; stage {} and switch through contracts instead",
                active.id, rev.id
            ));
        }
        self.active = Some(rev);
        Ok(())
    }

    /// Install candidate beside the active revision.
    pub fn stage(&mut self, rev: AdapterRevision) {
        self.staged = Some(rev);
    }

    #[must_use]
    pub fn active(&self) -> Option<&AdapterRevision> {
        self.active.as_ref()
    }

    #[must_use]
    pub fn staged(&self) -> Option<&AdapterRevision> {
        self.staged.as_ref()
    }

    pub fn bind_session(&mut self, session_id: &str) {
        if let Some(a) = &self.active {
            self.sessions.insert(session_id.to_string(), a.id.clone());
        }
    }

    #[must_use]
    pub fn session_owner(&self, session_id: &str) -> Option<&str> {
        self.sessions.get(session_id).map(String::as_str)
    }

    /// Switch only after real contract checks pass and the staged
    /// revision is actually newer than the active one; otherwise preserve
    /// active and in-flight session ownership. "Nothing staged" is its
    /// own verdict, distinct from a contract or version failure.
    pub fn switch_if_contracts_pass(&mut self, contracts: &ContractResults) -> SwitchVerdict {
        let Some(staged) = self.staged.take() else {
            return SwitchVerdict::NoCandidateStaged;
        };
        if !contracts.all_pass() {
            return SwitchVerdict::RejectedContractFailure;
        }
        if let Some(active) = &self.active {
            if staged.version <= active.version {
                return SwitchVerdict::RejectedDowngrade;
            }
        }
        self.active = Some(staged);
        SwitchVerdict::Switched
    }
}
