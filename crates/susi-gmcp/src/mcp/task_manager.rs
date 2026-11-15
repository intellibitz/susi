//! Long-running tool calls as pollable, durable, principal-scoped tasks.

use super::model::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    future::Future,
    io::Write,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, oneshot, watch};

pub const DEFAULT_TASK_TTL_MS: u64 = 300_000;
pub const DEFAULT_POLL_INTERVAL_MS: u64 = 1_000;
pub const MAX_TASK_TTL_MS: u64 = 86_400_000;
pub const MAX_ACTIVE_TASKS_PER_PRINCIPAL: usize = 64;
pub const MAX_GLOBAL_ACTIVE_TASKS: usize = 512;
pub const MAX_RETAINED_TERMINAL_TASKS: usize = 2048;
pub const MAX_INPUT_REQUESTS_PER_TASK: usize = 128;
pub const MAX_SUBSCRIPTION_TASK_IDS: usize = 64;
pub const MAX_INPUT_RESPONSE_BYTES: usize = 1_048_576;
const POLL_BURST_ALLOWANCE: f64 = 24.0;
const STORE_FILE: &str = "tasks.json";
const STORE_VERSION: u32 = 1;

/// Authenticated caller identity bound to every task (`T3`/`T6`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TaskPrincipal(pub String);

impl TaskPrincipal {
    pub fn local() -> Self {
        Self("local:stdio".into())
    }
    /// Hash bearer tokens so plaintext secrets never enter maps or disk.
    pub fn from_bearer_token(token: &str) -> Self {
        let mut h = String::with_capacity(32);
        for b in &Sha256::digest(token.as_bytes())[..16] {
            h.push_str(&format!("{b:02x}"));
        }
        Self(format!("bearer:{h}"))
    }
    pub fn from_node(node_id: &str) -> Self {
        Self(format!("node:{node_id}"))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn current_unix_ms() -> u64 {
    let d = SystemTime::now().duration_since(UNIX_EPOCH);
    d.unwrap_or_default().as_millis() as u64
}

/// RFC 3339 UTC timestamp with second fractions as needed (0, 3, 6 or 9 digits).
pub fn current_timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let (secs, ns) = (now.as_secs() as i64, now.subsec_nanos());
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let mon = if mp < 10 { mp + 3 } else { mp - 9 };
    let yr = yoe + era * 400 + i64::from(mon <= 2);
    let day = doy - (153 * mp + 2) / 5 + 1;
    let frac = match ns {
        0 => String::new(),
        n if n.is_multiple_of(1_000_000) => format!(".{:03}", n / 1_000_000),
        n if n.is_multiple_of(1_000) => format!(".{:06}", n / 1_000),
        n => format!(".{n:09}"),
    };
    format!(
        "{yr:04}-{mon:02}-{day:02}T{:02}:{:02}:{:02}{frac}+00:00",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// A random (version 4) UUID from the operating system's random source.
pub fn uuid_v4() -> String {
    let hex = crate::susi_config::cluster_key::random_nonce_hex();
    let raw = u128::from_str_radix(hex.get(..32).unwrap_or_default(), 16).unwrap_or(0);
    let v = (raw & !(0xf << 76) & !(0x3 << 62)) | (0x4 << 76) | (0x2 << 62);
    let (a, b, c) = ((v >> 96) as u32, (v >> 80) as u16, (v >> 64) as u16);
    format!(
        "{a:08x}-{b:04x}-{c:04x}-{:04x}-{:012x}",
        (v >> 48) as u16,
        (v as u64) & 0xffff_ffff_ffff
    )
}

#[derive(Debug)]
pub enum TaskExit {
    Cancelled,
    Error(ErrorData),
}
impl From<ErrorData> for TaskExit {
    fn from(e: ErrorData) -> Self {
        Self::Error(e)
    }
}
pub type TaskFuture = Pin<Box<dyn Future<Output = Result<CallToolResult, TaskExit>> + Send>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Stored(
    TaskPrincipal,
    Task,
    TaskPayload,
    HashSet<String>,
    u64,
    Option<u64>,
);

#[derive(Debug, Serialize, Deserialize)]
struct StoredState(u32, Vec<Stored>);

struct TaskEntry {
    owner: TaskPrincipal,
    task: Task,
    terminal: Option<(TaskPayload, Option<Instant>, Option<u64>)>,
    pending: HashMap<String, (InputRequest, oneshot::Sender<serde_json::Value>)>,
    used_keys: HashSet<String>,
    cancel: (bool, watch::Sender<bool>),
    created: (Option<Instant>, u64),
    poll: (f64, Instant),
    join: Option<tokio::task::JoinHandle<()>>,
}

impl TaskEntry {
    fn working(owner: TaskPrincipal, task: Task, created_unix_ms: u64) -> Self {
        let now = Instant::now();
        Self {
            owner,
            task,
            terminal: None,
            pending: HashMap::new(),
            used_keys: HashSet::new(),
            cancel: (false, watch::channel(false).0),
            created: (Some(now), created_unix_ms),
            poll: (POLL_BURST_ALLOWANCE, now),
            join: None,
        }
    }
    fn status(&self) -> TaskStatus {
        match &self.terminal {
            Some((p, _, _)) => p.status(),
            None if !self.pending.is_empty() => TaskStatus::InputRequired,
            None => TaskStatus::Working,
        }
    }
    fn touch(&mut self) {
        self.task.last_updated_at = current_timestamp();
        self.task.status = self.status();
    }
    fn settle(&mut self, payload: TaskPayload, at: Option<Instant>, ts_ms: u64) {
        self.terminal = Some((payload, at, Some(ts_ms)));
        self.pending.clear();
        self.touch();
    }
    fn settle_res(&mut self, res: Result<CallToolResult, TaskExit>) {
        if self.terminal.is_none() {
            let p = if self.cancel.0 {
                TaskPayload::Cancelled
            } else {
                match res {
                    Ok(r) => TaskPayload::Completed { result: to_obj(&r) },
                    Err(TaskExit::Cancelled) => TaskPayload::Cancelled,
                    Err(TaskExit::Error(e)) => TaskPayload::Failed { error: to_obj(&e) },
                }
            };
            self.settle(p, Some(Instant::now()), current_unix_ms());
        }
        self.join = None;
    }
    fn detailed(&self) -> DetailedTask {
        let p = if let Some((payload, _, _)) = &self.terminal {
            payload.clone()
        } else if self.pending.is_empty() {
            TaskPayload::Working
        } else {
            let input_requests = self
                .pending
                .iter()
                .map(|(k, (r, _))| (k.clone(), r.clone()))
                .collect();
            TaskPayload::InputRequired { input_requests }
        };
        DetailedTask::new(self.task.clone(), p)
    }
    fn persisted(&self) -> Stored {
        let d = self.detailed();
        let term = self.terminal.as_ref().and_then(|t| t.2);
        Stored(
            self.owner.clone(),
            d.task,
            d.payload,
            self.used_keys.clone(),
            self.created.1,
            term,
        )
    }
}

struct Inner {
    tasks: HashMap<String, TaskEntry>,
    store_dir: Option<PathBuf>,
    events: broadcast::Sender<(TaskPrincipal, DetailedTask)>,
}
impl Default for Inner {
    fn default() -> Self {
        Self {
            tasks: HashMap::new(),
            store_dir: None,
            events: broadcast::channel(256).0,
        }
    }
}

impl Inner {
    fn persist(&self) -> Result<(), ErrorData> {
        let Some(dir) = &self.store_dir else {
            return Ok(());
        };
        let tasks = self.tasks.values().map(TaskEntry::persisted).collect();
        let bytes = serde_json::to_vec(&StoredState(STORE_VERSION, tasks)).map_err(err_int)?;
        let tmp = dir.join(format!(
            "{STORE_FILE}.tmp.{}.{}",
            std::process::id(),
            uuid_v4()
        ));
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(err_int)?;
        let res = f.write_all(&bytes).and_then(|()| f.sync_all());
        drop(f);
        res.and_then(|()| fs::rename(&tmp, dir.join(STORE_FILE)))
            .map_err(|e| {
                let _ = fs::remove_file(&tmp);
                err_int(e)
            })
    }
    fn emit(&self, id: &str) {
        if let Some(e) = self.tasks.get(id) {
            let _ = self.events.send((e.owner.clone(), e.detailed()));
        }
    }
    fn sync(&self, id: &str) -> Result<(), ErrorData> {
        self.persist()?;
        self.emit(id);
        Ok(())
    }
    fn owned_mut(&mut self, id: &str, owner: &TaskPrincipal) -> Result<&mut TaskEntry, ErrorData> {
        self.sweep_expired();
        self.tasks
            .get_mut(id)
            .filter(|e| e.owner == *owner)
            .ok_or_else(|| unknown(id))
    }
    fn mutate_active(
        &mut self,
        id: &str,
        owner: &TaskPrincipal,
        err: &'static str,
        f: impl FnOnce(&mut TaskEntry),
    ) -> Result<(), ErrorData> {
        let e = self.owned_mut(id, owner)?;
        if e.status().is_terminal() {
            return Err(bad(err));
        }
        f(e);
        e.touch();
        self.sync(id)
    }
    fn sweep_expired(&mut self) {
        let now_ms = current_unix_ms();
        let mut expired = Vec::new();
        for (id, e) in &mut self.tasks {
            if e.terminal.is_none() && is_expired(e, now_ms) {
                if let Some(h) = e.join.take() {
                    h.abort();
                }
                e.settle(
                    fail_payload("task expired: TTL elapsed"),
                    Some(Instant::now()),
                    now_ms,
                );
                expired.push(id.clone());
            }
        }
        for id in &expired {
            self.emit(id);
        }
        self.tasks
            .retain(|_, e| e.terminal.is_none() || retained(e, now_ms));
        let mut terms: Vec<_> = self
            .tasks
            .iter()
            .filter_map(|(id, e)| {
                e.terminal
                    .as_ref()
                    .map(|t| (t.2.unwrap_or(e.created.1), id.clone()))
            })
            .collect();
        let excess = terms.len().saturating_sub(MAX_RETAINED_TERMINAL_TASKS);
        if excess > 0 {
            terms.sort_by_key(|(ts, _)| *ts);
            for (_, id) in terms.into_iter().take(excess) {
                self.tasks.remove(&id);
            }
        }
        if !expired.is_empty() || excess > 0 {
            let _ = self.persist();
        }
    }
}

fn lock(m: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}
fn err_int(m: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(m.to_string(), None)
}
fn bad(m: &'static str) -> ErrorData {
    ErrorData::invalid_params(m, None)
}
fn busy(m: &'static str, d: Option<serde_json::Value>) -> ErrorData {
    ErrorData::new(ErrorCode(-32000), m, d)
}
fn unknown(id: &str) -> ErrorData {
    ErrorData::invalid_params(format!("unknown task: {id}"), None)
}
fn to_obj<T: Serialize>(v: &T) -> JsonObject {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}
fn fail_payload(msg: &str) -> TaskPayload {
    TaskPayload::Failed {
        error: to_obj(&err_int(msg)),
    }
}
fn is_expired(e: &TaskEntry, now_ms: u64) -> bool {
    e.task.ttl_ms.is_some_and(|ttl| {
        e.created.0.map_or_else(
            || now_ms.saturating_sub(e.created.1) >= ttl,
            |c| c.elapsed().as_millis() >= u128::from(ttl),
        )
    })
}
fn retained(e: &TaskEntry, now_ms: u64) -> bool {
    match (e.task.ttl_ms, e.terminal.as_ref()) {
        (Some(ttl), Some((_, Some(at), _))) => at.elapsed().as_millis() < u128::from(ttl),
        (Some(ttl), Some((_, None, Some(ts)))) => now_ms.saturating_sub(*ts) < ttl,
        _ => true,
    }
}
fn make_task(id: String, msg: Option<String>, opts: &TaskOptions) -> Task {
    let now = current_timestamp();
    Task {
        task_id: id,
        status: TaskStatus::Working,
        status_message: msg,
        created_at: now.clone(),
        last_updated_at: now,
        ttl_ms: opts.ttl_ms.map(|t| t.min(MAX_TASK_TTL_MS)),
        poll_interval_ms: opts.poll_interval_ms,
    }
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
    pub async fn request_input(
        &self,
        key: impl Into<String>,
        req: InputRequest,
    ) -> Result<serde_json::Value, TaskExit> {
        let key = key.into();
        let (tx, rx) = oneshot::channel();
        {
            let mut g = lock(&self.inner);
            let e = g
                .tasks
                .get_mut(&self.task_id)
                .ok_or_else(|| TaskExit::Error(err_int("task missing")))?;
            if e.used_keys.len() >= MAX_INPUT_REQUESTS_PER_TASK {
                return Err(TaskExit::Error(bad("max inputRequests per task exceeded")));
            }
            if !e.used_keys.insert(key.clone()) {
                return Err(TaskExit::Error(err_int(format!(
                    "inputRequests key {key:?} already used"
                ))));
            }
            e.pending.insert(key, (req, tx));
            e.touch();
            let _ = g.sync(&self.task_id);
        }
        rx.await.map_err(|_| TaskExit::Cancelled)
    }
    pub fn set_status_message(&self, message: impl Into<String>) {
        let mut g = lock(&self.inner);
        if let Some(e) = g.tasks.get_mut(&self.task_id) {
            e.task.status_message = Some(message.into());
            e.touch();
            let _ = g.sync(&self.task_id);
        }
    }
    pub fn is_cancel_requested(&self) -> bool {
        lock(&self.inner)
            .tasks
            .get(&self.task_id)
            .is_some_and(|e| e.cancel.0)
    }
    pub async fn cancelled(&self) {
        let mut rx = match lock(&self.inner).tasks.get(&self.task_id) {
            Some(e) if !e.cancel.0 => e.cancel.1.subscribe(),
            _ => return,
        };
        while !*rx.borrow_and_update() && rx.changed().await.is_ok() {}
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

impl TaskManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a disk-backed durable store (fails closed if unreadable or corrupt).
    pub fn open(store_dir: impl Into<PathBuf>) -> Result<Self, ErrorData> {
        let dir = store_dir.into();
        fs::create_dir_all(&dir).map_err(err_int)?;
        let (target, now_ms, mut tasks) = (dir.join(STORE_FILE), current_unix_ms(), HashMap::new());
        if target.exists() {
            let StoredState(ver, items) =
                serde_json::from_slice(&fs::read(&target).map_err(err_int)?).map_err(err_int)?;
            if ver != STORE_VERSION {
                return Err(err_int(format!("unsupported task store version {ver}")));
            }
            for Stored(owner, task, payload, used, created_ms, term_ms) in items {
                let mut e = TaskEntry::working(owner, task, created_ms);
                (e.created.0, e.used_keys, e.terminal) =
                    (None, used, Some((payload, None, term_ms)));
                if !e.status().is_terminal() {
                    e.settle(
                        fail_payload("task interrupted by server restart"),
                        None,
                        now_ms,
                    );
                }
                if retained(&e, now_ms) {
                    tasks.insert(e.task.task_id.clone(), e);
                }
            }
        }
        let mut inner = Inner {
            tasks,
            store_dir: Some(dir),
            events: broadcast::channel(256).0,
        };
        inner.sweep_expired();
        inner.persist()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<(TaskPrincipal, DetailedTask)> {
        lock(&self.inner).events.subscribe()
    }

    pub fn try_spawn_for<F>(
        &self,
        owner: TaskPrincipal,
        opts: TaskOptions,
        make_future: F,
    ) -> Result<Task, ErrorData>
    where
        F: FnOnce(TaskContext) -> TaskFuture,
    {
        let (id, now_ms) = (uuid_v4(), current_unix_ms());
        let task = make_task(id.clone(), opts.status_message.clone(), &opts);
        {
            let mut g = lock(&self.inner);
            g.sweep_expired();
            let active = g.tasks.values().filter(|e| e.terminal.is_none());
            let owned = active.clone().filter(|e| e.owner == owner).count();
            if owned >= MAX_ACTIVE_TASKS_PER_PRINCIPAL || active.count() >= MAX_GLOBAL_ACTIVE_TASKS
            {
                return Err(busy("active task limit exceeded", None));
            }
            g.tasks
                .insert(id.clone(), TaskEntry::working(owner, task.clone(), now_ms));
            if let Err(e) = g.persist() {
                g.tasks.remove(&id);
                return Err(e);
            }
            g.emit(&id);
        }
        let fut = make_future(TaskContext {
            task_id: id.clone(),
            inner: self.inner.clone(),
        });
        let (arc, tid) = (self.inner.clone(), id.clone());
        let handle = tokio::spawn(async move {
            let res = fut.await;
            let mut g = lock(&arc);
            if let Some(e) = g.tasks.get_mut(&tid) {
                e.settle_res(res);
                let _ = g.sync(&tid);
            }
        });
        match lock(&self.inner).tasks.get_mut(&id) {
            Some(e) if e.terminal.is_none() => e.join = Some(handle),
            Some(_) => {}
            None => handle.abort(),
        }
        Ok(task)
    }

    pub fn spawn<F>(&self, opts: TaskOptions, make_future: F) -> Task
    where
        F: FnOnce(TaskContext) -> TaskFuture,
    {
        let fallback = opts.clone();
        self.try_spawn_for(TaskPrincipal::local(), opts, make_future)
            .unwrap_or_else(|_| {
                let mut t = make_task(
                    uuid_v4(),
                    Some("active task limit exceeded".into()),
                    &fallback,
                );
                t.status = TaskStatus::Failed;
                t
            })
    }

    pub fn get_task_for(&self, id: &str, owner: &TaskPrincipal) -> Result<DetailedTask, ErrorData> {
        let mut g = lock(&self.inner);
        let e = g.owned_mut(id, owner)?;
        let (now, iv) = (
            Instant::now(),
            e.task
                .poll_interval_ms
                .unwrap_or(DEFAULT_POLL_INTERVAL_MS)
                .max(1),
        );
        let dt = now.duration_since(e.poll.1).as_secs_f64();
        e.poll = (
            (e.poll.0 + dt * 1_000.0 / iv as f64).min(POLL_BURST_ALLOWANCE),
            now,
        );
        if e.poll.0 < 1.0 {
            return Err(busy(
                "tasks/get rate limit exceeded; respect pollIntervalMs",
                Some(serde_json::json!({ "pollIntervalMs": iv })),
            ));
        }
        e.poll.0 -= 1.0;
        e.task.status = e.status();
        Ok(e.detailed())
    }
    pub fn get_task(&self, id: &str) -> Result<DetailedTask, ErrorData> {
        self.get_task_for(id, &TaskPrincipal::local())
    }

    pub fn update_task_for(
        &self,
        id: &str,
        owner: &TaskPrincipal,
        inputs: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<(), ErrorData> {
        let updates: Vec<_> = inputs.into_iter().collect();
        let bytes = updates.iter().try_fold(0usize, |acc, (k, v)| {
            serde_json::to_vec(v)
                .ok()
                .and_then(|b| acc.checked_add(k.len())?.checked_add(b.len()))
        });
        if updates.len() > MAX_INPUT_REQUESTS_PER_TASK
            || bytes.is_none_or(|b| b > MAX_INPUT_RESPONSE_BYTES)
        {
            return Err(bad("inputResponses exceeds size or key count limit"));
        }
        lock(&self.inner).mutate_active(id, owner, "cannot update a terminal task", |e| {
            for (k, v) in updates {
                if let Some((_, tx)) = e.pending.remove(&k) {
                    let _ = tx.send(v);
                }
            }
        })
    }
    pub fn update_task(
        &self,
        id: &str,
        inputs: impl IntoIterator<Item = (String, serde_json::Value)>,
    ) -> Result<(), ErrorData> {
        self.update_task_for(id, &TaskPrincipal::local(), inputs)
    }

    pub fn cancel_task_for(&self, id: &str, owner: &TaskPrincipal) -> Result<(), ErrorData> {
        lock(&self.inner).mutate_active(id, owner, "cannot cancel a terminal task", |e| {
            e.cancel.0 = true;
            let _ = e.cancel.1.send(true);
            e.pending.clear();
        })
    }
    pub fn cancel_task(&self, id: &str) -> Result<(), ErrorData> {
        self.cancel_task_for(id, &TaskPrincipal::local())
    }

    pub fn authorize_tasks_for(
        &self,
        owner: &TaskPrincipal,
        ids: &[String],
    ) -> Result<Vec<String>, ErrorData> {
        if ids.len() > MAX_SUBSCRIPTION_TASK_IDS {
            return Err(bad("too many taskIds in subscription filter"));
        }
        let mut g = lock(&self.inner);
        g.sweep_expired();
        let (mut seen, mut out) = (HashSet::new(), Vec::new());
        for id in ids {
            if !g.tasks.get(id).is_some_and(|e| e.owner == *owner) {
                return Err(unknown(id));
            }
            if seen.insert(id.clone()) {
                out.push(id.clone());
            }
        }
        Ok(out)
    }

    pub fn snapshots_for(&self, owner: &TaskPrincipal, ids: &[String]) -> Vec<DetailedTask> {
        let mut g = lock(&self.inner);
        g.sweep_expired();
        ids.iter()
            .filter_map(|id| g.tasks.get(id).filter(|e| e.owner == *owner))
            .map(TaskEntry::detailed)
            .collect()
    }

    pub fn running_task_count(&self) -> usize {
        let mut g = lock(&self.inner);
        g.sweep_expired();
        g.tasks.values().filter(|e| e.terminal.is_none()).count()
    }
}
