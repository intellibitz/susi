//! Sandbox and roll back adapter upgrades (VC-201-089).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterRevision {
    pub id: String,
    pub version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchVerdict {
    Switched,
    PreservedActive,
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

    pub fn install_active(&mut self, rev: AdapterRevision) {
        self.active = Some(rev);
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

    /// Switch only after contract checks succeed; otherwise preserve active
    /// and in-flight session ownership.
    pub fn switch_if_contracts_pass(&mut self, contracts_ok: bool) -> SwitchVerdict {
        if !contracts_ok {
            self.staged = None;
            return SwitchVerdict::PreservedActive;
        }
        if let Some(staged) = self.staged.take() {
            self.active = Some(staged);
            SwitchVerdict::Switched
        } else {
            SwitchVerdict::PreservedActive
        }
    }
}
