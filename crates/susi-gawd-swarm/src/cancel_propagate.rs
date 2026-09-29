//! Propagate cancellation and remaining deadlines through the swarm (VC-201-026).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    Local,
    ExternalAgent,
    PeerDispatch,
    ModelCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelToken {
    pub root_id: String,
    pub deadline_unix: u64,
    pub cancelled: bool,
}

impl CancelToken {
    #[must_use]
    pub fn fresh(root_id: &str, deadline_unix: u64) -> Self {
        Self {
            root_id: root_id.to_string(),
            deadline_unix,
            cancelled: false,
        }
    }

    #[must_use]
    pub fn remaining_secs(&self, now_unix: u64) -> Option<u64> {
        if self.cancelled || now_unix >= self.deadline_unix {
            None
        } else {
            Some(self.deadline_unix - now_unix)
        }
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Descendant {
    pub id: String,
    pub kind: WorkerKind,
    pub cancellable: bool,
}

#[derive(Debug, Default)]
pub struct CancelBus {
    pub token: Option<CancelToken>,
    pub terminated: BTreeSet<String>,
    pub irreversible_running: BTreeSet<String>,
    pub descendants: BTreeMap<String, Descendant>,
}

impl CancelBus {
    pub fn register(&mut self, d: Descendant) {
        self.descendants.insert(d.id.clone(), d);
    }

    pub fn set_token(&mut self, token: CancelToken) {
        self.token = Some(token);
    }

    /// Propagate cancel: terminate cancellable descendants; report irreversible.
    pub fn propagate(&mut self) -> Vec<String> {
        let Some(tok) = self.token.as_mut() else {
            return Vec::new();
        };
        tok.cancel();
        let mut reports = Vec::new();
        for d in self.descendants.values() {
            if d.cancellable {
                self.terminated.insert(d.id.clone());
            } else {
                self.irreversible_running.insert(d.id.clone());
                reports.push(format!("irreversible remote job still running: {}", d.id));
            }
        }
        reports
    }

    #[must_use]
    pub fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}
