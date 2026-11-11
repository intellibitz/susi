//! The A2A task store and its bounded retention.
//!
//! Tasks live in memory, keyed by id, for `tasks/get`, `tasks/list`,
//! `tasks/cancel` and push-config follow-up. A long-lived daemon must not
//! grow the store without bound, so a periodic reaper evicts the oldest
//! terminal tasks beyond [`MAX_TERMINAL_TASKS`]; running tasks are never
//! evicted.

use crate::protocol::{ListTasksRequest, ListTasksResponse, Task};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// Terminal tasks retained for `tasks/get` follow-up — older ones are
/// reaped so a long-lived daemon cannot grow the store without bound.
pub const MAX_TERMINAL_TASKS: usize = 512;

/// Sweep cadence; the bound is approximate between sweeps.
const REAP_INTERVAL: Duration = Duration::from_secs(300);

/// `tasks/list` page size when the request names none, and its maximum.
const DEFAULT_PAGE_SIZE: i32 = 50;
const MAX_PAGE_SIZE: i32 = 100;

/// In-memory tasks by id (ordered, so `tasks/list` pages are stable).
#[derive(Debug, Default)]
pub struct TaskStore {
    tasks: Mutex<BTreeMap<String, Task>>,
}

impl TaskStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn tasks(&self) -> MutexGuard<'_, BTreeMap<String, Task>> {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Insert or replace the task's snapshot.
    pub fn save(&self, task: &Task) {
        self.tasks().insert(task.id.clone(), task.clone());
    }

    pub fn get(&self, task_id: &str) -> Option<Task> {
        self.tasks().get(task_id).cloned()
    }

    /// One `tasks/list` page: filtered by context and state, `pageSize`
    /// (default 50, at most 100) tasks from the numeric offset in
    /// `pageToken`; `nextPageToken` is the next offset, empty on the last
    /// page.
    pub fn list(&self, req: &ListTasksRequest) -> ListTasksResponse {
        let matching: Vec<Task> = self
            .tasks()
            .values()
            .filter(|t| req.context_id.as_ref().is_none_or(|c| t.context_id == *c))
            .filter(|t| req.status.is_none_or(|s| t.status.state == s))
            .cloned()
            .collect();
        let total_size = i32::try_from(matching.len()).unwrap_or(i32::MAX);
        let page_size = usize::try_from(
            req.page_size
                .unwrap_or(DEFAULT_PAGE_SIZE)
                .min(MAX_PAGE_SIZE),
        )
        .unwrap_or(DEFAULT_PAGE_SIZE as usize);
        let offset: usize = req
            .page_token
            .as_deref()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        let tasks: Vec<Task> = matching.into_iter().skip(offset).take(page_size).collect();
        let next = offset + tasks.len();
        ListTasksResponse {
            next_page_token: if i32::try_from(next).unwrap_or(i32::MAX) < total_size {
                next.to_string()
            } else {
                String::new()
            },
            tasks,
            page_size: i32::try_from(page_size).unwrap_or(i32::MAX),
            total_size,
        }
    }
}

/// Periodic eviction loop — spawn once alongside the server.
pub async fn reaper(store: Arc<TaskStore>) {
    loop {
        tokio::time::sleep(REAP_INTERVAL).await;
        reap_terminal(&store);
    }
}

/// Delete the oldest terminal tasks beyond `MAX_TERMINAL_TASKS`.
/// Non-terminal tasks are never evicted — a running task must stay
/// addressable via `tasks/get` until it settles.
pub fn reap_terminal(store: &TaskStore) {
    reap_terminal_with_limit(store, MAX_TERMINAL_TASKS);
}

/// Delete the oldest terminal tasks beyond `max_terminal`.
/// Non-terminal tasks are never evicted.
pub fn reap_terminal_with_limit(store: &TaskStore, max_terminal: usize) {
    let mut tasks = store.tasks();
    let mut terminal: Vec<(Option<String>, String)> = tasks
        .values()
        .filter(|t| t.status.state.is_terminal())
        .map(|t| (t.status.timestamp.clone(), t.id.clone()))
        .collect();
    if terminal.len() <= max_terminal {
        return;
    }
    // RFC 3339 timestamps sort lexicographically.
    terminal.sort_by(|a, b| a.0.cmp(&b.0));
    let excess = terminal.len() - max_terminal;
    for (_, id) in terminal.into_iter().take(excess) {
        tasks.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{TaskState, TaskStatus};

    fn make_task(id: &str, state: TaskState, timestamp: &str) -> Task {
        let mut task = Task::new(id, "ctx-reaper-test");
        let mut status = TaskStatus::new(state);
        status.timestamp = Some(timestamp.to_string());
        task.status = status;
        task
    }

    #[test]
    fn reap_terminal_with_limit_evicts_oldest_terminal_tasks_and_preserves_active_tasks() {
        let store = TaskStore::new();

        // Active (non-terminal) task with the oldest timestamp must never be evicted.
        store.save(&make_task(
            "task-working-oldest",
            TaskState::Working,
            "2026-11-04T00:00:00Z",
        ));

        // Four terminal tasks ordered by RFC 3339 timestamp.
        for (id, state, ts) in [
            ("task-term-1", TaskState::Completed, "2026-11-04T00:01:00Z"),
            ("task-term-2", TaskState::Failed, "2026-11-04T00:02:00Z"),
            ("task-term-3", TaskState::Canceled, "2026-11-04T00:03:00Z"),
            ("task-term-4", TaskState::Completed, "2026-11-04T00:04:00Z"),
        ] {
            store.save(&make_task(id, state, ts));
        }

        reap_terminal_with_limit(&store, 2);

        assert!(store.get("task-working-oldest").is_some());
        assert!(store.get("task-term-1").is_none());
        assert!(store.get("task-term-2").is_none());
        assert!(store.get("task-term-3").is_some());
        assert!(store.get("task-term-4").is_some());
    }

    #[test]
    fn reap_terminal_noop_within_limit_and_zero_limit_evicts_all_terminal_states_only() {
        let store = TaskStore::new();

        for (id, state, ts) in [
            (
                "active-submitted",
                TaskState::Submitted,
                "2026-11-04T00:00:01Z",
            ),
            ("active-working", TaskState::Working, "2026-11-04T00:00:02Z"),
            (
                "active-input-required",
                TaskState::InputRequired,
                "2026-11-04T00:00:03Z",
            ),
            (
                "term-completed",
                TaskState::Completed,
                "2026-11-04T00:01:00Z",
            ),
            ("term-failed", TaskState::Failed, "2026-11-04T00:02:00Z"),
            ("term-canceled", TaskState::Canceled, "2026-11-04T00:03:00Z"),
            ("term-rejected", TaskState::Rejected, "2026-11-04T00:04:00Z"),
        ] {
            store.save(&make_task(id, state, ts));
        }

        // Within limit (4 <= 4 and 4 <= MAX_TERMINAL_TASKS): nothing evicted.
        reap_terminal_with_limit(&store, 4);
        reap_terminal(&store);
        for id in [
            "active-submitted",
            "active-working",
            "active-input-required",
            "term-completed",
            "term-failed",
            "term-canceled",
            "term-rejected",
        ] {
            assert!(store.get(id).is_some(), "{id} should remain");
        }

        // Zero limit: all terminal states evicted, all non-terminal states preserved.
        reap_terminal_with_limit(&store, 0);
        for active_id in [
            "active-submitted",
            "active-working",
            "active-input-required",
        ] {
            assert!(
                store.get(active_id).is_some(),
                "{active_id} must be preserved"
            );
        }
        for term_id in [
            "term-completed",
            "term-failed",
            "term-canceled",
            "term-rejected",
        ] {
            assert!(store.get(term_id).is_none(), "{term_id} must be evicted");
        }
    }

    #[test]
    fn max_terminal_tasks_constant_and_empty_store_reap_are_safe() {
        assert_eq!(MAX_TERMINAL_TASKS, 512);
        let store = TaskStore::new();
        reap_terminal_with_limit(&store, 0);
        reap_terminal_with_limit(&store, 10);
        reap_terminal(&store);
        let listed = store.list(&ListTasksRequest::default());
        assert!(listed.tasks.is_empty());
    }

    #[test]
    fn reap_terminal_with_limit_paginates_across_over_100_terminal_tasks_and_keeps_newest_five() {
        let store = TaskStore::new();
        store.save(&make_task(
            "task-working-live",
            TaskState::Working,
            "2026-11-04T00:00:00Z",
        ));

        for i in 0..110usize {
            let hour = i / 60;
            let minute = i % 60;
            let ts = format!("2026-11-04T{hour:02}:{minute:02}:00Z");
            let id = format!("task-paginated-{i:03}");
            store.save(&make_task(&id, TaskState::Completed, &ts));
        }

        reap_terminal_with_limit(&store, 5);

        assert!(store.get("task-working-live").is_some());
        for i in 0..105usize {
            let id = format!("task-paginated-{i:03}");
            assert!(store.get(&id).is_none(), "{id} should be evicted");
        }
        for i in 105..110usize {
            let id = format!("task-paginated-{i:03}");
            assert!(store.get(&id).is_some(), "{id} should remain");
        }
    }

    #[test]
    fn reap_interval_constant_and_reap_terminal_preserve_single_completed_task() {
        assert_eq!(REAP_INTERVAL, Duration::from_secs(300));
        let store = TaskStore::new();
        store.save(&make_task(
            "task-single-done",
            TaskState::Completed,
            "2026-11-04T12:00:00Z",
        ));
        reap_terminal(&store);
        assert!(store.get("task-single-done").is_some());
    }
}
