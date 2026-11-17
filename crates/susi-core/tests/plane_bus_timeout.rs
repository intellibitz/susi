//! A plane-bus caller waits on another copy no longer than its
//! `bus_timeout_ms`: a slow peer answers the named deadline error in time and
//! keeps its route, and a request whose time is already spent is refused
//! before its handler runs. Without the field a caller waits for the answer.
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use susi_core::plane_bus::PlaneHandler;
use susi_core::plane_bus_ipc::{IpcPlaneBus, DEADLINE_EXCEEDED};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Sleeps for the payload's `sleep_ms`, counting the calls it ran.
struct Slow(Arc<AtomicUsize>);

impl PlaneHandler for Slow {
    fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        let ms = payload.get("sleep_ms").and_then(Value::as_u64).unwrap_or(0);
        std::thread::sleep(Duration::from_millis(ms));
        Ok(json!({ "done": true }))
    }
}

/// An owner copy serving `test.slow` and a caller copy, sharing one rendezvous.
fn copies(tag: &str) -> (PathBuf, IpcPlaneBus, IpcPlaneBus, Arc<AtomicUsize>) {
    // A bus root of its own: lookups scan the rendezvous dir's siblings.
    let dir = std::env::temp_dir().join(format!("susi_bus_timeout_{}_{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let owner = IpcPlaneBus::with_rendezvous(dir.join("1"));
    let caller = IpcPlaneBus::with_rendezvous(dir.join("1"));
    let ran = Arc::new(AtomicUsize::new(0));
    owner.register("test.slow", Arc::new(Slow(Arc::clone(&ran))));
    (dir, owner, caller, ran)
}

#[test]
fn a_slow_peer_answers_the_deadline_error_in_time_and_keeps_its_route() -> TestResult {
    let (dir, _owner, caller, _ran) = copies("slow");
    let sent = Instant::now();
    let err = caller
        .request(
            "test.slow",
            json!({ "sleep_ms": 3000, "bus_timeout_ms": 200 }),
        )
        .err()
        .ok_or("a 3 s handler answered inside 200 ms")?;
    assert!(err.starts_with(DEADLINE_EXCEEDED), "{err}");
    assert!(
        sent.elapsed() < Duration::from_secs(2),
        "{:?}",
        sent.elapsed()
    );
    assert!(
        caller.is_wired("test.slow"),
        "a slow peer is not a dead one"
    );
    let again = caller.request("test.slow", json!({ "bus_timeout_ms": 5000 }))?;
    assert_eq!(again["done"], true);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_spent_request_is_refused_before_its_handler_runs() -> TestResult {
    let (dir, _owner, caller, ran) = copies("spent");
    let err = caller
        .request("test.slow", json!({ "bus_timeout_ms": 0 }))
        .err()
        .ok_or("a spent request ran")?;
    assert!(err.starts_with(DEADLINE_EXCEEDED), "{err}");
    assert!(err.contains("before"), "{err}");
    assert_eq!(ran.load(Ordering::SeqCst), 0, "the handler never ran");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn without_a_timeout_a_caller_waits_for_the_answer() -> TestResult {
    let (dir, _owner, caller, ran) = copies("wait");
    let answer = caller.request("test.slow", json!({ "sleep_ms": 400 }))?;
    assert_eq!(answer["done"], true);
    assert_eq!(ran.load(Ordering::SeqCst), 1);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
