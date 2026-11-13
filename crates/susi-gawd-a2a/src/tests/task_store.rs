use super::*;
use crate::protocol::{
    AgentCapabilities, AgentCard, AgentExecutor, EventQueue, Handler, RequestContext, Result,
    TaskPushNotificationConfig, TaskState, TaskStatus,
};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

fn make_task(id: &str, state: TaskState, timestamp: &str) -> Task {
    let mut task = Task::new(id, "ctx-reaper-test");
    let mut status = TaskStatus::new(state);
    status.timestamp = Some(timestamp.to_string());
    task.status = status;
    task
}

struct NoopExecutor;

impl AgentExecutor for NoopExecutor {
    fn execute<'a>(
        &'a self,
        _ctx: &'a RequestContext,
        _queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    fn cancel<'a>(
        &'a self,
        _ctx: &'a RequestContext,
        _queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
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

#[test]
fn reaper_eviction_cleans_up_handler_push_configs_without_dropping_kept_tasks() {
    let store = Arc::new(TaskStore::new());
    let card = AgentCard {
        name: "push-test".to_string(),
        description: "test".to_string(),
        supported_interfaces: Vec::new(),
        version: "1.0.0".to_string(),
        documentation_url: None,
        capabilities: AgentCapabilities {
            push_notifications: Some(true),
            ..AgentCapabilities::default()
        },
        security_schemes: std::collections::HashMap::new(),
        security_requirements: Vec::new(),
        default_input_modes: Vec::new(),
        default_output_modes: Vec::new(),
        skills: Vec::new(),
    };
    let handler = Handler::new(Arc::new(NoopExecutor), card, Arc::clone(&store));

    store.save(&make_task(
        "working-1",
        TaskState::Working,
        "2026-11-04T00:00:00Z",
    ));
    store.save(&make_task(
        "done-old",
        TaskState::Completed,
        "2026-11-04T00:01:00Z",
    ));
    store.save(&make_task(
        "done-new",
        TaskState::Completed,
        "2026-11-04T00:02:00Z",
    ));

    for task_id in ["working-1", "done-old", "done-new"] {
        handler
            .create_push_config(TaskPushNotificationConfig {
                tenant: None,
                id: Some(format!("cfg-{task_id}")),
                task_id: Some(task_id.to_string()),
                url: "https://example.invalid/a2a-hook".to_string(),
                token: None,
                authentication: None,
            })
            .unwrap();
    }

    reap_terminal_with_limit(&store, 1);

    // Evicted task lost both its Task snapshot and its handler push config.
    assert!(store.get("done-old").is_none());
    assert!(!handler.has_push_configs("done-old"));
    // Non-evicted working and newest terminal tasks retained their push configs.
    assert!(handler.has_push_configs("working-1"));
    assert!(handler.has_push_configs("done-new"));

    // Dropping the handler auto-prunes its eviction listener without error on later reaps.
    drop(handler);
    reap_terminal_with_limit(&store, 0);
    assert!(store.get("done-new").is_none());
}
