//! Long-running tool calls as pollable tasks (`tasks/get`, `tasks/update`,
//! `tasks/cancel`): each task runs on its own tokio task, records its
//! terminal payload, and is evicted once its TTL has passed.

use super::model::*;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::Instant,
};
use tokio::sync::{oneshot, watch};

pub const DEFAULT_TASK_TTL_MS: u64 = 300_000;
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;

/// RFC 3339 UTC timestamp with second fractions as needed (0, 3, 6 or 9 digits).
pub fn current_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let nanos = now.subsec_nanos();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let fraction = if nanos == 0 {
        String::new()
    } else if nanos.is_multiple_of(1_000_000) {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos.is_multiple_of(1_000) {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{nanos:09}")
    };
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{fraction}+00:00",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// A random (version 4) UUID from the operating system's random source.
pub fn uuid_v4() -> String {
    let hex = crate::susi_config::cluster_key::random_nonce_hex();
    let mut bytes = [0u8; 16];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = hex
            .get(i * 2..i * 2 + 2)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
            .unwrap_or(0);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// How a task's operation ended other than with a result.
#[derive(Debug)]
pub enum TaskExit {
    Cancelled,
    Error(ErrorData),
}

impl From<ErrorData> for TaskExit {
    fn from(error: ErrorData) -> Self {
        Self::Error(error)
    }
}

pub type TaskFuture = Pin<Box<dyn Future<Output = Result<CallToolResult, TaskExit>> + Send>>;

struct TaskEntry {
    task: Task,
    terminal: Option<TaskPayload>,
    terminal_at: Option<Instant>,
    pending_inputs: HashMap<String, (InputRequest, oneshot::Sender<serde_json::Value>)>,
    used_input_keys: HashSet<String>,
    cancel_requested: bool,
    cancel_signal: watch::Sender<bool>,
    created: Instant,
    join_handle: Option<tokio::task::JoinHandle<()>>,
}

impl TaskEntry {
    fn touch(&mut self) {
        self.task.last_updated_at = current_timestamp();
    }
    fn current_status(&self) -> TaskStatus {
        match &self.terminal {
            Some(payload) => payload.status(),
            None if !self.pending_inputs.is_empty() => TaskStatus::InputRequired,
            None => TaskStatus::Working,
        }
    }
    fn detailed(&self) -> DetailedTask {
        let payload = match &self.terminal {
            Some(payload) => payload.clone(),
            None if !self.pending_inputs.is_empty() => TaskPayload::InputRequired {
                input_requests: self
                    .pending_inputs
                    .iter()
                    .map(|(k, (request, _))| (k.clone(), request.clone()))
                    .collect(),
            },
            None => TaskPayload::Working,
        };
        DetailedTask::new(self.task.clone(), payload)
    }
}

#[derive(Default)]
struct Inner {
    tasks: HashMap<String, TaskEntry>,
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(|e| e.into_inner())
}

/// What a running operation can do with its own task.
#[derive(Clone)]
pub struct TaskContext {
    task_id: String,
    inner: Arc<Mutex<Inner>>,
}

impl TaskContext {
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    /// Park the operation on one input request until `tasks/update`
    /// answers it under `key` (or the task is cancelled).
    pub async fn request_input(
        &self,
        key: impl Into<String>,
        request: InputRequest,
    ) -> Result<serde_json::Value, TaskExit> {
        let key = key.into();
        let (tx, rx) = oneshot::channel();
        {
            let mut inner = lock(&self.inner);
            let entry = inner.tasks.get_mut(&self.task_id).ok_or_else(|| {
                TaskExit::Error(ErrorData::internal_error("task no longer exists", None))
            })?;
            if !entry.used_input_keys.insert(key.clone()) {
                return Err(TaskExit::Error(ErrorData::internal_error(
                    format!("inputRequests key {key:?} was already used for this task"),
                    None,
                )));
            }
            entry.pending_inputs.insert(key, (request, tx));
            entry.touch();
        }
        rx.await.map_err(|_| TaskExit::Cancelled)
    }
    pub fn set_status_message(&self, message: impl Into<String>) {
        if let Some(entry) = lock(&self.inner).tasks.get_mut(&self.task_id) {
            entry.task.status_message = Some(message.into());
            entry.touch();
        }
    }
    pub fn is_cancel_requested(&self) -> bool {
        lock(&self.inner)
            .tasks
            .get(&self.task_id)
            .is_some_and(|e| e.cancel_requested)
    }
    /// Resolves once `tasks/cancel` asked this task to stop.
    pub async fn cancelled(&self) {
        let mut rx = {
            let inner = lock(&self.inner);
            let Some(entry) = inner.tasks.get(&self.task_id) else {
                return;
            };
            if entry.cancel_requested {
                return;
            }
            entry.cancel_signal.subscribe()
        };
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskOptions {
    pub ttl_ms: Option<u64>,
    pub poll_interval_ms: Option<u64>,
    pub status_message: Option<String>,
}

impl Default for TaskOptions {
    fn default() -> Self {
        Self {
            ttl_ms: Some(DEFAULT_TASK_TTL_MS),
            poll_interval_ms: Some(DEFAULT_POLL_INTERVAL_MS),
            status_message: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct TaskManager {
    inner: Arc<Mutex<Inner>>,
}

fn unknown_task(task_id: &str) -> ErrorData {
    ErrorData::invalid_params(format!("unknown task: {task_id}"), None)
}

fn to_object<T: serde::Serialize>(value: &T) -> JsonObject {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::Object(map)) => map,
        _ => JsonObject::new(),
    }
}

impl TaskManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start `make_future`'s operation as a task and return its handle.
    pub fn spawn<F>(&self, options: TaskOptions, make_future: F) -> Task
    where
        F: FnOnce(TaskContext) -> TaskFuture,
    {
        let task_id = uuid_v4();
        let now = current_timestamp();
        let task = Task {
            task_id: task_id.clone(),
            status: TaskStatus::Working,
            status_message: options.status_message,
            created_at: now.clone(),
            last_updated_at: now,
            ttl_ms: options.ttl_ms,
            poll_interval_ms: options.poll_interval_ms,
        };
        {
            let mut inner = lock(&self.inner);
            Self::sweep_expired(&mut inner);
            inner.tasks.insert(
                task_id.clone(),
                TaskEntry {
                    task: task.clone(),
                    terminal: None,
                    terminal_at: None,
                    pending_inputs: HashMap::new(),
                    used_input_keys: HashSet::new(),
                    cancel_requested: false,
                    cancel_signal: watch::channel(false).0,
                    created: Instant::now(),
                    join_handle: None,
                },
            );
        }
        let future = make_future(TaskContext {
            task_id: task_id.clone(),
            inner: self.inner.clone(),
        });
        let inner = self.inner.clone();
        let id = task_id.clone();
        let handle = tokio::spawn(async move {
            let result = future.await;
            let mut inner = lock(&inner);
            if let Some(entry) = inner.tasks.get_mut(&id) {
                if entry.terminal.is_none() {
                    entry.terminal = Some(match result {
                        Ok(result) => TaskPayload::Completed {
                            result: to_object(&result),
                        },
                        Err(TaskExit::Cancelled) => TaskPayload::Cancelled,
                        Err(TaskExit::Error(error)) => TaskPayload::Failed {
                            error: to_object(&error),
                        },
                    });
                    entry.terminal_at = Some(Instant::now());
                    entry.pending_inputs.clear();
                    entry.touch();
                    entry.task.status = entry.current_status();
                }
                entry.join_handle = None;
            }
        });
        match lock(&self.inner).tasks.get_mut(&task_id) {
            Some(entry) if entry.terminal.is_none() => entry.join_handle = Some(handle),
            Some(_) => {}
            None => handle.abort(),
        }
        task
    }

    pub fn get_task(&self, task_id: &str) -> Result<DetailedTask, ErrorData> {
        let mut inner = lock(&self.inner);
        Self::sweep_expired(&mut inner);
        let entry = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| unknown_task(task_id))?;
        entry.task.status = entry.current_status();
        Ok(entry.detailed())
    }

    pub fn update_task(
        &self,
        task_id: &str,
        input_responses: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<(), ErrorData> {
        let mut inner = lock(&self.inner);
        Self::sweep_expired(&mut inner);
        let entry = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| unknown_task(task_id))?;
        for (key, value) in input_responses {
            if let Some((_, tx)) = entry.pending_inputs.remove(&key) {
                let _ = tx.send(value);
            }
        }
        entry.touch();
        entry.task.status = entry.current_status();
        Ok(())
    }

    /// Cooperative: the operation decides when (and whether) to stop.
    pub fn cancel_task(&self, task_id: &str) -> Result<(), ErrorData> {
        let mut inner = lock(&self.inner);
        Self::sweep_expired(&mut inner);
        let entry = inner
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| unknown_task(task_id))?;
        entry.cancel_requested = true;
        let _ = entry.cancel_signal.send(true);
        if entry.terminal.is_none() {
            entry.pending_inputs.clear();
            entry.touch();
            entry.task.status = entry.current_status();
        }
        Ok(())
    }

    pub fn running_task_count(&self) -> usize {
        let mut inner = lock(&self.inner);
        Self::sweep_expired(&mut inner);
        inner
            .tasks
            .values()
            .filter(|e| e.terminal.is_none())
            .count()
    }

    fn sweep_expired(inner: &mut Inner) {
        for entry in inner.tasks.values_mut() {
            let expired = entry.terminal.is_none()
                && entry
                    .task
                    .ttl_ms
                    .is_some_and(|ttl| entry.created.elapsed().as_millis() >= u128::from(ttl));
            if expired {
                if let Some(handle) = entry.join_handle.take() {
                    handle.abort();
                }
                entry.terminal = Some(TaskPayload::Failed {
                    error: to_object(&ErrorData::internal_error(
                        "task expired: TTL elapsed before completion",
                        None,
                    )),
                });
                entry.terminal_at = Some(Instant::now());
                entry.pending_inputs.clear();
                entry.touch();
                entry.task.status = TaskStatus::Failed;
            }
        }
        inner
            .tasks
            .retain(|_, entry| match (entry.task.ttl_ms, entry.terminal_at) {
                (Some(ttl), Some(at)) => at.elapsed().as_millis() < u128::from(ttl),
                _ => true,
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let ts = current_timestamp();
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
        assert!(ts.ends_with("+00:00"), "{ts}");
    }

    #[test]
    fn uuids_are_version_4() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, uuid_v4());
    }

    #[tokio::test]
    async fn unknown_task_is_invalid_params() {
        let manager = TaskManager::new();
        for err in [
            manager.get_task("nope").unwrap_err(),
            manager.cancel_task("nope").unwrap_err(),
            manager.update_task("nope", []).unwrap_err(),
        ] {
            assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
        }
    }

    #[tokio::test]
    async fn cancel_settles_to_cancelled_when_the_operation_honors_it() {
        let manager = TaskManager::new();
        let task = manager.spawn(TaskOptions::default(), |ctx| {
            Box::pin(async move {
                ctx.cancelled().await;
                Err(TaskExit::Cancelled)
            })
        });
        manager.cancel_task(&task.task_id).unwrap();
        for _ in 0..200 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            if manager
                .get_task(&task.task_id)
                .unwrap()
                .status()
                .is_terminal()
            {
                assert_eq!(
                    manager.get_task(&task.task_id).unwrap().status(),
                    TaskStatus::Cancelled
                );
                return;
            }
        }
        panic!("task did not settle after cancel");
    }
}
