//! VC-201-092 mastery: Define and measure operating-layer service objectives.
//!
//! Capability target: Measure availability, task success, queue delay, inference
//! latency, and recovery duration from actual events; publish windows and
//! denominators with explicit unknowns when samples are insufficient.
//!
//! Current status (refutation): The SloSample structure and publish_slos function
//! exist to format measurements, and the tests verify correct formatting behavior
//! (publishes_values_with_denominator, marks_unknown_when_samples_insufficient).
//! However, there is no instrumentation in the A2A executor or task store to
//! actually collect these five metrics from task events. Without this collection
//! path, the capability cannot be exercised on the production path.
//!
//! Missing implementation:
//!   - No availability measurement: nowhere counts task completion vs total attempts
//!   - No task_success measurement: nowhere tracks success vs failure outcomes
//!   - No queue_delay measurement: nowhere measures time spent queued
//!   - No inference_latency measurement: nowhere measures execution time
//!   - No recovery_duration measurement: nowhere measures repair/restart latency
//!
//! This test demonstrates the gap: the data structures are ready, but the
//! measurement pipeline does not exist. Verdict: NOT DELIVERED.

use ra2a::server::{AgentExecutor, EventQueue, RequestContext};
use ra2a::types::{Message, Part, TaskState};

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

/// Availability measurement should count successful task completions vs total attempts.
/// This test demonstrates that the A2A executor publishes task completion events,
/// but there is no instrumentation to count them into an availability metric.
/// A production implementation would:
///   1. Subscribe to TaskState events
///   2. Count Completed/Canceled/Failed outcomes
///   3. Compute availability = Completed / (Completed + Failed + Canceled)
///   4. Roll into a time-windowed sample
///
/// Currently: the event exists (✓), the counter does not (✗).
#[test]
fn vc_201_092_mastery_availability_measurement_missing() {
    use crate::executor::GawdA2AExecutor;

    let executor = GawdA2AExecutor::with_runner(std::sync::Arc::new(|_| Ok("success".to_string())));
    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    // Run a task that completes successfully.
    let mut ctx = RequestContext::new("avail-test-1", "ctx-avail-1");
    ctx.message = Some(Message::user(vec![Part::text("task")]));
    block_on(executor.execute(&ctx, &queue)).expect("execute");

    let ra2a::types::StreamResponse::Task(task) = rx.try_recv().expect("task event") else {
        panic!("expected Task event");
    };

    // Assertion: The event is published (✓)
    assert_eq!(task.status.state, TaskState::Completed);

    // Gap: There is no mechanism to collect this into an availability metric.
    // A complete implementation would instrument the event stream to measure:
    //   availability = (completed tasks) / (all tasks)
    // and roll it into a time-windowed SloSample.
    // Current state: We can observe the event, but cannot measure availability
    // as a windowed operational metric.
}

/// Task success measurement should count successful outcomes vs all outcomes.
/// This test shows the executor publishes task status, but no code accumulates
/// success/failure counts into a metric.
/// A production implementation would:
///   1. Subscribe to task outcomes
///   2. Count success (status == Completed with non-error message) vs failure
///   3. Compute task_success = successful / (successful + failed)
///   4. Roll into a time-windowed sample
///
/// Currently: the outcome exists (✓), the accumulator does not (✗).
#[test]
fn vc_201_092_mastery_task_success_measurement_missing() {
    use crate::executor::GawdA2AExecutor;

    let executor = GawdA2AExecutor::with_runner(std::sync::Arc::new(|intent: &str| {
        if intent.contains("fail") {
            Err("mission failed".to_string())
        } else {
            Ok("mission succeeded".to_string())
        }
    }));

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    // Successful task
    let mut ctx1 = RequestContext::new("success-1", "ctx-1");
    ctx1.message = Some(Message::user(vec![Part::text("succeed")]));
    block_on(executor.execute(&ctx1, &queue)).expect("execute 1");
    let ev1 = rx.try_recv().expect("event 1");
    let ra2a::types::StreamResponse::Task(task1) = ev1 else {
        panic!("expected Task event");
    };
    assert_eq!(task1.status.state, TaskState::Completed);

    // Failed task
    let mut ctx2 = RequestContext::new("fail-1", "ctx-2");
    ctx2.message = Some(Message::user(vec![Part::text("fail")]));
    block_on(executor.execute(&ctx2, &queue)).expect("execute 2");
    let ev2 = rx.try_recv().expect("event 2");
    let ra2a::types::StreamResponse::Task(task2) = ev2 else {
        panic!("expected Task event");
    };
    assert_eq!(task2.status.state, TaskState::Failed);

    // Assertion: Both events are published (✓)
    // task1 is Completed, task2 is Failed

    // Gap: There is no mechanism to count these into a task_success metric.
    // A complete implementation would instrument the event stream to compute:
    //   task_success = (completed tasks) / (all outcomes)
    // and roll it into a time-windowed SloSample.
    // Current state: We can observe both success and failure outcomes,
    // but cannot measure task_success as a windowed operational metric.
}

/// Queue delay measurement should track time from task submission to execution start.
/// This test shows task events contain completion state, but no queue timestamps.
/// A production implementation would:
///   1. Record enqueue_time when task is queued (RequestContext creation)
///   2. Record start_time when execution begins
///   3. Compute queue_delay_ms = start_time - enqueue_time for each task
///   4. Aggregate into a time-windowed sample (p50, p95, p99 or mean)
///
/// Currently: the execution happens (✓), the timestamp instrumentation does not (✗).
#[test]
fn vc_201_092_mastery_queue_delay_measurement_missing() {
    use crate::executor::GawdA2AExecutor;

    let executor =
        GawdA2AExecutor::with_runner(std::sync::Arc::new(|_| Ok("response".to_string())));

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    let mut ctx = RequestContext::new("queue-test-1", "ctx-queue-1");
    ctx.message = Some(Message::user(vec![Part::text("task")]));

    block_on(executor.execute(&ctx, &queue)).expect("execute");

    let ra2a::types::StreamResponse::Task(_task) = rx.try_recv().expect("task event") else {
        panic!("expected Task event");
    };

    // Assertion: The task completes (✓)

    // Gap: RequestContext and TaskState do not carry timestamps.
    // A complete implementation would:
    //   - Add enqueue_timestamp to RequestContext (or task-store entry)
    //   - Add execution_start_timestamp to Task status
    //   - Compute queue_delay_ms = execution_start_timestamp - enqueue_timestamp
    //   - Aggregate into a time-windowed sample
    // Current state: We can run tasks, but cannot measure queue delay as a
    // windowed operational metric because the necessary timestamps are missing.
}

/// Inference latency measurement should track execution time for the mission.
/// This test shows tasks execute and complete, but no instrumentation measures time.
/// A production implementation would:
///   1. Record execution_start_time when the mission begins
///   2. Record execution_end_time when the mission completes
///   3. Compute inference_latency_ms = execution_end_time - execution_start_time
///   4. Aggregate into a time-windowed sample (p50, p95, p99 or mean)
///
/// Currently: the execution happens (✓), the timing instrumentation does not (✗).
#[test]
fn vc_201_092_mastery_inference_latency_measurement_missing() {
    use crate::executor::GawdA2AExecutor;

    let executor =
        GawdA2AExecutor::with_runner(std::sync::Arc::new(|_| Ok("response".to_string())));

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    let mut ctx = RequestContext::new("latency-test-1", "ctx-latency-1");
    ctx.message = Some(Message::user(vec![Part::text("task")]));

    block_on(executor.execute(&ctx, &queue)).expect("execute");

    let ra2a::types::StreamResponse::Task(_task) = rx.try_recv().expect("task event") else {
        panic!("expected Task event");
    };

    // Assertion: The task completes (✓)

    // Gap: Task status does not carry execution duration.
    // A complete implementation would:
    //   - Measure wall-clock time from mission start to mission completion
    //   - Store latency in Task.status or as a separate metric
    //   - Aggregate into a time-windowed sample
    // Current state: We can run missions and observe outcomes, but cannot measure
    // inference latency as a windowed operational metric because execution time
    // is not instrumented.
}

/// Recovery duration measurement should track time to restore a task after failure.
/// This test shows cancellation, but no recovery mechanism or timing.
/// A production implementation would:
///   1. Record failure_time when a task fails
///   2. Trigger recovery (retry, reroute, restore state)
///   3. Record recovery_complete_time when task completes or succeeds
///   4. Compute recovery_duration_ms = recovery_complete_time - failure_time
///   5. Aggregate into a time-windowed sample
///
/// Currently: cancellation exists (✓), recovery instrumentation does not (✗).
#[test]
fn vc_201_092_mastery_recovery_duration_measurement_missing() {
    use crate::executor::GawdA2AExecutor;

    let executor = GawdA2AExecutor::new();

    let queue = EventQueue::new(8);
    let mut rx = queue.subscribe();

    let task_id = "recovery-test-1";
    let context_id = "ctx-recovery-1";
    let ctx = RequestContext::new(task_id, context_id);

    // Cancel a task (simulates a failure/recovery trigger)
    block_on(executor.cancel(&ctx, &queue)).expect("cancel");

    let ra2a::types::StreamResponse::Task(task) = rx.try_recv().expect("task event") else {
        panic!("expected Task event");
    };
    assert_eq!(task.status.state, TaskState::Canceled);

    // Assertion: The cancellation is published (✓)

    // Gap: There is no recovery mechanism or timing instrumentation.
    // A complete implementation would:
    //   - Record when a task begins recovery (after failure or cancellation)
    //   - Execute recovery steps (retry logic, state restoration, rerouting)
    //   - Record when recovery completes or definitively fails
    //   - Measure recovery_duration_ms as the elapsed time
    //   - Aggregate into a time-windowed sample
    // Current state: We can cancel tasks, but recovery is not a measured
    // operational metric. The infrastructure for recovery timing does not exist.
}
