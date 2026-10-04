//! Production-entry cancellation regressions for VC-201-026.

use crate::amas::{PeerDispatchOutcome, SusiSupervisor};
use crate::cancel_propagate::{
    run_cancellable, run_cancellable_command, CancellableResult, CancellationHandle,
};
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn test_dir(tag: &str) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!(
        "susi-swarm-gap-{tag}-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn swarm_gap_real_cancellation_kills_process_descendants() {
    let dir = test_dir("process");
    let marker = dir.join("pids");
    let script = format!(
        "sleep 30 & child=$!; printf '%s %s\\n' \"$$\" \"$child\" > {}; wait \"$child\"",
        marker.display()
    );
    let command = vec!["sh".to_string(), "-c".to_string(), script];
    let handle = CancellationHandle::with_deadline_after(Duration::from_secs(30));
    let worker_handle = handle.clone();
    let worker_dir = dir.clone();
    let worker =
        std::thread::spawn(move || run_cancellable_command(&command, &worker_dir, &worker_handle));

    let marker_deadline = Instant::now() + Duration::from_secs(2);
    while !marker.exists() && Instant::now() < marker_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists(), "live shell did not start");
    let pid_deadline = Instant::now() + Duration::from_secs(1);
    let child_pid = loop {
        let pids = fs::read_to_string(&marker).unwrap_or_default();
        if let Some(pid) = pids
            .split_whitespace()
            .nth(1)
            .and_then(|pid| pid.parse::<u32>().ok())
        {
            break pid;
        }
        assert!(
            Instant::now() < pid_deadline,
            "shell pid marker stayed incomplete"
        );
        std::thread::sleep(Duration::from_millis(5));
    };

    let started = Instant::now();
    handle.cancel();
    let result = worker.join().unwrap().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "process cancellation exceeded bound: {:?}",
        started.elapsed()
    );
    assert_eq!(result, CancellableResult::Cancelled);

    let exited_deadline = Instant::now() + Duration::from_secs(1);
    while process_is_alive(child_pid) && Instant::now() < exited_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !process_is_alive(child_pid),
        "descendant process {child_pid} survived group cancellation"
    );
    // Best-effort cleanup: a spawned descendant may still hold a handle to the
    // temp dir, so "Directory not empty" on remove must not fail the test.
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn swarm_gap_real_cancellation_bounds_hung_model_and_drops_late_result() {
    let handle = CancellationHandle::with_deadline_after(Duration::from_secs(5));
    let started = Arc::new(AtomicBool::new(false));
    let late_result = Arc::new(AtomicBool::new(false));
    let worker_handle = handle.clone();
    let worker_started = Arc::clone(&started);
    let worker_late = Arc::clone(&late_result);
    let worker = std::thread::spawn(move || {
        run_cancellable(&worker_handle, move || {
            worker_started.store(true, Ordering::Release);
            std::thread::sleep(Duration::from_millis(250));
            worker_late.store(true, Ordering::Release);
            "late-model-result".to_string()
        })
    });

    let start_deadline = Instant::now() + Duration::from_secs(1);
    while !started.load(Ordering::Acquire) && Instant::now() < start_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        started.load(Ordering::Acquire),
        "model worker did not start"
    );
    let cancelled_at = Instant::now();
    handle.cancel();
    let result = worker.join().unwrap().unwrap();
    assert!(
        cancelled_at.elapsed() < Duration::from_millis(200),
        "hung model waiter did not return promptly: {:?}",
        cancelled_at.elapsed()
    );
    assert_eq!(result, CancellableResult::Cancelled);
    assert!(
        !late_result.load(Ordering::Acquire),
        "late model output was published after cancellation"
    );
    assert_eq!(handle.active_operations(), 1);
    assert!(handle.wait_for_idle(Duration::from_secs(1)));
    assert!(late_result.load(Ordering::Acquire));
}

#[test]
fn swarm_gap_real_cancellation_marks_peer_work_unresolved_without_late_mutation() {
    let _env_lock = crate::susi_core::commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let config_dir = test_dir("peer-config");
    let mut env = susi_paths::test_env::EnvGuard::isolated();
    env.set("HOME", &config_dir);
    env.set("XDG_CONFIG_HOME", config_dir.join("config"));
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let accepted = Arc::new(AtomicBool::new(false));
    let server_accepted = Arc::clone(&accepted);
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        server_accepted.store(true, Ordering::Release);
        std::thread::sleep(Duration::from_millis(500));
    });

    let handle = CancellationHandle::with_deadline_after(Duration::from_secs(10));
    let worker_handle = handle.clone();
    let worker_address = address.clone();
    let worker = std::thread::spawn(move || {
        SusiSupervisor::dispatch_peer_task_cancellable(
            &worker_address,
            "reason",
            r#"{"goal":"hang"}"#,
            &worker_handle,
        )
    });
    let accept_deadline = Instant::now() + Duration::from_secs(1);
    while !accepted.load(Ordering::Acquire) && Instant::now() < accept_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        accepted.load(Ordering::Acquire),
        "peer request did not start"
    );
    let cancelled_at = Instant::now();
    handle.cancel();
    let result = worker.join().unwrap();
    assert!(
        cancelled_at.elapsed() < Duration::from_millis(300),
        "peer cancellation exceeded bound: {:?}",
        cancelled_at.elapsed()
    );
    assert!(matches!(
        result,
        PeerDispatchOutcome::UnresolvedRemote { ref peer, .. } if peer == &address
    ));
    assert!(
        handle.unresolved_remotes().contains_key(&address),
        "remote cancellation status was not retained"
    );
    server.join().unwrap();
    drop(env);
    // Best-effort cleanup: a spawned peer process may still hold a handle to
    // the temp dir, so "Directory not empty" on remove must not fail the test.
    let _ = fs::remove_dir_all(config_dir);
}

fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
