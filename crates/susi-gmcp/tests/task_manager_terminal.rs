#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use susi_gmcp::mcp::{
    model::{CallToolResult, DetailedTask, ErrorCode, ErrorData, TaskStatus},
    task_manager::{TaskExit, TaskManager, TaskOptions},
};

async fn wait_for_terminal(manager: &TaskManager, task_id: &str) -> DetailedTask {
    for _ in 0..200 {
        let task = manager.get_task(task_id).unwrap();
        if task.status().is_terminal() {
            return task;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("task {task_id} did not settle");
}

fn assert_rejected_without_mutation(manager: &TaskManager, task: &DetailedTask) {
    let err = manager.cancel_task(&task.task.task_id).unwrap_err();
    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
    let after = manager.get_task(&task.task.task_id).unwrap();
    assert_eq!(after.status(), task.status());
    assert_eq!(after.task.last_updated_at, task.task.last_updated_at);
}

#[tokio::test]
async fn terminal_tasks_reject_cancellation_without_mutation() {
    let manager = TaskManager::new();
    let completed = manager.spawn(TaskOptions::default(), |_ctx| {
        Box::pin(async { Ok(CallToolResult::default()) })
    });
    let completed = wait_for_terminal(&manager, &completed.task_id).await;
    assert_eq!(completed.status(), TaskStatus::Completed);
    assert_rejected_without_mutation(&manager, &completed);

    let failed = manager.spawn(TaskOptions::default(), |_ctx| {
        Box::pin(async {
            Err(TaskExit::Error(ErrorData::internal_error(
                "expected test failure",
                None,
            )))
        })
    });
    let failed = wait_for_terminal(&manager, &failed.task_id).await;
    assert_eq!(failed.status(), TaskStatus::Failed);
    assert_rejected_without_mutation(&manager, &failed);

    let cancelled = manager.spawn(TaskOptions::default(), |ctx| {
        Box::pin(async move {
            ctx.cancelled().await;
            Err(TaskExit::Cancelled)
        })
    });
    manager.cancel_task(&cancelled.task_id).unwrap();
    let cancelled = wait_for_terminal(&manager, &cancelled.task_id).await;
    assert_eq!(cancelled.status(), TaskStatus::Cancelled);
    assert_rejected_without_mutation(&manager, &cancelled);
}
