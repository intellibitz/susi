//! Bounded retention for the A2A task store.
//!
//! ra2a's `TaskVersion` and `GetTaskFuture` are not exported, so a custom
//! `TaskStore` implementation cannot be written outside the crate. The
//! bound is applied here instead: the server owns the `InMemoryTaskStore`
//! and a periodic reaper deletes the oldest terminal tasks via the
//! exported `list`/`delete` surface.

use ra2a::server::{InMemoryTaskStore, TaskStore};
use ra2a::types::ListTasksRequest;
use std::sync::Arc;
use std::time::Duration;

/// Terminal tasks retained for `tasks/get` follow-up — older ones are
/// reaped so a long-lived daemon cannot grow the store without bound.
pub const MAX_TERMINAL_TASKS: usize = 512;

/// Sweep cadence; the bound is approximate between sweeps.
const REAP_INTERVAL: Duration = Duration::from_secs(300);

/// Periodic eviction loop — spawn once alongside the server.
pub async fn reaper(store: Arc<InMemoryTaskStore>) {
    loop {
        tokio::time::sleep(REAP_INTERVAL).await;
        reap_terminal(&store).await;
    }
}

/// Delete the oldest terminal tasks beyond `MAX_TERMINAL_TASKS`.
/// Non-terminal tasks are never evicted — a running task must stay
/// addressable via `tasks/get` until it settles.
pub async fn reap_terminal(store: &InMemoryTaskStore) {
    reap_terminal_with_limit(store, MAX_TERMINAL_TASKS).await;
}

/// Delete the oldest terminal tasks beyond `max_terminal`.
/// Non-terminal tasks are never evicted.
pub async fn reap_terminal_with_limit(store: &InMemoryTaskStore, max_terminal: usize) {
    let mut tasks = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let req = ListTasksRequest {
            page_size: Some(100),
            page_token: page_token.clone(),
            ..ListTasksRequest::default()
        };
        let Ok(resp) = store.list(&req).await else {
            return;
        };
        let next = if resp.next_page_token.is_empty() {
            None
        } else {
            Some(resp.next_page_token)
        };
        tasks.extend(resp.tasks);
        match next {
            Some(t) => page_token = Some(t),
            None => break,
        }
    }
    let mut terminal: Vec<_> = tasks
        .into_iter()
        .filter(|t| t.status.state.is_terminal())
        .collect();
    if terminal.len() <= max_terminal {
        return;
    }
    // RFC 3339 timestamps sort lexicographically.
    terminal.sort_by(|a, b| a.status.timestamp.cmp(&b.status.timestamp));
    let excess = terminal.len() - max_terminal;
    for task in terminal.into_iter().take(excess) {
        let _ = store.delete(task.id.as_str()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ra2a::types::{Task, TaskState, TaskStatus};

    fn make_task(id: &str, state: TaskState, timestamp: &str) -> Task {
        let mut task = Task::new(id, "ctx-reaper-test");
        let mut status = TaskStatus::new(state);
        status.timestamp = Some(timestamp.to_string());
        task.status = status;
        task
    }

    #[tokio::test]
    async fn reap_terminal_with_limit_evicts_oldest_terminal_tasks_and_preserves_active_tasks() {
        let store = InMemoryTaskStore::new();

        // Active (non-terminal) task with the oldest timestamp must never be evicted.
        store
            .save(
                &make_task(
                    "task-working-oldest",
                    TaskState::Working,
                    "2026-11-04T00:00:00Z",
                ),
                None,
                Default::default(),
            )
            .await
            .unwrap();

        // Four terminal tasks ordered by RFC 3339 timestamp.
        for (id, state, ts) in [
            ("task-term-1", TaskState::Completed, "2026-11-04T00:01:00Z"),
            ("task-term-2", TaskState::Failed, "2026-11-04T00:02:00Z"),
            ("task-term-3", TaskState::Canceled, "2026-11-04T00:03:00Z"),
            ("task-term-4", TaskState::Completed, "2026-11-04T00:04:00Z"),
        ] {
            store
                .save(&make_task(id, state, ts), None, Default::default())
                .await
                .unwrap();
        }

        reap_terminal_with_limit(&store, 2).await;

        assert!(store.get("task-working-oldest").await.unwrap().is_some());
        assert!(store.get("task-term-1").await.unwrap().is_none());
        assert!(store.get("task-term-2").await.unwrap().is_none());
        assert!(store.get("task-term-3").await.unwrap().is_some());
        assert!(store.get("task-term-4").await.unwrap().is_some());
    }
}
