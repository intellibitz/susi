//! VC-201-092 mastery: Define and measure operating-layer service objectives.
//!
//! Capability target: Measure availability, task success, queue delay, inference
//! latency, and recovery duration from actual events; publish windows and
//! denominators with explicit unknowns when samples are insufficient.

use crate::protocol::{AgentExecutor, EventQueue, RequestContext};
use crate::protocol::{Message, Part, TaskState};

/// Drives a future to completion on the bare test thread.
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    fn noop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    let waker = unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut fut = std::pin::pin!(fut);
    loop {
        if let Poll::Ready(v) = fut.as_mut().poll(&mut cx) {
            return v;
        }
    }
}

#[test]
fn vc_201_092_mastery_availability_measured_from_events_with_window_and_denominator() {
    use crate::executor::GawdA2AExecutor;

    let executor = GawdA2AExecutor::with_runner(std::sync::Arc::new(|_| Ok("success".to_string())))
        .with_workspace(crate::test_board_fixture::init_board_workspace(
            "vc92-avail",
        ));
    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    // Before sufficient samples, publish_slos marks availability as unknown
    let initial = executor.publish_slos(300, 1);
    assert!(
        initial["availability"].starts_with("unknown"),
        "expected unknown before samples, got {}",
        initial["availability"]
    );

    let mut ctx = RequestContext::new("avail-test-1", "ctx-avail-1");
    ctx.message = Some(Message::user(vec![Part::text("task")]));
    block_on(executor.execute(&ctx, &queue)).expect("execute");

    let task = rx.try_recv().expect("task event");
    assert_eq!(task.status.state, TaskState::Completed);

    let sample = executor.slo_sample(300);
    assert_eq!(sample.availability_num, 1);
    assert_eq!(sample.availability_den, 1);
    let published = executor.publish_slos(300, 1);
    assert_eq!(published["availability"], "100.00% (1/1, window=300s)");
}

#[test]
fn vc_201_092_mastery_task_success_measured_from_events() {
    use crate::executor::GawdA2AExecutor;

    let executor = GawdA2AExecutor::with_runner(std::sync::Arc::new(|intent: &str| {
        if intent.contains("fail") {
            Err("mission failed".to_string())
        } else {
            Ok("mission succeeded".to_string())
        }
    }))
    .with_workspace(crate::test_board_fixture::init_board_workspace("vc92-succ"));

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    // Successful task
    let mut ctx1 = RequestContext::new("success-1", "ctx-1");
    ctx1.message = Some(Message::user(vec![Part::text("succeed")]));
    block_on(executor.execute(&ctx1, &queue)).expect("execute 1");
    let ev1 = rx.try_recv().expect("event 1");
    let task1 = ev1;
    assert_eq!(task1.status.state, TaskState::Completed);

    // Failed task
    let mut ctx2 = RequestContext::new("fail-1", "ctx-2");
    ctx2.message = Some(Message::user(vec![Part::text("fail")]));
    block_on(executor.execute(&ctx2, &queue)).expect("execute 2");
    let ev2 = rx.try_recv().expect("event 2");
    let task2 = ev2;
    assert_eq!(task2.status.state, TaskState::Failed);

    let sample = executor.slo_sample(300);
    assert_eq!(sample.task_success_num, 1);
    assert_eq!(sample.task_success_den, 2);
    let published = executor.publish_slos(300, 2);
    assert_eq!(published["task_success"], "50.00% (1/2, window=300s)");
    // With min_samples=3, marks unknown with explicit denominator
    let unknown_pub = executor.publish_slos(300, 3);
    assert!(unknown_pub["task_success"].starts_with("unknown (0/2, min_samples=3)"));
}

#[test]
fn vc_201_092_mastery_queue_delay_measured_from_events() {
    use crate::executor::GawdA2AExecutor;

    let executor =
        GawdA2AExecutor::with_runner(std::sync::Arc::new(|_| Ok("response".to_string())))
            .with_workspace(crate::test_board_fixture::init_board_workspace(
                "vc92-queue",
            ));

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    let mut ctx = RequestContext::new("queue-test-1", "ctx-queue-1");
    ctx.message = Some(Message::user(vec![Part::text("task")]));

    block_on(executor.execute(&ctx, &queue)).expect("execute");

    let _task = rx.try_recv().expect("task event");

    let sample = executor.slo_sample(60);
    assert_eq!(sample.queue_delay_samples, 1);
    let published = executor.publish_slos(60, 1);
    assert!(
        published["queue_delay"].contains("/1, window=60s)"),
        "expected queue_delay with denominator and window, got {}",
        published["queue_delay"]
    );
}

#[test]
fn vc_201_092_mastery_inference_latency_measured_from_events() {
    use crate::executor::GawdA2AExecutor;

    let executor = GawdA2AExecutor::with_runner(std::sync::Arc::new(|_| {
        std::thread::sleep(std::time::Duration::from_millis(2));
        Ok("response".to_string())
    }))
    .with_workspace(crate::test_board_fixture::init_board_workspace(
        "vc92-latency",
    ));

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    let mut ctx = RequestContext::new("latency-test-1", "ctx-latency-1");
    ctx.message = Some(Message::user(vec![Part::text("task")]));

    block_on(executor.execute(&ctx, &queue)).expect("execute");

    let _task = rx.try_recv().expect("task event");

    let sample = executor.slo_sample(60);
    assert_eq!(sample.inference_latency_samples, 1);
    assert!(
        sample.inference_latency_total_us >= 1000,
        "expected measured inference latency >= 1000us, got {}",
        sample.inference_latency_total_us
    );
    let published = executor.publish_slos(60, 1);
    assert!(published["inference_latency"].contains("/1, window=60s)"));
}

#[test]
fn vc_201_092_mastery_recovery_duration_measured_from_events() {
    use crate::executor::GawdA2AExecutor;

    let executor =
        GawdA2AExecutor::with_runner(std::sync::Arc::new(|_| Ok("recovered".to_string())))
            .with_workspace(crate::test_board_fixture::init_board_workspace("vc92-rec"));

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    let task_id = "recovery-test-1";
    let context_id = "ctx-recovery-1";
    let ctx = RequestContext::new(task_id, context_id);

    // 1. Cancel task (starts recovery timer for task_id)
    block_on(executor.cancel(&ctx, &queue)).expect("cancel");
    let task = rx.try_recv().expect("task event");
    assert_eq!(task.status.state, TaskState::Canceled);

    std::thread::sleep(std::time::Duration::from_millis(2));

    // 2. Recover task by executing it to completion
    let mut retry_ctx = RequestContext::new(task_id, context_id);
    retry_ctx.message = Some(Message::user(vec![Part::text("retry mission")]));
    block_on(executor.execute(&retry_ctx, &queue)).expect("execute recovery");
    let recovered_task = rx.try_recv().expect("recovered event");
    assert_eq!(recovered_task.status.state, TaskState::Completed);

    let sample = executor.slo_sample(120);
    assert_eq!(sample.recovery_duration_samples, 1);
    assert!(sample.recovery_duration_total_us >= 1000);
    let published = executor.publish_slos(120, 1);
    assert!(
        published["recovery_duration"].contains("/1, window=120s)"),
        "expected recovery_duration with denominator and window, got {}",
        published["recovery_duration"]
    );
}
