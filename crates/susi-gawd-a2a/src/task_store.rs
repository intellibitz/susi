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

type EvictHook = Box<dyn Fn(&[String]) -> bool + Send + Sync>;

/// In-memory tasks by id (ordered, so `tasks/list` pages are stable).
#[derive(Default)]
pub struct TaskStore {
    tasks: Mutex<BTreeMap<String, Task>>,
    on_evict: Mutex<Vec<EvictHook>>,
}

impl std::fmt::Debug for TaskStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskStore")
            .field("tasks", &self.tasks)
            .finish_non_exhaustive()
    }
}

impl TaskStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn tasks(&self) -> MutexGuard<'_, BTreeMap<String, Task>> {
        self.tasks.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn on_evict(&self, hook: impl Fn(&[String]) -> bool + Send + Sync + 'static) {
        self.on_evict
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(hook));
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
    let evicted: Vec<String> = {
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
        terminal
            .into_iter()
            .take(excess)
            .filter_map(|(_, id)| tasks.remove(&id).map(|_| id))
            .collect()
    };
    if !evicted.is_empty() {
        let mut hooks = store.on_evict.lock().unwrap_or_else(|e| e.into_inner());
        hooks.retain(|hook| hook(&evicted));
    }
}

#[cfg(test)]
#[path = "tests/task_store.rs"]
mod tests;
