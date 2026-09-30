//! Fence coordinator failover during live missions (VC-201-035).
//!
//! Task leases and authoritative mission writes bind to accepted leadership
//! epochs; stale-coordinator writes after partition are rejected.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadershipEpoch {
    pub epoch: u64,
    pub coordinator: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionWrite {
    pub mission_id: String,
    pub epoch: u64,
    pub coordinator: String,
    pub payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteVerdict {
    Accepted,
    StaleEpoch,
    WrongCoordinator,
}

#[derive(Debug, Default)]
pub struct FencedMissionStore {
    epoch: Option<LeadershipEpoch>,
    state: BTreeMap<String, String>,
}

impl FencedMissionStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept a new leadership epoch (failover). Must be strictly greater.
    pub fn accept_epoch(&mut self, epoch: LeadershipEpoch) -> bool {
        if let Some(cur) = &self.epoch {
            if epoch.epoch <= cur.epoch {
                return false;
            }
        }
        self.epoch = Some(epoch);
        true
    }

    #[must_use]
    pub fn current_epoch(&self) -> Option<&LeadershipEpoch> {
        self.epoch.as_ref()
    }

    #[must_use]
    pub fn mission(&self, id: &str) -> Option<&String> {
        self.state.get(id)
    }

    pub fn write(&mut self, w: MissionWrite) -> WriteVerdict {
        let Some(cur) = &self.epoch else {
            return WriteVerdict::StaleEpoch;
        };
        if w.epoch < cur.epoch {
            return WriteVerdict::StaleEpoch;
        }
        if w.epoch > cur.epoch {
            return WriteVerdict::StaleEpoch;
        }
        if w.coordinator != cur.coordinator {
            return WriteVerdict::WrongCoordinator;
        }
        self.state.insert(w.mission_id, w.payload);
        WriteVerdict::Accepted
    }
}
