//! Exercise the concrete executor's unwind boundary, not only its event queue.
use super::*;
use std::sync::atomic::AtomicUsize;
use susi_gawd::acp::AcpMessageRole;

#[test]
fn callback_panic_cancels_and_releases_a_saturated_executor_with_live_observers() {
    // The production queue admits 32 events. The callback consumes one and
    // then holds the receiver: 33 accepted starts therefore fill it again.
    const ACCEPTED_BEFORE_BLOCK: usize = 33;
    const CALLBACK_PANIC: &str = "borrowed ACP callback fixture panic";
    let cancelled = Arc::new(AtomicBool::new(false));
    let accepted = Arc::new(AtomicUsize::new(0));
    let held_scope = Arc::new(Mutex::new(None::<Arc<ToolScope>>));
    let observers = Arc::new(Mutex::new(Vec::<Box<dyn ScopedToolObserver>>::new()));
    let (at_capacity, capacity_reached) = mpsc::sync_channel(1);
    let (producer_done, producer_returned) = mpsc::sync_channel(1);
    let runner_accepted = Arc::clone(&accepted);
    let runner_scope = Arc::clone(&held_scope);
    let runner_observers = Arc::clone(&observers);
    let executor = SessionExecutor {
        base: Arc::new(GawdAcpExecutor::local(
            ".",
            Arc::new(move |_, _| {
                let scope = scoped_tools::current().expect("executor installs its actual scope");
                *runner_scope.lock().unwrap() = Some(Arc::clone(&scope));
                for index in 0..=ACCEPTED_BEFORE_BLOCK {
                    if index == ACCEPTED_BEFORE_BLOCK {
                        at_capacity.send(()).unwrap();
                    }
                    let Some(observer) = scope.observe_call("queue-probe") else {
                        producer_done.send(()).unwrap();
                        return Ok("producer released".into());
                    };
                    runner_observers.lock().unwrap().push(observer);
                    runner_accepted.fetch_add(1, Ordering::Release);
                }
                panic!("event queue did not apply the expected backpressure");
            }),
        )),
        tools: Arc::new(SessionTools {
            admission: AcpAdmissionPolicy::default(),
            servers: Vec::new(),
            tools: vec![ScopedTool {
                name: "queue-probe".into(),
                description: "Observation-only local queue fixture".into(),
                input_schema: json!({"type":"object"}),
                required_actions: Vec::new(),
            }],
            targets: BTreeMap::new(),
            closed: AtomicBool::new(false),
            next_call: AtomicU64::new(1),
        }),
        lease: Duration::from_secs(30),
    };
    let context = AcpTurnContext {
        session_id: "queue-session".into(),
        turn_id: "queue-turn".into(),
        workspace: None,
        admitted_capabilities: Vec::new(),
        role: AcpMessageRole::User,
        prompt: vec![AcpContentBlock::text(
            "Summarize local fixture measurements.",
        )],
        tool_calls: Vec::new(),
        history: Vec::new(),
    };
    let worker_cancel = Arc::clone(&cancelled);
    let callback_accepted = Arc::clone(&accepted);
    let (finished, completion) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let mut callback_calls = 0;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            executor.execute_turn_with_events(&context, worker_cancel, &mut |event| {
                callback_calls += 1;
                assert!(matches!(event, AcpExecutorEvent::ToolCall(_)));
                capacity_reached
                    .recv_timeout(Duration::from_secs(1))
                    .unwrap();
                assert_eq!(
                    callback_accepted.load(Ordering::Acquire),
                    ACCEPTED_BEFORE_BLOCK
                );
                assert!(matches!(
                    producer_returned.recv_timeout(Duration::from_millis(30)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ));
                // This callback borrows local state and unwinds while the
                // concrete mission producer is waiting on the full queue.
                panic!("borrowed ACP callback fixture panic");
            })
        }));
        let callback_panicked = caught
            .err()
            .is_some_and(|panic| panic.downcast_ref::<&str>() == Some(&CALLBACK_PANIC));
        finished.send((callback_panicked, callback_calls)).unwrap();
    });
    // An incorrect Drain ordering must fail within a bound instead of hanging
    // this test thread on the executor's scoped worker join.
    let (callback_panicked, callback_calls) = completion
        .recv_timeout(Duration::from_secs(3))
        .expect("callback unwind must release the saturated scoped worker");
    worker.join().unwrap();
    assert!(
        callback_panicked,
        "the borrowed callback's own panic must propagate"
    );
    assert_eq!(callback_calls, 1);
    assert!(cancelled.load(Ordering::Acquire));
    assert_eq!(accepted.load(Ordering::Acquire), ACCEPTED_BEFORE_BLOCK);
    assert_eq!(observers.lock().unwrap().len(), ACCEPTED_BEFORE_BLOCK);
    assert!(held_scope
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .check()
        .is_err());
    // Live observers retain event-sender clones until after executor return.
    // Their cleanup must not need the destroyed callback or another receiver.
    observers.lock().unwrap().clear();
    held_scope.lock().unwrap().take();
}
