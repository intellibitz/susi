//! Operator emergency stop across the fleet (VC-201-080).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmergencyStop {
    pub scope: String,
    pub reason: String,
    pub active: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopStatus {
    pub stop: Option<EmergencyStop>,
    pub cancelled_tasks: BTreeSet<String>,
    pub unreachable_peers: BTreeSet<String>,
    pub unrecalled_external: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub struct EmergencyStopBus {
    pub status: StopStatus,
    /// peer_id -> reachable
    peers: BTreeMap<String, bool>,
    running: BTreeMap<String, RunningTask>,
}

#[derive(Debug, Clone)]
pub struct RunningTask {
    pub id: String,
    pub cancellable: bool,
    pub external: bool,
}

impl EmergencyStopBus {
    pub fn register_peer(&mut self, id: &str, reachable: bool) {
        self.peers.insert(id.to_string(), reachable);
    }

    pub fn register_task(&mut self, task: RunningTask) {
        self.running.insert(task.id.clone(), task);
    }

    /// Persist and apply a scoped stop: reject new work, cancel eligible tasks.
    pub fn apply_stop(&mut self, scope: &str, reason: &str) {
        self.status.stop = Some(EmergencyStop {
            scope: scope.to_string(),
            reason: reason.to_string(),
            active: true,
        });
        for (pid, ok) in &self.peers {
            if !*ok {
                self.status.unreachable_peers.insert(pid.clone());
            }
        }
        for t in self.running.values() {
            if t.cancellable {
                self.status.cancelled_tasks.insert(t.id.clone());
            } else if t.external {
                self.status.unrecalled_external.insert(t.id.clone());
            }
        }
    }

    #[must_use]
    pub fn rejects_new_work(&self) -> bool {
        self.status.stop.as_ref().is_some_and(|s| s.active)
    }

    /// Survive restart by loading persisted JSON.
    pub fn persist_to(&self, path: &Path) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&self.status).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn restore_from(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let status: StopStatus = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        Ok(Self {
            status,
            peers: BTreeMap::new(),
            running: BTreeMap::new(),
        })
    }
}
