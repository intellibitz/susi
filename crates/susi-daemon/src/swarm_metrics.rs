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
        total_ms = total_ms.saturating_add(end.saturating_sub(task.start_time_secs).saturating_mul(1000));
        if matches!(status, TaskStatus::Completed) {
            snap.missions_succeeded += 1;
            if let Some(result) = task.result.read().as_ref() {
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
