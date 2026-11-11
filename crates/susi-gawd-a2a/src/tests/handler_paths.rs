//! Protocol paths the GAWD executor never takes on the wire (it answers
//! every task with one terminal event), driven over real HTTP through the
//! stack `serve` mounts: a task still working when `message/send` returns
//! and its cancellation, an executor error, an unreachable push endpoint,
//! the end of an SSE stream, and `tasks/list` paging.

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::protocol::{
    A2AError, AgentExecutor, EventQueue, ListTasksRequest, Message, Part, PartContent,
    RequestContext, Result, Task, TaskState, TaskStatus,
};
use crate::task_store::TaskStore;

/// Reports `working` and then never finishes on its own; "boom" fails the
/// execution without writing an event; a cancel writes `canceled`.
struct Staged;

fn text_of(ctx: &RequestContext) -> String {
    ctx.message
        .as_ref()
        .and_then(|m| m.parts.first())
        .and_then(|p| match &p.content {
            PartContent::Text(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

impl AgentExecutor for Staged {
    fn execute<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if text_of(ctx) == "boom" {
                return Err(A2AError::Other("boom".into()));
            }
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status = TaskStatus::with_message(
                TaskState::Working,
                Message::agent(vec![Part::text("started")]),
            );
            queue.send(task)?;
            std::future::pending::<()>().await;
            Ok(())
        })
    }

    fn cancel<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let mut task = Task::new(&ctx.task_id, &ctx.context_id);
            task.status = TaskStatus::new(TaskState::Canceled);
            queue.send(task)
        })
    }
}

fn serve(runtime: &tokio::runtime::Runtime, executor: Arc<dyn AgentExecutor>) -> SocketAddr {
    let card = crate::executor::GawdA2AExecutor::new().agent_card();
    let app = super::app(
        executor,
        card,
        Arc::new(TaskStore::new()),
        Arc::new(|_: &super::VerifierContext| true),
    );
    super::serve_on_loopback(runtime, app)
}

fn call(addr: SocketAddr, path: &str, method: &str, params: Value) -> String {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let payload = serde_json::to_vec(&body).unwrap();
    susi_http_transport::http_call_with_body(
        "POST",
        &format!("http://{addr}{path}"),
        &[("Content-Type", "application/json")],
        Some(&payload),
        20,
        0,
    )
    .unwrap()
    .into_utf8(1_000_000)
    .unwrap()
}

fn rpc(addr: SocketAddr, method: &str, params: Value) -> Value {
    serde_json::from_str(&call(addr, "/", method, params)).unwrap()
}

fn send(text: &str) -> Value {
    json!({"message": {"role": "ROLE_USER", "parts": [{"text": text}], "messageId": "m"}})
}

#[test]
fn a_working_task_is_canceled_through_the_executor() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let addr = serve(&runtime, Arc::new(Staged));
    let sent = rpc(addr, "message/send", send("work"));
    let task = &sent["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_WORKING", "{sent}");
    let id = task["id"].as_str().unwrap().to_string();
    let got = rpc(addr, "GetTask", json!({ "id": id }));
    assert_eq!(got["result"]["status"]["state"], "TASK_STATE_WORKING");
    // `message/send` released the task's queue when it answered, so there
    // is no live stream to resubscribe to.
    let resubscribe: Value = serde_json::from_str(&call(
        addr,
        "/stream",
        "tasks/resubscribe",
        json!({ "id": id }),
    ))
    .unwrap();
    assert_eq!(resubscribe["error"]["code"], -32001, "{resubscribe}");
    let canceled = rpc(addr, "tasks/cancel", json!({ "id": id }));
    assert_eq!(canceled["result"]["id"], id.as_str(), "{canceled}");
    assert_eq!(canceled["result"]["status"]["state"], "TASK_STATE_CANCELED");
    let got = rpc(addr, "tasks/get", json!({ "id": id }));
    assert_eq!(got["result"]["status"]["state"], "TASK_STATE_CANCELED");
    let again = rpc(addr, "CancelTask", json!({ "id": id }));
    assert_eq!(
        again["error"],
        json!({"code": -32603, "message": format!("task cannot be canceled: {id}")})
    );
}

#[test]
fn an_executor_error_is_a_failed_task_and_a_dead_push_endpoint_is_not_an_error() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let addr = serve(&runtime, Arc::new(Staged));
    let mut params = send("boom");
    // Port 9 (discard) has no listener on the loopback: delivery fails and
    // is logged; the reply is unaffected.
    params["configuration"] = json!({"taskPushNotificationConfig": {
        "url": "http://127.0.0.1:9/hook", "token": "t"}});
    let sent = rpc(addr, "SendMessage", params);
    let status = &sent["result"]["task"]["status"];
    assert_eq!(status["state"], "TASK_STATE_FAILED", "{sent}");
    assert_eq!(status["message"]["parts"], json!([{"text": "boom"}]));
    assert_eq!(status["message"]["role"], "ROLE_AGENT");
}

#[test]
fn a_stream_ends_after_the_final_event() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let executor = crate::executor::GawdA2AExecutor::with_runner(Arc::new(|intent: &str| {
        Ok(format!("done: {intent}"))
    }));
    let addr = serve(&runtime, Arc::new(executor));
    let started = Instant::now();
    // Read to the end of the body: the server must close the stream.
    let body = call(addr, "/stream", "message/stream", send("stream me"));
    assert!(started.elapsed() < Duration::from_secs(10));
    let frames: Vec<&str> = body.split_terminator("\n\n").collect();
    assert_eq!(frames.len(), 1, "{body}");
    let event: Value = serde_json::from_str(frames[0].strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(event["id"], 1);
    assert_eq!(
        event["result"]["task"]["status"]["state"],
        "TASK_STATE_COMPLETED"
    );
    assert_eq!(
        event["result"]["task"]["status"]["message"]["parts"][0]["text"],
        "done: stream me"
    );
}

#[test]
fn tasks_list_pages_by_offset_and_filters() {
    let store = TaskStore::new();
    for (id, context, state) in [
        ("a", "c1", TaskState::Completed),
        ("b", "c1", TaskState::Working),
        ("c", "c1", TaskState::Completed),
        ("d", "c2", TaskState::Completed),
    ] {
        let mut task = Task::new(id, context);
        task.status = TaskStatus::new(state);
        store.save(&task);
    }
    let ids = |page: &crate::protocol::ListTasksResponse| -> Vec<String> {
        page.tasks.iter().map(|t| t.id.clone()).collect()
    };
    let first = store.list(&ListTasksRequest {
        context_id: Some("c1".into()),
        page_size: Some(2),
        ..ListTasksRequest::default()
    });
    assert_eq!(ids(&first), ["a", "b"]);
    assert_eq!(
        (
            first.next_page_token.as_str(),
            first.page_size,
            first.total_size
        ),
        ("2", 2, 3)
    );
    let second = store.list(&ListTasksRequest {
        context_id: Some("c1".into()),
        page_size: Some(2),
        page_token: Some(first.next_page_token),
        ..ListTasksRequest::default()
    });
    assert_eq!(ids(&second), ["c"]);
    assert_eq!(second.next_page_token, "");
    let completed = store.list(&ListTasksRequest {
        status: Some(TaskState::Completed),
        ..ListTasksRequest::default()
    });
    assert_eq!(ids(&completed), ["a", "c", "d"]);
    assert_eq!(completed.page_size, 50);
    let clamped = store.list(&ListTasksRequest {
        page_size: Some(1_000),
        ..ListTasksRequest::default()
    });
    assert_eq!((clamped.page_size, clamped.total_size), (100, 4));
    let negative = store.list(&ListTasksRequest {
        page_size: Some(-1),
        ..ListTasksRequest::default()
    });
    assert_eq!(negative.page_size, 50);
}
