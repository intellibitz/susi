//! Swarm-Level Metrics
//!
//! Swarm-level throughput, success rate, solution diversity and
//! time-to-resolution, derived from the live task table
//! (`SwarmTaskManager`) — the input `auto_tune` and the SLA monitor read.
//! Distinct from `metrics_aggregator.rs`'s global inference/token counter.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SwarmMetricsSnapshot {
    pub missions_started: usize,
    pub missions_completed: usize,
    pub missions_succeeded: usize,
    /// Count of distinct solution fingerprints seen across completed
    /// missions — a real proxy for "diversity of solutions": how many
    /// different final answers the swarm actually produced.
    pub distinct_solutions: usize,
    pub avg_time_to_resolution_ms: u64,
}

/// Snapshot of the live task table (`SwarmTaskManager::list_tasks`): every
/// registered task is a started mission; `Completed`/`Failed`/`Killed` ones
/// are completed, `Completed` ones succeeded; distinct solutions are the
/// distinct result texts of succeeded tasks. Resolution time is
/// `last_progress - start` (both whole seconds, so this is second-resolution).
pub fn snapshot_from_tasks(tasks: &[susi_core::task_manager::TaskRecord]) -> SwarmMetricsSnapshot {
    use std::sync::atomic::Ordering;
    use susi_core::task_manager::TaskStatus;
    let mut snap = SwarmMetricsSnapshot {
        missions_started: tasks.len(),
        ..SwarmMetricsSnapshot::default()
    };
    let mut solutions = std::collections::HashSet::new();
    let mut total_ms: u64 = 0;
    for task in tasks {
        let status = TaskStatus::from(task.status.load(Ordering::Acquire));
        let finished = matches!(
            status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Killed
        );
        if !finished {
            continue;
        }
        snap.missions_completed += 1;
        let end = task.last_progress_secs.load(Ordering::Acquire);
        total_ms = total_ms.saturating_add(
            end.saturating_sub(task.start_time_secs)
                .saturating_mul(1000),
        );
        if matches!(status, TaskStatus::Completed) {
            snap.missions_succeeded += 1;
            if let Some(result) = task
                .result
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                solutions.insert(result.clone());
            }
        }
    }
    snap.distinct_solutions = solutions.len();
    if snap.missions_completed > 0 {
        snap.avg_time_to_resolution_ms = total_ms / snap.missions_completed as u64;
    }
    snap
}

#[cfg(test)]
mod inline_tests {
    use super::*;
    use susi_core::task_manager::{SwarmTaskManager, TaskRecord, TaskStatus};

    fn make_record(
        id: &str,
        status: TaskStatus,
        start_secs: u64,
        end_secs: u64,
        result: Option<&str>,
    ) -> TaskRecord {
        let mgr = SwarmTaskManager::global();
        let handle = mgr.register_task_scoped(id, "test-intent", Some("swarm-metrics-inline"));
        if let Some(r) = result {
            handle.mark_completed(r);
        } else {
            handle.mark_failed("none");
            *handle.result.write().unwrap_or_else(|e| e.into_inner()) = None;
        }
        let mut rec = mgr
            .tasks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&handle.task_id)
            .unwrap()
            .clone();
        mgr.tasks
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&handle.task_id);
        rec.task_id = id.into();
        rec.status = std::sync::Arc::new(std::sync::atomic::AtomicU8::new(status as u8));
        rec.start_time_secs = start_secs;
        rec.last_progress_secs = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(end_secs));
        rec
    }

    #[test]
    fn snapshot_from_empty_tasks_is_default_zeroed() {
        let snap = snapshot_from_tasks(&[]);
        assert_eq!(snap, SwarmMetricsSnapshot::default());
    }

    #[test]
    fn snapshot_ignores_running_tasks_in_completion_counts() {
        let tasks = vec![
            make_record("t1", TaskStatus::Running, 10, 20, None),
            make_record("t2", TaskStatus::Paused, 10, 25, None),
        ];
        let snap = snapshot_from_tasks(&tasks);
        assert_eq!(snap.missions_started, 2);
        assert_eq!(snap.missions_completed, 0);
        assert_eq!(snap.missions_succeeded, 0);
        assert_eq!(snap.avg_time_to_resolution_ms, 0);
    }

    #[test]
    fn snapshot_tracks_distinct_solutions_and_resolution_time() {
        let tasks = vec![
            make_record("t1", TaskStatus::Completed, 100, 104, Some("ans-a")),
            make_record("t2", TaskStatus::Completed, 100, 106, Some("ans-a")),
            make_record("t3", TaskStatus::Completed, 100, 108, Some("ans-b")),
        ];
        let snap = snapshot_from_tasks(&tasks);
        assert_eq!(snap.missions_started, 3);
        assert_eq!(snap.missions_completed, 3);
        assert_eq!(snap.missions_succeeded, 3);
        assert_eq!(snap.distinct_solutions, 2);
        assert_eq!(snap.avg_time_to_resolution_ms, 6000);
    }

    #[test]
    fn snapshot_counts_failed_and_killed_as_completed_but_not_succeeded() {
        let tasks = vec![
            make_record("t1", TaskStatus::Failed, 50, 52, Some("err")),
            make_record("t2", TaskStatus::Killed, 50, 54, None),
        ];
        let snap = snapshot_from_tasks(&tasks);
        assert_eq!(snap.missions_started, 2);
        assert_eq!(snap.missions_completed, 2);
        assert_eq!(snap.missions_succeeded, 0);
        assert_eq!(snap.distinct_solutions, 0);
        assert_eq!(snap.avg_time_to_resolution_ms, 3000);
    }

    #[test]
    fn snapshot_saturates_duration_when_end_precedes_start() {
        let tasks = vec![make_record(
            "t-clock-skew",
            TaskStatus::Completed,
            200,
            150,
            Some("skew-ok"),
        )];
        let snap = snapshot_from_tasks(&tasks);
        assert_eq!(snap.missions_completed, 1);
        assert_eq!(snap.missions_succeeded, 1);
        assert_eq!(snap.distinct_solutions, 1);
        assert_eq!(snap.avg_time_to_resolution_ms, 0);
    }
}
