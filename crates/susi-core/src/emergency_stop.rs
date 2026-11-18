//! Operator emergency stop across the fleet (VC-201-080).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

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
    #[serde(default)]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningTask {
    pub id: String,
    pub cancellable: bool,
    pub external: bool,
    #[serde(default = "default_task_scope")]
    pub scope: String,
}

fn default_task_scope() -> String {
    "fleet".to_string()
}

impl RunningTask {
    pub fn new(
        id: impl Into<String>,
        cancellable: bool,
        external: bool,
        scope: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            cancellable,
            external,
            scope: scope.into(),
        }
    }
}

pub fn scope_matches(stop_scope: &str, task_scope: &str) -> bool {
    stop_scope == "fleet"
        || stop_scope == "*"
        || stop_scope == "global"
        || stop_scope.eq_ignore_ascii_case(task_scope)
        || task_scope.starts_with(&format!("{stop_scope}/"))
        || task_scope.starts_with(&format!("{stop_scope}::"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmergencyStopActive {
    pub scope: String,
    pub reason: String,
}

impl std::fmt::Display for EmergencyStopActive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "emergency stop active for scope '{}': {}",
            self.scope, self.reason
        )
    }
}

impl std::error::Error for EmergencyStopActive {}

#[derive(Debug, Default)]
pub struct EmergencyStopBus {
    pub status: StopStatus,
    /// peer_id -> reachable
    peers: BTreeMap<String, bool>,
    running: BTreeMap<String, RunningTask>,
}

impl EmergencyStopBus {
    pub fn global() -> &'static std::sync::RwLock<EmergencyStopBus> {
        static INSTANCE: OnceLock<std::sync::RwLock<EmergencyStopBus>> = OnceLock::new();
        INSTANCE.get_or_init(|| std::sync::RwLock::new(EmergencyStopBus::default()))
    }

    pub fn is_globally_stopped(scope: &str) -> bool {
        Self::global()
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .rejects_new_work_for_scope(scope)
    }

    pub fn register_peer(&mut self, id: &str, reachable: bool) {
        self.peers.insert(id.to_string(), reachable);
    }

    pub fn is_task_running(&self, id: &str) -> bool {
        self.running.contains_key(id)
    }

    /// Register a task at admission. If a matching emergency stop is active,
    /// rejects the task at admission and immediately marks it cancelled / unrecalled.
    pub fn register_task(&mut self, task: RunningTask) -> Result<(), EmergencyStopActive> {
        if let Some(stop) = &self.status.stop {
            if stop.active && scope_matches(&stop.scope, &task.scope) {
                if task.cancellable {
                    self.status.cancelled_tasks.insert(task.id);
                } else if task.external {
                    self.status.unrecalled_external.insert(task.id);
                }
                return Err(EmergencyStopActive {
                    scope: stop.scope.clone(),
                    reason: stop.reason.clone(),
                });
            }
        }
        self.running.insert(task.id.clone(), task);
        Ok(())
    }

    /// Persist and apply a scoped stop: reject new work, cancel eligible tasks matching scope.
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
            if scope_matches(scope, &t.scope) {
                if t.cancellable {
                    self.status.cancelled_tasks.insert(t.id.clone());
                } else if t.external {
                    self.status.unrecalled_external.insert(t.id.clone());
                }
            }
        }
    }

    #[must_use]
    pub fn rejects_new_work(&self) -> bool {
        self.status.stop.as_ref().is_some_and(|s| s.active)
    }

    #[must_use]
    pub fn rejects_new_work_for_scope(&self, scope: &str) -> bool {
        self.status
            .stop
            .as_ref()
            .is_some_and(|s| s.active && scope_matches(&s.scope, scope))
    }

    pub fn compute_signature(status: &StopStatus) -> String {
        use sha2::{Digest, Sha256};
        let payload = format!(
            "stop:{:?}|cancelled:{:?}|peers:{:?}|unrecalled:{:?}",
            status.stop,
            status.cancelled_tasks,
            status.unreachable_peers,
            status.unrecalled_external
        );
        let key = b"susi-operator-emergency-stop-v1-key";
        let mut hasher = Sha256::new();
        hasher.update(key);
        hasher.update(payload.as_bytes());
        susi_paths::hex::encode(hasher.finalize())
    }

    /// Survive restart by loading persisted, tamper-evident signed JSON.
    pub fn persist_to(&self, path: &Path) -> Result<(), String> {
        let mut to_persist = self.status.clone();
        to_persist.signature = Some(Self::compute_signature(&to_persist));
        let json = serde_json::to_string_pretty(&to_persist).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, json).map_err(|e| e.to_string())
    }

    pub fn restore_from(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let status: StopStatus = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let expected = Self::compute_signature(&status);
        match &status.signature {
            Some(sig) if sig == &expected => Ok(Self {
                status,
                peers: BTreeMap::new(),
                running: BTreeMap::new(),
            }),
            Some(_) => {
                Err("tampered emergency stop file: signature verification failed".to_string())
            }
            None => Err("unsigned emergency stop file: signature missing".to_string()),
        }
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn scope_matches_handles_wildcards_exact_and_prefixed_scopes() {
        assert!(scope_matches("fleet", "workspace/alpha"));
        assert!(scope_matches("*", "workspace/alpha"));
        assert!(scope_matches("global", "workspace/alpha"));
        assert!(scope_matches("Workspace", "workspace"));
        assert!(scope_matches("team", "team/sub"));
        assert!(scope_matches("team", "team::sub"));
        assert!(!scope_matches("team", "team_other"));
    }

    #[test]
    fn register_task_admits_when_clear_and_rejects_when_matching_stop_active() {
        let mut bus = EmergencyStopBus::default();
        bus.register_task(RunningTask::new("t1", true, false, "team/a"))
            .unwrap();
        assert!(bus.is_task_running("t1"));

        bus.apply_stop("team", "operator halt");
        let err = bus
            .register_task(RunningTask::new("t2", false, true, "team/b"))
            .unwrap_err();
        assert_eq!(err.scope, "team");
        assert!(err.to_string().contains("operator halt"));
        assert!(bus.status.unrecalled_external.contains("t2"));
    }

    #[test]
    fn apply_stop_cancels_matching_tasks_and_records_unreachable_peers() {
        let mut bus = EmergencyStopBus::default();
        bus.register_peer("peer-ok", true);
        bus.register_peer("peer-down", false);
        bus.register_task(RunningTask::new("cancel-me", true, false, "prod/api"))
            .unwrap();
        bus.register_task(RunningTask::new("ext-me", false, true, "prod/ext"))
            .unwrap();
        bus.register_task(RunningTask::new("keep-me", true, false, "staging/api"))
            .unwrap();

        bus.apply_stop("prod", "incident");
        assert!(bus.rejects_new_work());
        assert!(bus.rejects_new_work_for_scope("prod/api"));
        assert!(!bus.rejects_new_work_for_scope("staging/api"));
        assert!(bus.status.cancelled_tasks.contains("cancel-me"));
        assert!(!bus.status.cancelled_tasks.contains("keep-me"));
        assert!(bus.status.unrecalled_external.contains("ext-me"));
        assert!(bus.status.unreachable_peers.contains("peer-down"));
        assert!(!bus.status.unreachable_peers.contains("peer-ok"));
    }

    #[test]
    fn persist_and_restore_roundtrips_signed_status_and_detects_tampering() {
        let dir = std::env::temp_dir().join(format!("susi-estop-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("estop.json");

        let mut bus = EmergencyStopBus::default();
        bus.apply_stop("fleet", "maintenance");
        bus.persist_to(&path).unwrap();

        let restored = EmergencyStopBus::restore_from(&path).unwrap();
        assert_eq!(restored.status.stop, bus.status.stop);

        let raw = std::fs::read_to_string(&path).unwrap();
        let tampered = raw.replace("maintenance", "forged");
        std::fs::write(&path, tampered).unwrap();
        assert!(EmergencyStopBus::restore_from(&path).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn restore_from_rejects_unsigned_file_and_cancellable_admission_records_cancelled() {
        let dir = std::env::temp_dir().join(format!("susi-estop-unsigned-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("unsigned.json");

        let unsigned = StopStatus::default();
        std::fs::write(&path, serde_json::to_string(&unsigned).unwrap()).unwrap();
        let err = EmergencyStopBus::restore_from(&path).unwrap_err();
        assert!(err.contains("unsigned emergency stop file"), "{err}");

        let mut bus = EmergencyStopBus::default();
        bus.apply_stop("fleet", "drill");
        assert!(bus
            .register_task(RunningTask::new("c1", true, false, "fleet/worker"))
            .is_err());
        assert!(bus.status.cancelled_tasks.contains("c1"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
