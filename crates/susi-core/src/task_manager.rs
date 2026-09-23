// Tracks running task status and idle-timeout watchdog behavior.
// Mandate 12: Hardware Authority - operations run as slow as legitimate hardware
//   work requires; no blind wall-clock cap on total task duration.
// Mandate 32: Zero-Client-Wait Guarantee - an operation with zero observed
//   progress for longer than its empirically-calibrated idle lease is
//   "unresponsive," not "slow," and is proactively terminated.
// Mandate 26: Glass Box Transparency & Omni-Trace Task Control.

use dashmap::DashMap;
use serde::{ser::SerializeStruct, Deserialize, Serialize, Serializer};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TaskStatus {
    Running = 0,
    Paused = 1,
    Completed = 2,
    Failed = 3,
    Stalled = 4,
    Killed = 5,
}

impl From<u8> for TaskStatus {
    fn from(v: u8) -> Self {
        match v {
            0 => TaskStatus::Running,
            1 => TaskStatus::Paused,
            2 => TaskStatus::Completed,
            3 => TaskStatus::Failed,
            4 => TaskStatus::Stalled,
            5 => TaskStatus::Killed,
            _ => TaskStatus::Failed,
        }
    }
}

impl Serialize for TaskStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(match self {
            TaskStatus::Running => "Running",
            TaskStatus::Paused => "Paused",
            TaskStatus::Completed => "Completed",
            TaskStatus::Failed => "Failed",
            TaskStatus::Stalled => "Stalled",
            TaskStatus::Killed => "Killed",
        })
    }
}

impl<'de> Deserialize<'de> for TaskStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "Running" => TaskStatus::Running,
            "Paused" => TaskStatus::Paused,
            "Completed" => TaskStatus::Completed,
            "Failed" => TaskStatus::Failed,
            "Stalled" => TaskStatus::Stalled,
            "Killed" => TaskStatus::Killed,
            _ => TaskStatus::Failed,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub task_id: String,
    pub name: String,
    pub intent: String,
    pub start_time_secs: u64,
    pub expected_idle_ms: u64,
    pub status: Arc<AtomicU8>,
    pub last_progress_secs: Arc<AtomicU64>,
    pub progress_count: Arc<AtomicU64>,
    pub result: Arc<parking_lot::RwLock<Option<String>>>,
}

impl Serialize for TaskRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("TaskRecord", 9)?;
        state.serialize_field("task_id", &self.task_id)?;
        state.serialize_field("name", &self.name)?;
        state.serialize_field("intent", &self.intent)?;
        state.serialize_field(
            "status",
            &TaskStatus::from(self.status.load(Ordering::Acquire)),
        )?;
        state.serialize_field("start_time_secs", &self.start_time_secs)?;
        state.serialize_field("expected_idle_ms", &self.expected_idle_ms)?;
        state.serialize_field(
            "last_progress_secs",
            &self.last_progress_secs.load(Ordering::Acquire),
        )?;
        state.serialize_field(
            "progress_count",
            &self.progress_count.load(Ordering::Acquire),
        )?;
        state.serialize_field("result", &*self.result.read())?;
        state.end()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntentTelemetryProfile {
    pub intent_category: String,
    pub sample_count: u64,
    pub avg_latency_ms: u64,
    pub p99_idle_interval_ms: u64,
}

/// Empirical Telemetry History Store ("SUSI Never Trusts Words")
pub struct TelemetryHistoryStore {
    profiles: DashMap<String, IntentTelemetryProfile>,
}

impl TelemetryHistoryStore {
    pub fn global() -> &'static Self {
        static STORE: OnceLock<TelemetryHistoryStore> = OnceLock::new();
        STORE.get_or_init(|| {
            let store = TelemetryHistoryStore {
                profiles: DashMap::new(),
            };
            store.load_history();
            store
        })
    }

    fn get_history_file() -> PathBuf {
        crate::susi_paths::SusiDirs::data_dir().join("telemetry_history.json")
    }

    fn load_history(&self) {
        let file = Self::get_history_file();
        if file.is_file() {
            if let Ok(content) = std::fs::read_to_string(&file) {
                if let Ok(map) = serde_json::from_str::<
                    std::collections::HashMap<String, IntentTelemetryProfile>,
                >(&content)
                {
                    for (k, v) in map {
                        self.profiles.insert(k, v);
                    }
                }
            }
        }
    }

    pub fn save_history(&self) {
        let file = Self::get_history_file();
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut map = std::collections::HashMap::new();
        for r in self.profiles.iter() {
            map.insert(r.key().clone(), r.value().clone());
        }
        if let Ok(json) = serde_json::to_string_pretty(&map) {
            let _ = std::fs::write(file, json);
        }
    }

    pub fn record_execution_telemetry(
        &self,
        category: &str,
        elapsed_ms: u64,
        max_observed_idle_ms: u64,
    ) {
        // The entry guard below holds a write lock on this key's DashMap
        // shard; save_history()'s `self.profiles.iter()` locks every shard
        // in turn, including this one. Since DashMap's shard locks aren't
        // reentrant, calling save_history() while still holding the guard
        // deadlocks the thread against itself - it was never hit before
        // because every existing caller of mark_completed ran under
        // cfg!(test), which short-circuits inference before ever reaching
        // it. The block scope drops the guard before save_history() runs.
        {
            let mut profile = self
                .profiles
                .entry(category.to_string())
                .or_insert_with(|| IntentTelemetryProfile {
                    intent_category: category.to_string(),
                    sample_count: 0,
                    avg_latency_ms: elapsed_ms,
                    p99_idle_interval_ms: max_observed_idle_ms.max(200),
                });

            profile.sample_count += 1;
            let count = profile.sample_count;
            profile.avg_latency_ms = ((profile.avg_latency_ms * (count - 1)) + elapsed_ms) / count;
            profile.p99_idle_interval_ms = profile
                .p99_idle_interval_ms
                .max(max_observed_idle_ms.max(200));
        }

        self.save_history();
    }

    /// Empirically-calibrated idle lease (Mandate 32: Zero-Client-Wait Guarantee).
    /// Hardware capability still governs how *slow* a legitimate operation may be
    /// (Mandate 12: Hardware Authority) — this only bounds how long an operation
    /// may go with zero observed progress, which is the "unresponsive" case the
    /// mandate targets, not a cap on total task duration.
    ///
    /// Until a category has at least 3 completed samples, no threshold is applied
    /// ("SUSI Never Trusts Words" — an unseen operation's real idle behavior is
    /// unknown, so guessing a limit for it risks killing legitimate first-run
    /// work). Once calibrated, the lease is 5x the worst observed idle gap for
    /// that category, floored by the operator-configured execution_lease_secs so
    /// a single unusually fast historical sample can't produce an unreasonably
    /// tight threshold.
    pub fn get_idle_threshold_ms(&self, category: &str) -> u64 {
        let Some(profile) = self.profiles.get(category) else {
            return u64::MAX;
        };
        if profile.sample_count < 3 {
            return u64::MAX;
        }
        let global_dir = crate::susi_paths::SusiDirs::config_dir();
        let cfg = crate::susi_config::SusiConfig::load(&global_dir).unwrap_or_default();
        let floor_ms = cfg.execution_lease_secs().saturating_mul(1000);
        profile.p99_idle_interval_ms.saturating_mul(5).max(floor_ms)
    }
}

pub struct TaskHandle {
    pub task_id: String,
    pub cancel_flag: Arc<AtomicBool>,
    pub pause_flag: Arc<AtomicBool>,
    pub last_progress_secs: Arc<AtomicU64>,
    pub progress_count: Arc<AtomicU64>,
    pub status: Arc<AtomicU8>,
    pub result: Arc<parking_lot::RwLock<Option<String>>>,
    pub name: String,
    start_time: Instant,
}

impl TaskHandle {
    pub fn report_progress(&self) {
        let _guard = self.result.write();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.last_progress_secs.store(now, Ordering::Release);
        self.progress_count.fetch_add(1, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Acquire)
    }

    pub fn check_pause(&self) {
        while self.pause_flag.load(Ordering::Acquire) && !self.is_cancelled() {
            // Callers can update the public flags without a thread handle to unpark.
            // Bound the wait so resume/cancellation is observed, and recheck after
            // spurious wakes instead of allowing a paused task to continue.
            std::thread::park_timeout(Duration::from_millis(100));
        }
    }

    pub fn mark_completed(&self, res_text: &str) {
        if !self.finish(TaskStatus::Completed, res_text) {
            return;
        }
        let elapsed = self.start_time.elapsed().as_millis().min(u64::MAX as u128) as u64;
        TelemetryHistoryStore::global().record_execution_telemetry(&self.name, elapsed, 100);
    }

    pub fn mark_failed(&self, err: &str) {
        self.finish(TaskStatus::Failed, err);
    }

    fn finish(&self, status: TaskStatus, text: &str) -> bool {
        let mut result = self.result.write();
        if !matches!(
            TaskStatus::from(self.status.load(Ordering::Acquire)),
            TaskStatus::Running | TaskStatus::Paused
        ) {
            return false;
        }
        *result = Some(text.to_owned());
        self.status.store(status as u8, Ordering::Release);
        true
    }
}

pub struct SwarmTaskManager {
    pub tasks: DashMap<String, TaskRecord>,
    pub cancel_map: DashMap<String, Arc<AtomicBool>>,
    pub pause_map: DashMap<String, Arc<AtomicBool>>,
}

impl SwarmTaskManager {
    pub fn global() -> &'static Self {
        static MANAGER: OnceLock<SwarmTaskManager> = OnceLock::new();
        MANAGER.get_or_init(|| {
            let manager = SwarmTaskManager {
                tasks: DashMap::new(),
                cancel_map: DashMap::new(),
                pause_map: DashMap::new(),
            };
            manager.start_watchdog();
            manager
        })
    }

    pub fn register_task(&self, name: &str, intent: &str) -> Arc<TaskHandle> {
        let task_id = format!(
            "task_{}_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            rand_id()
        );
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let idle_threshold = TelemetryHistoryStore::global().get_idle_threshold_ms(name);

        let status = Arc::new(AtomicU8::new(TaskStatus::Running as u8));
        let last_progress_secs = Arc::new(AtomicU64::new(now_secs));
        let progress_count = Arc::new(AtomicU64::new(0));
        let result = Arc::new(parking_lot::RwLock::new(None));
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let pause_flag = Arc::new(AtomicBool::new(false));

        let record = TaskRecord {
            task_id: task_id.clone(),
            name: name.to_string(),
            intent: intent.to_string(),
            status: Arc::clone(&status),
            start_time_secs: now_secs,
            expected_idle_ms: idle_threshold,
            last_progress_secs: Arc::clone(&last_progress_secs),
            progress_count: Arc::clone(&progress_count),
            result: Arc::clone(&result),
        };

        self.tasks.insert(task_id.clone(), record);
        self.cancel_map
            .insert(task_id.clone(), Arc::clone(&cancel_flag));
        self.pause_map
            .insert(task_id.clone(), Arc::clone(&pause_flag));

        Arc::new(TaskHandle {
            task_id,
            cancel_flag,
            pause_flag,
            last_progress_secs,
            progress_count,
            status,
            result,
            name: name.to_string(),
            start_time: Instant::now(),
        })
    }

    fn start_watchdog(&self) {
        if cfg!(test) {
            return;
        }
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                let mgr = SwarmTaskManager::global();
                let now_secs = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                // Zero-Lock Iteration Phase
                let mut expired: Vec<(String, u64, u64)> = Vec::new();
                for r in mgr.tasks.iter() {
                    let record = r.value();
                    if record.status.load(Ordering::Acquire) == TaskStatus::Running as u8
                        && record.expected_idle_ms != u64::MAX
                    {
                        let last_prog = record.last_progress_secs.load(Ordering::Acquire);
                        let idle_ms = now_secs.saturating_sub(last_prog).saturating_mul(1000);
                        if idle_ms > record.expected_idle_ms {
                            expired.push((r.key().clone(), idle_ms, record.expected_idle_ms));
                        }
                    }
                }

                // Mandate 32 (Zero-Client-Wait Guarantee): proactively terminate
                // operations that have produced zero observed progress for longer
                // than their empirically-calibrated lease.
                for (task_id, idle_ms, threshold_ms) in expired {
                    let Some(r) = mgr.tasks.get(&task_id) else {
                        continue;
                    };
                    let mut result = r.result.write();
                    if r.status.load(Ordering::Acquire) != TaskStatus::Running as u8
                        || now_secs
                            .saturating_sub(r.last_progress_secs.load(Ordering::Acquire))
                            .saturating_mul(1000)
                            <= r.expected_idle_ms
                    {
                        continue;
                    }
                    r.status.store(TaskStatus::Stalled as u8, Ordering::Release);
                    *result = Some(format!(
                            "[LEASE_EXPIRED] No progress for {}ms (calibrated threshold: {}ms) — proactively terminated.",
                            idle_ms, threshold_ms
                        ));
                    if let Some(c) = mgr.cancel_map.get(&task_id) {
                        c.store(true, Ordering::Release);
                    }
                    tracing::warn!(
                        target: "susi.task_manager",
                        task_id = %task_id,
                        idle_ms,
                        threshold_ms,
                        "LEASE_EXPIRED — cancelled by watchdog"
                    );
                }
            }
        });
    }

    pub fn list_tasks(&self) -> Vec<TaskRecord> {
        self.tasks.iter().map(|r| r.value().clone()).collect()
    }

    pub fn pause_task(&self, task_id: &str) -> bool {
        let Some(record) = self.tasks.get(task_id) else {
            return false;
        };
        let _guard = record.result.write();
        if record.status.load(Ordering::Acquire) != TaskStatus::Running as u8 {
            return false;
        }
        if let Some(flag) = self.pause_map.get(task_id) {
            flag.store(true, Ordering::Release);
        }
        record
            .status
            .store(TaskStatus::Paused as u8, Ordering::Release);
        true
    }

    pub fn resume_task(&self, task_id: &str) -> bool {
        let Some(record) = self.tasks.get(task_id) else {
            return false;
        };
        let _guard = record.result.write();
        if record.status.load(Ordering::Acquire) != TaskStatus::Paused as u8 {
            return false;
        }
        // Time explicitly spent paused must not consume the running idle lease.
        record.last_progress_secs.store(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            Ordering::Release,
        );
        if let Some(flag) = self.pause_map.get(task_id) {
            flag.store(false, Ordering::Release);
        }
        record
            .status
            .store(TaskStatus::Running as u8, Ordering::Release);
        true
    }

    pub fn kill_task(&self, task_id: &str) -> bool {
        let Some(record) = self.tasks.get(task_id) else {
            return false;
        };
        let mut result = record.result.write();
        if !matches!(
            TaskStatus::from(record.status.load(Ordering::Acquire)),
            TaskStatus::Running | TaskStatus::Paused
        ) {
            return false;
        }
        if let Some(flag) = self.cancel_map.get(task_id) {
            flag.store(true, Ordering::Release);
        }
        *result = Some("Killed by pulse".to_owned());
        record
            .status
            .store(TaskStatus::Killed as u8, Ordering::Release);
        true
    }
}

fn rand_id() -> u32 {
    (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
        % 100000) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_tasks_cannot_be_resumed_or_overwritten() {
        let manager = SwarmTaskManager {
            tasks: DashMap::new(),
            cancel_map: DashMap::new(),
            pause_map: DashMap::new(),
        };
        for status in [
            TaskStatus::Completed,
            TaskStatus::Failed,
            TaskStatus::Killed,
            TaskStatus::Stalled,
        ] {
            let handle = manager.register_task("lifecycle_test", "test");
            assert!(handle.finish(status, "original"));
            assert!(!manager.pause_task(&handle.task_id));
            assert!(!manager.resume_task(&handle.task_id));
            assert!(!manager.kill_task(&handle.task_id));
            assert!(!handle.finish(TaskStatus::Completed, "replacement"));
            assert_eq!(handle.status.load(Ordering::Acquire), status as u8);
            assert_eq!(handle.result.read().as_deref(), Some("original"));
        }
    }

    #[test]
    fn resume_refreshes_idle_lease_and_kill_preserves_cancellation() {
        let manager = SwarmTaskManager {
            tasks: DashMap::new(),
            cancel_map: DashMap::new(),
            pause_map: DashMap::new(),
        };
        let handle = manager.register_task("lifecycle_test", "test");
        assert!(manager.pause_task(&handle.task_id));
        handle.last_progress_secs.store(0, Ordering::Release);
        assert!(manager.resume_task(&handle.task_id));
        assert!(!handle.pause_flag.load(Ordering::Acquire));
        assert!(handle.last_progress_secs.load(Ordering::Acquire) > 0);
        assert!(manager.kill_task(&handle.task_id));
        assert!(handle.is_cancelled());
        handle.mark_failed("late error");
        assert_eq!(
            handle.status.load(Ordering::Acquire),
            TaskStatus::Killed as u8
        );
        assert_eq!(handle.result.read().as_deref(), Some("Killed by pulse"));
    }

    #[test]
    fn paused_task_observes_resume_and_cancellation() {
        for cancel in [false, true] {
            let handle = Arc::new(TaskHandle {
                task_id: "test".into(),
                cancel_flag: Arc::new(AtomicBool::new(false)),
                pause_flag: Arc::new(AtomicBool::new(true)),
                last_progress_secs: Arc::new(AtomicU64::new(0)),
                progress_count: Arc::new(AtomicU64::new(0)),
                status: Arc::new(AtomicU8::new(TaskStatus::Paused as u8)),
                result: Arc::new(parking_lot::RwLock::new(None)),
                name: "test".into(),
                start_time: Instant::now(),
            });
            let worker_handle = Arc::clone(&handle);
            let (tx, rx) = std::sync::mpsc::channel();
            let worker = std::thread::spawn(move || {
                // An unrelated park token must not bypass the pause.
                std::thread::current().unpark();
                worker_handle.check_pause();
                tx.send(()).unwrap();
            });
            assert!(matches!(
                rx.recv_timeout(Duration::from_millis(200)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ));
            if cancel {
                handle.cancel_flag.store(true, Ordering::Release);
            } else {
                handle.pause_flag.store(false, Ordering::Release);
            }
            rx.recv_timeout(Duration::from_secs(5))
                .expect("paused task did not wake");
            worker.join().unwrap();
        }
    }

    #[test]
    fn test_record_execution_telemetry_does_not_deadlock_on_repeat_calls() {
        // Regression: record_execution_telemetry used to call self.save_history()
        // (which iterates every DashMap shard) while still holding a live
        // `entry()` guard on this category's own shard, self-deadlocking the
        // thread since DashMap's shard locks aren't reentrant. This never
        // surfaced in tests because every real caller (LlamaCppEngine's
        // generation loop, via TaskHandle::mark_completed) is bypassed under
        // cfg!(test). Call it directly, twice (so the entry already exists on
        // the second call, exercising the same guard/iterate interleaving a
        // real completed inference triggers), off the test-harness thread so
        // a regression hangs this test instead of the whole suite.
        let category = format!("test_no_deadlock_{}", rand_id());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let store = TelemetryHistoryStore::global();
            store.record_execution_telemetry(&category, 10, 5);
            store.record_execution_telemetry(&category, 20, 5);
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "record_execution_telemetry did not return within 5s - likely deadlocked"
        );
    }
}
