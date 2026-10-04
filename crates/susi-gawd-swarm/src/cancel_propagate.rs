//! Propagate cancellation and remaining deadlines through the swarm (VC-201-026).

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::susi_error::{EaiError, EaiResult};

#[derive(Debug)]
struct RuntimeState {
    cancelled: bool,
    deadline_unix_ms: Option<u64>,
    active: usize,
    scopes: BTreeSet<String>,
    unresolved_remotes: BTreeMap<String, String>,
}

/// A cloneable cancellation capability shared by every operation in one
/// mission. It is deliberately independent of the serializable CancelToken:
/// workers need a live signal and a bounded quiescence counter, neither of
/// which belongs in persisted mission state.
#[derive(Clone, Debug)]
pub struct CancellationHandle {
    state: Arc<(Mutex<RuntimeState>, Condvar)>,
}

impl Default for CancellationHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationHandle {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new((
                Mutex::new(RuntimeState {
                    cancelled: false,
                    deadline_unix_ms: None,
                    active: 0,
                    scopes: BTreeSet::new(),
                    unresolved_remotes: BTreeMap::new(),
                }),
                Condvar::new(),
            )),
        }
    }

    /// Build a live handle with a wall-clock deadline. The persisted token
    /// remains second-granularity; this constructor is for bounded
    /// in-process operations and tests.
    #[must_use]
    pub fn with_deadline_after(duration: Duration) -> Self {
        let handle = Self::new();
        let now = now_unix_millis();
        let millis = duration.as_millis().min(u64::MAX as u128) as u64;
        handle.set_deadline_unix_ms(now.saturating_add(millis));
        handle
    }

    fn set_deadline_unix_ms(&self, deadline_unix_ms: u64) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.deadline_unix_ms = Some(deadline_unix_ms);
        wake.notify_all();
    }

    fn set_deadline_unix_secs(&self, deadline_unix: u64) {
        self.set_deadline_unix_ms(deadline_unix.saturating_mul(1_000));
    }

    /// Register a task-manager scope that must be cancelled with this live
    /// capability. This makes cancellation effective even when the caller
    /// holds only a cloned handle and cannot borrow the CancelBus.
    pub fn register_scope(&self, scope: &str) {
        let (lock, _) = &*self.state;
        let should_cancel = {
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.scopes.insert(scope.to_string());
            state.cancelled || deadline_expired(state.deadline_unix_ms)
        };
        if should_cancel {
            crate::susi_core::task_manager::SwarmTaskManager::global().cancel_scope(scope);
        }
    }

    /// Request cancellation and notify every waiter. Task-manager scopes are
    /// cancelled here as well, so running exec_command processes receive the
    /// same signal as model/peer workers.
    pub fn cancel(&self) {
        let scopes = {
            let (lock, wake) = &*self.state;
            let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
            state.cancelled = true;
            wake.notify_all();
            state.scopes.iter().cloned().collect::<Vec<_>>()
        };
        let manager = crate::susi_core::task_manager::SwarmTaskManager::global();
        for scope in scopes {
            manager.cancel_scope(&scope);
        }
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        let (lock, _) = &*self.state;
        lock.lock().unwrap_or_else(|e| e.into_inner()).cancelled
    }

    #[must_use]
    pub fn deadline_exceeded(&self) -> bool {
        let (lock, _) = &*self.state;
        let state = lock.lock().unwrap_or_else(|e| e.into_inner());
        deadline_expired(state.deadline_unix_ms)
    }

    #[must_use]
    pub fn should_stop(&self) -> bool {
        self.is_cancelled() || self.deadline_exceeded()
    }

    /// Number of operations that have not yet acknowledged cancellation.
    #[must_use]
    pub fn active_operations(&self) -> usize {
        let (lock, _) = &*self.state;
        lock.lock().unwrap_or_else(|e| e.into_inner()).active
    }

    /// Record remote work whose transport cannot guarantee rollback. The
    /// caller may return promptly, but the remote is never presented as
    /// completed after local cancellation.
    pub fn record_unresolved_remote(&self, remote: &str, reason: &str) {
        let (lock, _) = &*self.state;
        lock.lock()
            .unwrap_or_else(|e| e.into_inner())
            .unresolved_remotes
            .insert(remote.to_string(), reason.to_string());
    }

    #[must_use]
    pub fn unresolved_remotes(&self) -> BTreeMap<String, String> {
        let (lock, _) = &*self.state;
        lock.lock()
            .unwrap_or_else(|e| e.into_inner())
            .unresolved_remotes
            .clone()
    }

    fn try_begin(&self) -> Option<CancellationGuard> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if state.cancelled || deadline_expired(state.deadline_unix_ms) {
            return None;
        }
        state.active += 1;
        Some(CancellationGuard {
            state: Arc::clone(&self.state),
        })
    }

    /// Cancel and wait briefly for registered operations to acknowledge it.
    /// A false result is intentionally observable: a remote request or a
    /// third-party model may be irreversible even though its local waiter
    /// returned promptly.
    #[must_use]
    pub fn cancel_and_wait(&self, timeout: Duration) -> bool {
        self.cancel();
        self.wait_for_idle(timeout)
    }

    #[must_use]
    pub fn wait_for_idle(&self, timeout: Duration) -> bool {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if state.active == 0 {
            return true;
        }
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return state.active == 0;
            }
            match wake.wait_timeout(state, remaining) {
                Ok((next, result)) => {
                    state = next;
                    if state.active == 0 {
                        return true;
                    }
                    if result.timed_out() {
                        return false;
                    }
                }
                Err(poisoned) => {
                    state = poisoned.into_inner().0;
                }
            }
        }
    }
}

fn deadline_expired(deadline_unix_ms: Option<u64>) -> bool {
    deadline_unix_ms.is_some_and(|deadline| now_unix_millis() >= deadline)
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

struct CancellationGuard {
    state: Arc<(Mutex<RuntimeState>, Condvar)>,
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state.active.saturating_sub(1);
        wake.notify_all();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancellableResult<T> {
    Completed(T),
    Cancelled,
    DeadlineExceeded,
}

/// Run a model, external-agent, or other blocking request with a bounded
/// local wait. The request is detached when the deadline/cancel fires; its
/// result is dropped and cannot be published by this call site. The guard
/// remains active until the detached request really returns, allowing the
/// caller to report unresolved work honestly.
pub fn run_cancellable<T, F>(
    handle: &CancellationHandle,
    operation: F,
) -> EaiResult<CancellableResult<T>>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let Some(guard) = handle.try_begin() else {
        return Ok(if handle.deadline_exceeded() {
            CancellableResult::DeadlineExceeded
        } else {
            CancellableResult::Cancelled
        });
    };
    let (sender, receiver) = sync_channel(1);
    std::thread::Builder::new()
        .name("susi-swarm-cancellable".to_string())
        .spawn(move || {
            let _guard = guard;
            let value = operation();
            let _ = sender.send(value);
        })
        .map_err(|e| EaiError::process(format!("spawn cancellable operation: {e}")))?;

    loop {
        match receiver.recv_timeout(Duration::from_millis(20)) {
            Ok(value) => {
                if handle.is_cancelled() {
                    return Ok(CancellableResult::Cancelled);
                }
                if handle.deadline_exceeded() {
                    return Ok(CancellableResult::DeadlineExceeded);
                }
                return Ok(CancellableResult::Completed(value));
            }
            Err(RecvTimeoutError::Timeout) => {
                if handle.is_cancelled() {
                    return Ok(CancellableResult::Cancelled);
                }
                if handle.deadline_exceeded() {
                    return Ok(CancellableResult::DeadlineExceeded);
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err(EaiError::process(
                    "cancellable operation exited without a result",
                ));
            }
        }
    }
}

/// Dispatch an external agent with the same deadline and late-result
/// suppression as local model calls. A queued remote run cannot be rolled
/// back, so cancellation is recorded as unresolved rather than accepted as
/// completion.
pub fn run_external_agent_cancellable(
    handle: &CancellationHandle,
    workspace: &Path,
    kind: &str,
    agent: &str,
    prompt: &str,
) -> EaiResult<CancellableResult<Result<serde_json::Value, String>>> {
    let workspace = workspace.to_path_buf();
    let kind = kind.to_string();
    let agent = agent.to_string();
    let prompt = prompt.to_string();
    run_cancellable(handle, move || {
        crate::susi_core::plane_bus::agents::external_run(&workspace, &kind, &agent, &prompt)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Execute a command in its own process group and kill that group on cancel.
/// This is the swarm-native entry point for commands that cannot use the
/// daemon tool plane and proves that descendants, not just the immediate
/// shell, are reaped.
pub fn run_cancellable_command(
    command: &[String],
    workspace: &Path,
    handle: &CancellationHandle,
) -> EaiResult<CancellableResult<ProcessOutput>> {
    let Some(program) = command.first() else {
        return Err(EaiError::process("cancellable command cannot be empty"));
    };
    let Some(guard) = handle.try_begin() else {
        return Ok(if handle.deadline_exceeded() {
            CancellableResult::DeadlineExceeded
        } else {
            CancellableResult::Cancelled
        });
    };
    let mut builder = {
        #[cfg(unix)]
        {
            let mut command_builder = Command::new("setsid");
            command_builder.arg(program).args(&command[1..]);
            command_builder
        }
        #[cfg(not(unix))]
        {
            let mut command_builder = Command::new(program);
            command_builder.args(&command[1..]);
            command_builder
        }
    };
    let mut child = builder
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| EaiError::process(format!("spawn cancellable command: {e}")))?;
    let child_id = child.id();
    let stdout = child.stdout.take().map(spawn_pipe_reader);
    let stderr = child.stderr.take().map(spawn_pipe_reader);

    let exit = loop {
        if handle.should_stop() {
            terminate_process_group(&mut child, child_id);
            break child.wait().ok();
        }
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                terminate_process_group(&mut child, child_id);
                break child.wait().ok();
            }
        }
    };
    let stdout = join_pipe_reader(stdout)?;
    let stderr = join_pipe_reader(stderr)?;
    drop(guard);
    let output = ProcessOutput {
        status: exit.and_then(|status| status.code()),
        stdout,
        stderr,
    };
    if handle.is_cancelled() {
        Ok(CancellableResult::Cancelled)
    } else if handle.deadline_exceeded() {
        Ok(CancellableResult::DeadlineExceeded)
    } else {
        Ok(CancellableResult::Completed(output))
    }
}

fn spawn_pipe_reader<R>(mut pipe: R) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    })
}

fn join_pipe_reader(
    reader: Option<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>,
) -> EaiResult<String> {
    let Some(reader) = reader else {
        return Ok(String::new());
    };
    let bytes = match reader.join() {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => return Err(EaiError::io(format!("read command output: {e}"))),
        Err(_) => return Err(EaiError::process("command output reader panicked")),
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn terminate_process_group(child: &mut Child, child_id: u32) {
    #[cfg(unix)]
    {
        let group = format!("-{child_id}");
        let _ = Command::new("kill")
            .args(["-TERM", "--", group.as_str()])
            .stderr(Stdio::null())
            .status();
        std::thread::sleep(Duration::from_millis(10));
        let _ = Command::new("kill")
            .args(["-KILL", "--", group.as_str()])
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerKind {
    Local,
    ExternalAgent,
    PeerDispatch,
    ModelCall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelToken {
    pub root_id: String,
    pub deadline_unix: u64,
    pub cancelled: bool,
}

impl CancelToken {
    #[must_use]
    pub fn fresh(root_id: &str, deadline_unix: u64) -> Self {
        Self {
            root_id: root_id.to_string(),
            deadline_unix,
            cancelled: false,
        }
    }

    #[must_use]
    pub fn remaining_secs(&self, now_unix: u64) -> Option<u64> {
        if self.cancelled || now_unix >= self.deadline_unix {
            None
        } else {
            Some(self.deadline_unix - now_unix)
        }
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
}

// No PartialEq/Eq: `signal` is a live atomic, equality on it is meaningless.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Descendant {
    pub id: String,
    pub kind: WorkerKind,
    pub cancellable: bool,
    /// Task-registry scope whose running commands are killed on propagate
    /// (T-DEVIN-8). Without it, propagate only records the id.
    #[serde(default)]
    pub cancel_scope: Option<String>,
    /// Shared flag set on propagate; workers poll it between steps so they
    /// stop issuing new work mid-batch instead of running to completion.
    #[serde(skip)]
    pub signal: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

#[derive(Debug, Default)]
pub struct CancelBus {
    pub token: Option<CancelToken>,
    pub terminated: BTreeSet<String>,
    pub irreversible_running: BTreeSet<String>,
    /// Cancellable work whose termination was requested but not observed
    /// before the bounded propagation wait expired.
    pub termination_pending: BTreeSet<String>,
    pub descendants: BTreeMap<String, Descendant>,
    runtime: CancellationHandle,
}

impl CancelBus {
    pub fn register(&mut self, d: Descendant) {
        if d.cancellable {
            if let Some(scope) = &d.cancel_scope {
                self.runtime.register_scope(scope);
            }
            if self.runtime.should_stop() {
                if let Some(signal) = &d.signal {
                    signal.store(true, std::sync::atomic::Ordering::Release);
                }
            }
        }
        self.descendants.insert(d.id.clone(), d);
    }

    pub fn set_token(&mut self, token: CancelToken) {
        self.runtime.set_deadline_unix_secs(token.deadline_unix);
        if token.cancelled {
            self.runtime.cancel();
        }
        self.token = Some(token);
    }

    /// Clone the live cancellation capability for worker threads and
    /// blocking model/peer requests.
    #[must_use]
    pub fn handle(&self) -> CancellationHandle {
        self.runtime.clone()
    }

    /// Propagate cancel: terminate cancellable descendants; report irreversible.
    ///
    /// Termination is real, not bookkeeping (T-DEVIN-8): each descendant's
    /// shared `signal` is set so workers abort between steps, and its
    /// `cancel_scope` cancels every task the worker registered — running
    /// `exec_command` children are killed and reaped within one poll
    /// interval instead of continuing to mutate files or burn CPU.
    pub fn propagate(&mut self) -> Vec<String> {
        let Some(tok) = self.token.as_mut() else {
            return Vec::new();
        };
        tok.cancel();
        let quiesced = self.runtime.cancel_and_wait(Duration::from_millis(250));
        let mut reports = Vec::new();
        for d in self.descendants.values() {
            if d.cancellable {
                if let Some(sig) = &d.signal {
                    sig.store(true, std::sync::atomic::Ordering::Release);
                }
                if let Some(scope) = &d.cancel_scope {
                    crate::susi_core::task_manager::SwarmTaskManager::global().cancel_scope(scope);
                }
                if quiesced {
                    self.terminated.insert(d.id.clone());
                    self.termination_pending.remove(&d.id);
                } else {
                    self.termination_pending.insert(d.id.clone());
                    reports.push(format!(
                        "cancellable worker termination unresolved: {}",
                        d.id
                    ));
                }
            } else {
                self.irreversible_running.insert(d.id.clone());
                reports.push(format!("irreversible remote job still running: {}", d.id));
            }
        }
        reports
    }

    #[must_use]
    pub fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}
