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
    #[cfg(windows)]
    let mut unrelated = UnrelatedTestProcess::start();
    let command = descendant_command(&marker);
    let handle = CancellationHandle::with_deadline_after(Duration::from_secs(30));
    let worker_handle = handle.clone();
    let worker_dir = dir.clone();
    let worker =
        std::thread::spawn(move || run_cancellable_command(&command, &worker_dir, &worker_handle));

    let marker_deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < marker_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists(), "live shell did not start");
    let pid_deadline = Instant::now() + Duration::from_secs(2);
    let (child_pid, pids) = loop {
        let pids = fs::read_to_string(&marker).unwrap_or_default();
        if let Some(pid) = pids
            .split_whitespace()
            .nth(1)
            .and_then(|pid| pid.parse::<u32>().ok())
        {
            break (pid, pids);
        }
        assert!(
            Instant::now() < pid_deadline,
            "shell pid marker stayed incomplete"
        );
        std::thread::sleep(Duration::from_millis(5));
    };

    #[cfg(windows)]
    let observer = observe_windows_descendant(&dir, &pids);
    #[cfg(not(windows))]
    {
        let _ = pids;
        assert!(
            process_is_alive(child_pid),
            "descendant was not alive before cancellation"
        );
    }

    #[cfg(windows)]
    unrelated.assert_alive();
    let started = Instant::now();
    handle.cancel();
    let result = worker.join().unwrap().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "process cancellation exceeded bound: {:?}",
        started.elapsed()
    );
    assert_eq!(result, CancellableResult::Cancelled);

    #[cfg(windows)]
    {
        use std::io::Write;
        let mut observer = observer;
        observer.stdin.take().unwrap().write_all(b"\n").unwrap();
        let observed = observer.wait_with_output().unwrap();
        assert!(
            observed.status.success(),
            "descendant observation failed: {}",
            String::from_utf8_lossy(&observed.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&observed.stdout).trim(),
            "exited",
            "descendant process {child_pid} survived group cancellation"
        );
    }
    #[cfg(not(windows))]
    {
        let exited_deadline = Instant::now() + Duration::from_secs(1);
        while process_is_alive(child_pid) && Instant::now() < exited_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            !process_is_alive(child_pid),
            "descendant process {child_pid} survived group cancellation"
        );
    }
    #[cfg(windows)]
    unrelated.assert_alive();
    // Best-effort cleanup: a spawned descendant may still hold a handle to the
    // temp dir, so "Directory not empty" on remove must not fail the test.
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn swarm_gap_real_cancellation_bounds_hung_model_and_drops_late_result() {
    let handle = CancellationHandle::with_deadline_after(Duration::from_secs(5));
    let started = Arc::new(AtomicBool::new(false));
    let late_result = Arc::new(AtomicBool::new(false));
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let worker_handle = handle.clone();
    let worker_started = Arc::clone(&started);
    let worker_late = Arc::clone(&late_result);
    let worker = std::thread::spawn(move || {
        run_cancellable(&worker_handle, move || {
            worker_started.store(true, Ordering::Release);
            let _ = release_rx.recv_timeout(Duration::from_secs(5));
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
    let _ = release_tx.send(());
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
    // Readiness handshake: the peer signals the instant it accepts the
    // connection, so the test cancels only once the request is in flight.
    // Before connecting, the dispatch checks eviction, the roster and the
    // credential release, and any of those can return without connecting
    // (an evicted node, a refused credential). The dispatch therefore
    // signals its own return on the same channel: whichever happens first
    // decides, so a dispatch that gave up before the peer fails at once
    // with its real outcome instead of waiting out the bound. The bound
    // only fails a dispatch that neither connects nor returns.
    enum Event {
        PeerAccepted,
        DispatchReturned,
    }
    let (events_tx, events_rx) = std::sync::mpsc::channel::<Event>();
    let (release_server_tx, release_server_rx) = std::sync::mpsc::channel::<()>();
    let accepted_tx = events_tx.clone();
    let server = std::thread::spawn(move || {
        let (_stream, _) = listener.accept().unwrap();
        // The receiver is gone only when the test already failed.
        let _ = accepted_tx.send(Event::PeerAccepted);
        let _ = release_server_rx.recv_timeout(Duration::from_secs(5));
    });

    // Backstop only: the test cancels explicitly, long before this expires.
    let handle = CancellationHandle::with_deadline_after(Duration::from_secs(120));
    let worker_handle = handle.clone();
    let worker_address = address.clone();
    let worker = std::thread::spawn(move || {
        let outcome = SusiSupervisor::dispatch_peer_task_cancellable(
            &worker_address,
            "reason",
            r#"{"goal":"hang"}"#,
            &worker_handle,
        );
        let _ = events_tx.send(Event::DispatchReturned);
        outcome
    });
    match events_rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Event::PeerAccepted) => {}
        Ok(Event::DispatchReturned) => panic!(
            "dispatch returned before connecting to the peer: {:?}",
            worker.join().unwrap()
        ),
        Err(error) => panic!("peer request neither started nor returned: {error}"),
    }
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
    let _ = release_server_tx.send(());
    server.join().unwrap();
    drop(env);
    // Best-effort cleanup: a spawned peer process may still hold a handle to
    // the temp dir, so "Directory not empty" on remove must not fail the test.
    let _ = fs::remove_dir_all(config_dir);
}

#[test]
fn swarm_gap_real_cancellation_zero_deadline_returns_deadline_exceeded() {
    let handle = CancellationHandle::with_deadline_after(Duration::ZERO);
    let executed = Arc::new(AtomicBool::new(false));
    let worker_executed = Arc::clone(&executed);
    let outcome = run_cancellable(&handle, move || {
        worker_executed.store(true, Ordering::Release);
        "should-not-complete"
    })
    .expect("run_cancellable");
    assert_eq!(outcome, CancellableResult::DeadlineExceeded);
}

#[test]
fn swarm_gap_real_cancellation_pre_cancelled_handle_returns_cancelled_immediately() {
    let handle = CancellationHandle::with_deadline_after(Duration::from_secs(30));
    handle.cancel();
    let outcome = run_cancellable(&handle, || "never-runs").expect("run_cancellable");
    assert_eq!(outcome, CancellableResult::Cancelled);
}

/// A killed process stays a zombie until its parent reaps it, and `kill -0`
/// answers for a zombie too. The descendant's parent is the group the test
/// just killed, so the container's init reaps it — a cloud container's init
/// takes over a second to, so on Linux a zombie counts as exited.
#[cfg(not(windows))]
fn process_is_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    if let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the parenthesized command name, which may itself
        // contain spaces or parentheses.
        let state = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next());
        return state != Some("Z");
    }
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn descendant_command(marker: &std::path::Path) -> Vec<String> {
    let marker_sh = marker.to_string_lossy().replace('\\', "/");
    let script = format!(
        "sleep 30 & child=$!; printf '%s %s\\n' \"$$\" \"$child\" > \"{marker_sh}\"; wait \"$child\""
    );
    vec!["sh".to_string(), "-c".to_string(), script]
}

#[cfg(windows)]
fn descendant_command(marker: &std::path::Path) -> Vec<String> {
    let marker = marker.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $child=Start-Process powershell.exe -WindowStyle Hidden -PassThru \
         -ArgumentList '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30'; \
         [IO.File]::WriteAllText('{marker}', \
         ('{{0}} {{1}} {{2}}' -f $PID,$child.Id,$child.StartTime.ToUniversalTime().ToFileTimeUtc())); \
         $child.WaitForExit()"
    );
    vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-WindowStyle".into(),
        "Hidden".into(),
        "-Command".into(),
        script,
    ]
}

#[cfg(windows)]
fn observe_windows_descendant(dir: &std::path::Path, pids: &str) -> std::process::Child {
    use std::process::{Command, Stdio};
    let mut fields = pids.split_whitespace();
    let _parent_pid = fields.next().unwrap().parse::<u32>().unwrap();
    let child_pid = fields.next().unwrap().parse::<u32>().unwrap();
    let created = fields.next().unwrap().parse::<u64>().unwrap();
    // Retain the native process handle before cancellation and bind it to the
    // creation time recorded by its parent. PID reuse or observation errors
    // must fail closed. Cleanup acts only on that pinned test-owned process,
    // after recording whether the unchanged production runner terminated it.
    let script = format!(
        "$ErrorActionPreference='Stop'; \
         $child=[Diagnostics.Process]::GetProcessById({child_pid}); \
         $owned=$false; \
         try {{ \
           [void]$child.Handle; \
           if ($child.StartTime.ToUniversalTime().ToFileTimeUtc() -ne {created}) {{ throw 'process identity changed' }}; \
           $owned=$true; \
           if ($child.HasExited) {{ throw 'descendant was not alive before cancellation' }}; \
           [IO.File]::WriteAllText('observing','alive'); \
           [void][Console]::ReadLine(); \
           if ($child.WaitForExit(1000)) {{ [Console]::WriteLine('exited') }} \
           else {{ [Console]::WriteLine('alive') }} \
         }} finally {{ \
           if ($owned -and !$child.HasExited) {{ $child.Kill(); if (!$child.WaitForExit(5000)) {{ throw 'owned descendant cleanup timed out' }} }}; \
           $child.Dispose() \
         }}"
    );
    let mut observer = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            &script,
        ])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let ready = dir.join("observing");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.exists() && Instant::now() < deadline {
        if observer.try_wait().unwrap().is_some() {
            let output = observer.wait_with_output().unwrap();
            panic!(
                "descendant observer failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        ready.exists(),
        "descendant observer did not establish live identity"
    );
    assert_eq!(fs::read_to_string(ready).unwrap(), "alive");
    observer
}

/// A sibling of the cancellable root, never a member of its process tree.
#[cfg(windows)]
struct UnrelatedTestProcess(std::process::Child);

#[cfg(windows)]
impl UnrelatedTestProcess {
    fn start() -> Self {
        use std::process::{Command, Stdio};
        Self(
            Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn assert_alive(&mut self) {
        assert!(
            self.0.try_wait().unwrap().is_none(),
            "unrelated process {} must survive cancellation",
            self.0.id()
        );
    }
}

#[cfg(windows)]
impl Drop for UnrelatedTestProcess {
    fn drop(&mut self) {
        // Only the process handle returned by our own spawn is acted on.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
