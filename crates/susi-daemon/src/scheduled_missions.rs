//! Cron-style recurring missions persisted in the config dir, with run
//! history and failure backoff.
//!
//! A [`Schedule`] is a fixed interval in seconds (the cron-lite vocabulary
//! the daemon supports); a [`ScheduledMission`] binds one to a prompt and
//! records every run. Consecutive failures push the next run out
//! exponentially (base × 2^failures, capped) so a broken mission stops
//! hammering providers without being deleted.

use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_HISTORY: usize = 100;
const MAX_BACKOFF_SECS: u64 = 24 * 3600;

/// How often a mission recurs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// Minimum seconds between runs.
    pub every_secs: u64,
}

impl Schedule {
    #[must_use]
    pub fn hourly() -> Self {
        Self { every_secs: 3600 }
    }
    #[must_use]
    pub fn daily() -> Self {
        Self { every_secs: 86_400 }
    }
    #[must_use]
    pub fn weekly() -> Self {
        Self {
            every_secs: 604_800,
        }
    }
    #[must_use]
    pub fn every(every_secs: u64) -> Self {
        Self { every_secs }
    }
}

/// One executed (or attempted) run — the evidence trail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    pub started_unix: u64,
    pub finished_unix: u64,
    pub success: bool,
    /// Short receipt/summary of what happened.
    #[serde(default)]
    pub summary: String,
}

/// A persisted recurring mission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledMission {
    pub id: String,
    pub prompt: String,
    pub schedule: Schedule,
    /// When it last ran (start time); `None` = never.
    #[serde(default)]
    pub last_run_unix: Option<u64>,
    /// Consecutive failures — drives backoff.
    #[serde(default)]
    pub consecutive_failures: u32,
    /// Operator or backoff disable.
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub history: Vec<RunRecord>,
}

impl ScheduledMission {
    #[must_use]
    pub fn new(id: &str, prompt: &str, schedule: Schedule) -> Self {
        Self {
            id: id.to_string(),
            prompt: prompt.to_string(),
            schedule,
            last_run_unix: None,
            consecutive_failures: 0,
            paused: false,
            history: Vec::new(),
        }
    }

    /// Seconds of failure backoff in effect right now (0 when healthy).
    #[must_use]
    pub fn backoff_secs(&self) -> u64 {
        if self.consecutive_failures == 0 {
            return 0;
        }
        self.schedule
            .every_secs
            .saturating_mul(1u64 << self.consecutive_failures.min(6))
            .min(MAX_BACKOFF_SECS)
    }

    /// Unix time the mission becomes due; a never-run, unpaused mission is
    /// due immediately.
    #[must_use]
    pub fn due_at(&self) -> u64 {
        match self.last_run_unix {
            None => 0,
            Some(base) => base + self.schedule.every_secs + self.backoff_secs(),
        }
    }

    #[must_use]
    pub fn is_due(&self, now_unix: u64) -> bool {
        !self.paused && now_unix >= self.due_at()
    }

    /// Record a finished run: appends to bounded history, updates failure
    /// streak/backoff.
    pub fn record_run(&mut self, record: RunRecord) {
        self.last_run_unix = Some(record.started_unix);
        self.consecutive_failures = if record.success {
            0
        } else {
            self.consecutive_failures.saturating_add(1)
        };
        self.history.push(record);
        if self.history.len() > MAX_HISTORY {
            self.history.drain(..self.history.len() - MAX_HISTORY);
        }
    }
}

/// The on-disk set of scheduled missions.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ScheduleStore {
    #[serde(default)]
    pub missions: Vec<ScheduledMission>,
}

impl ScheduleStore {
    /// Path inside a config dir.
    #[must_use]
    pub fn path_in(config_dir: &Path) -> PathBuf {
        config_dir.join("scheduled-missions.json")
    }

    /// Load; a missing file yields an empty store.
    ///
    /// # Errors
    /// [`EaiError::io`] on unreadable or corrupt files.
    pub fn load(path: &Path) -> EaiResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| EaiError::io(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(EaiError::io(e.to_string())),
        }
    }

    /// Persist (creates parent dirs).
    ///
    /// # Errors
    /// [`EaiError::io`] on write/encode failures.
    pub fn save(&self, path: &Path) -> EaiResult<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).map_err(|e| EaiError::io(e.to_string()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| EaiError::io(e.to_string()))
    }

    /// Add or replace a mission by id.
    pub fn upsert(&mut self, m: ScheduledMission) {
        if let Some(slot) = self.missions.iter_mut().find(|x| x.id == m.id) {
            *slot = m;
        } else {
            self.missions.push(m);
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.missions.len();
        self.missions.retain(|m| m.id != id);
        self.missions.len() != before
    }

    /// Missions due at `now_unix`, ordered by due time (earliest first).
    #[must_use]
    pub fn due(&self, now_unix: u64) -> Vec<&ScheduledMission> {
        let mut v: Vec<&ScheduledMission> = self
            .missions
            .iter()
            .filter(|m| m.is_due(now_unix))
            .collect();
        v.sort_by_key(|m| m.due_at());
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(at: u64, ok: bool) -> RunRecord {
        RunRecord {
            started_unix: at,
            finished_unix: at + 10,
            success: ok,
            summary: String::new(),
        }
    }

    #[test]
    fn scheduled_missions_new_mission_due_immediately() {
        let m = ScheduledMission::new("m1", "scan", Schedule::hourly());
        assert!(m.is_due(0));
        assert!(m.is_due(u64::MAX / 4));
    }

    #[test]
    fn scheduled_missions_due_after_interval() {
        let mut m = ScheduledMission::new("m1", "scan", Schedule::every(100));
        m.record_run(run(1000, true));
        assert!(!m.is_due(1050));
        assert!(m.is_due(1100));
    }

    #[test]
    fn scheduled_missions_failure_backoff_grows() {
        let mut m = ScheduledMission::new("m1", "scan", Schedule::every(60));
        m.record_run(run(0, false));
        assert_eq!(m.consecutive_failures, 1);
        assert_eq!(m.backoff_secs(), 120); // 60 * 2^1
        assert!(m.is_due(180)); // last_run 0 + interval 60 + backoff 120
        m.record_run(run(180, false));
        assert_eq!(m.backoff_secs(), 240); // 60 * 2^2
        // success resets
        m.record_run(run(1000, true));
        assert_eq!(m.backoff_secs(), 0);
        assert_eq!(m.consecutive_failures, 0);
    }

    #[test]
    fn scheduled_missions_backoff_capped() {
        let mut m = ScheduledMission::new("m1", "s", Schedule::every(3600));
        m.consecutive_failures = 20;
        assert_eq!(m.backoff_secs(), MAX_BACKOFF_SECS);
    }

    #[test]
    fn scheduled_missions_paused_never_due() {
        let mut m = ScheduledMission::new("m1", "s", Schedule::every(1));
        m.paused = true;
        assert!(!m.is_due(u64::MAX / 4));
    }

    #[test]
    fn scheduled_missions_history_bounded() {
        let mut m = ScheduledMission::new("m1", "s", Schedule::every(1));
        for i in 0..150u64 {
            m.record_run(run(i, true));
        }
        assert_eq!(m.history.len(), MAX_HISTORY);
    }

    #[test]
    fn scheduled_missions_store_roundtrip_and_due_order() {
        let dir = std::env::temp_dir().join(format!("sched-{}", std::process::id()));
        let path = ScheduleStore::path_in(&dir);
        let mut s = ScheduleStore::default();
        let mut a = ScheduledMission::new("a", "p", Schedule::every(100));
        a.record_run(run(500, true)); // due at 600
        let b = ScheduledMission::new("b", "p", Schedule::hourly()); // due now
        s.upsert(a);
        s.upsert(b);
        s.save(&path).unwrap();

        let loaded = ScheduleStore::load(&path).unwrap();
        assert_eq!(loaded.missions.len(), 2);
        let due = loaded.due(650);
        assert_eq!(due.len(), 2);
        assert_eq!(due[0].id, "b"); // due_at 0 < 600
        assert!(loaded.due(550).iter().all(|m| m.id == "b"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn scheduled_missions_remove_and_missing_file() {
        let mut s = ScheduleStore::default();
        s.upsert(ScheduledMission::new("x", "p", Schedule::daily()));
        assert!(s.remove("x"));
        assert!(!s.remove("x"));
        let path = std::env::temp_dir().join(format!("sched-none-{}", std::process::id()));
        assert!(ScheduleStore::load(&path).unwrap().missions.is_empty());
    }
}
