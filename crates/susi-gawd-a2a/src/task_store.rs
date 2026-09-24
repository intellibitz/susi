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
    if terminal.len() <= MAX_TERMINAL_TASKS {
        return;
    }
    // RFC 3339 timestamps sort lexicographically.
    terminal.sort_by(|a, b| a.status.timestamp.cmp(&b.status.timestamp));
    let excess = terminal.len() - MAX_TERMINAL_TASKS;
    for task in terminal.into_iter().take(excess) {
        let _ = store.delete(task.id.as_str()).await;
    }
}
