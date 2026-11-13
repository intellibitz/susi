//! The A2A request handler: runs an [`AgentExecutor`] for `message/send`
//! and `message/stream`, keeps task snapshots in the [`TaskStore`], answers
//! `tasks/*` and push-config requests, and delivers push notifications.
//!
//! An executor writes its outcome as [`Task`] events to the task's
//! [`EventQueue`]; `message/send` returns the first task event (or, with
//! `returnImmediately: false`, waits for a terminal or auth-required one),
//! persisting each event and pushing it to the task's configured endpoints.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::broadcast;

use super::error::{A2AError, Result};
use super::types::{
    AgentCard, CancelTaskRequest, DeleteTaskPushNotificationConfigRequest,
    GetTaskPushNotificationConfigRequest, GetTaskRequest, ListTaskPushNotificationConfigsRequest,
    ListTaskPushNotificationConfigsResponse, ListTasksRequest, ListTasksResponse, Message,
    SendMessageRequest, SubscribeToTaskRequest, Task, TaskPushNotificationConfig, TaskState,
    TaskStatus,
};
use crate::task_store::TaskStore;

/// Capacity of a task's event channel.
const QUEUE_CAPACITY: usize = 100;

/// A push notification POST's whole-request budget.
const PUSH_TIMEOUT_SECS: u64 = 30;
const PUSH_MAX_REDIRECTS: u32 = 10;

/// The agent logic behind the protocol.
pub trait AgentExecutor: Send + Sync {
    /// Handle the message in `ctx`, writing the task's progress to `queue`.
    fn execute<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

    /// Cancel the task in `ctx`, writing its final state to `queue`.
    fn cancel<'a>(
        &'a self,
        ctx: &'a RequestContext,
        queue: &'a EventQueue,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

/// What an executor is asked to act on.
#[derive(Debug, Clone)]
pub struct RequestContext {
    pub task_id: String,
    pub context_id: String,
    /// The triggering message; `None` for a cancellation.
    pub message: Option<Message>,
}

impl RequestContext {
    pub fn new(task_id: impl Into<String>, context_id: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            context_id: context_id.into(),
            message: None,
        }
    }
}

/// One task's event channel: every subscriber sees every task event.
#[derive(Debug)]
pub struct EventQueue {
    sender: broadcast::Sender<Task>,
}

impl EventQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            sender: broadcast::channel(capacity).0,
        }
    }

    /// Publish a task event; fails when nobody is subscribed.
    pub fn send(&self, task: Task) -> Result<()> {
        self.sender
            .send(task)
            .map(|_| ())
            .map_err(|e| A2AError::Other(format!("Failed to send event: {e}")))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Task> {
        self.sender.subscribe()
    }
}

/// A push-notification endpoint as stored for a task.
#[derive(Debug, Clone)]
struct PushConfig {
    id: String,
    url: String,
    token: Option<String>,
    authentication: Option<super::types::AuthenticationInfo>,
}

impl PushConfig {
    fn reply(&self, tenant: Option<String>, task_id: Option<String>) -> TaskPushNotificationConfig {
        TaskPushNotificationConfig {
            tenant,
            id: Some(self.id.clone()),
            task_id,
            url: self.url.clone(),
            token: self.token.clone(),
            authentication: self.authentication.clone(),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

type PushConfigMap = HashMap<String, BTreeMap<String, PushConfig>>;

/// Serves the A2A methods for one executor and agent card.
pub struct Handler {
    executor: Arc<dyn AgentExecutor>,
    card: AgentCard,
    store: Arc<TaskStore>,
    queues: Mutex<HashMap<String, Arc<EventQueue>>>,
    running: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// Push configs by task id, then config id.
    push_configs: Arc<Mutex<PushConfigMap>>,
}

impl Handler {
    pub fn new(executor: Arc<dyn AgentExecutor>, card: AgentCard, store: Arc<TaskStore>) -> Self {
        let push_configs = Arc::new(Mutex::new(HashMap::new()));
        let weak_configs = Arc::downgrade(&push_configs);
        store.on_evict(move |evicted| {
            let Some(configs) = weak_configs.upgrade() else {
                return false;
            };
            let mut guard = lock(&configs);
            for task_id in evicted {
                guard.remove(task_id);
            }
            true
        });
        Self {
            executor,
            card,
            store,
            queues: Mutex::new(HashMap::new()),
            running: Arc::new(Mutex::new(HashMap::new())),
            push_configs,
        }
    }

    #[cfg(test)]
    pub(crate) fn has_push_configs(&self, task_id: &str) -> bool {
        lock(&self.push_configs)
            .get(task_id)
            .is_some_and(|m| !m.is_empty())
    }

    fn queue(&self, task_id: &str) -> Arc<EventQueue> {
        Arc::clone(
            lock(&self.queues)
                .entry(task_id.to_string())
                .or_insert_with(|| Arc::new(EventQueue::new(QUEUE_CAPACITY))),
        )
    }

    fn existing_queue(&self, task_id: &str) -> Option<Arc<EventQueue>> {
        lock(&self.queues).get(task_id).cloned()
    }

    fn remove_queue(&self, task_id: &str) {
        lock(&self.queues).remove(task_id);
    }

    fn task(&self, task_id: &str) -> Result<Task> {
        self.store
            .get(task_id)
            .ok_or_else(|| A2AError::TaskNotFound(task_id.to_string()))
    }

    fn terminal_state(task_id: &str, state: TaskState) -> A2AError {
        A2AError::InvalidParams(format!("Task {task_id} is in terminal state: {state:?}"))
    }

    /// Resolve the task and context ids a message continues or starts.
    fn request_context(&self, params: &SendMessageRequest) -> Result<RequestContext> {
        let message = &params.message;
        if message.id.is_empty() {
            return Err(A2AError::InvalidParams("message ID is required".into()));
        }
        if message.parts.is_empty() {
            return Err(A2AError::InvalidParams("message parts is required".into()));
        }
        let (task_id, context_id, stored) = match (&message.task_id, &message.context_id) {
            (Some(task_id), Some(context_id)) => {
                (task_id.clone(), context_id.clone(), self.store.get(task_id))
            }
            (Some(task_id), None) => {
                let stored = self.task(task_id)?;
                (task_id.clone(), stored.context_id.clone(), Some(stored))
            }
            (None, Some(context_id)) => (super::codec::uuid_v7(), context_id.clone(), None),
            (None, None) => (super::codec::uuid_v7(), super::codec::uuid_v7(), None),
        };
        if let Some(stored) = stored.filter(|t| t.status.state.is_terminal()) {
            return Err(Self::terminal_state(&task_id, stored.status.state));
        }
        if let Some(config) = params
            .configuration
            .as_ref()
            .and_then(|c| c.task_push_notification_config.as_ref())
        {
            if let Err(e) = self.save_push_config(&task_id, config) {
                eprintln!("[A2A] Failed to save push config: {e}");
            }
        }
        let mut ctx = RequestContext::new(task_id, context_id);
        ctx.message = Some(message.clone());
        Ok(ctx)
    }

    /// Run the executor in the background; an executor error becomes a
    /// failed task carrying the error text.
    fn spawn_execution(&self, ctx: RequestContext, queue: Arc<EventQueue>) {
        let executor = Arc::clone(&self.executor);
        let store = Arc::clone(&self.store);
        let running = Arc::clone(&self.running);
        let task_id = ctx.task_id.clone();
        let mut handles = lock(&self.running);
        let handle = tokio::spawn(async move {
            if let Err(e) = executor.execute(&ctx, &queue).await {
                eprintln!("[A2A] Agent execution failed for {}: {e}", ctx.task_id);
                let mut task = Task::new(&ctx.task_id, &ctx.context_id);
                task.status = TaskStatus::failed(e.to_string());
                store.save(&task);
                let _ = queue.send(task);
            }
            lock(&running).remove(&ctx.task_id);
        });
        handles.insert(task_id, handle);
    }

    /// Persist a task event and push it to the task's endpoints.
    async fn record(&self, task: &Task) {
        self.store.save(task);
        let configs: Vec<PushConfig> = lock(&self.push_configs)
            .get(&task.id)
            .map(|c| c.values().cloned().collect())
            .unwrap_or_default();
        for config in configs {
            if let Err(e) = push(&config, task).await {
                eprintln!(
                    "[A2A] Push notification delivery failed for {}: {e}",
                    task.id
                );
            }
        }
    }

    /// Wait for the task event that answers a non-streaming request: the
    /// first one when `first`, else a terminal or auth-required one (or the
    /// last seen if the queue closes).
    async fn collect(&self, mut rx: broadcast::Receiver<Task>, first: bool) -> Result<Task> {
        let mut last = None;
        loop {
            match rx.recv().await {
                Ok(task) => {
                    self.record(&task).await;
                    if first || task.status.state == TaskState::AuthRequired {
                        return Ok(self.store.get(&task.id).unwrap_or(task));
                    }
                    if task.status.state.is_terminal() {
                        return Ok(task);
                    }
                    last = Some(task);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return last
                        .ok_or_else(|| A2AError::Other("Event queue closed unexpectedly".into()));
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[A2A] Event stream lagged by {n} messages");
                }
            }
        }
    }

    /// `message/send`.
    pub async fn send(&self, params: SendMessageRequest) -> Result<Task> {
        let ctx = self.request_context(&params)?;
        let task_id = ctx.task_id.clone();
        let queue = self.queue(&task_id);
        let rx = queue.subscribe();
        self.spawn_execution(ctx, queue);
        let first = params
            .configuration
            .as_ref()
            .is_none_or(|c| c.return_immediately);
        let result = self.collect(rx, first).await;
        self.remove_queue(&task_id);
        result
    }

    /// `message/stream`: the task's events as they happen. Streamed events
    /// are not recorded in the store.
    pub fn stream(&self, params: SendMessageRequest) -> Result<TaskEvents> {
        let ctx = self.request_context(&params)?;
        let task_id = ctx.task_id.clone();
        let queue = self.queue(&task_id);
        let rx = queue.subscribe();
        self.spawn_execution(ctx, queue);
        Ok(TaskEvents {
            task_id,
            rx,
            owns_queue: true,
        })
    }

    /// A finished stream: a `message/stream` queue goes with its stream (a
    /// resubscription leaves the running execution's queue in place).
    pub fn release(&self, events: TaskEvents) {
        if events.owns_queue {
            self.remove_queue(&events.task_id);
        }
    }

    /// `tasks/resubscribe`: a running task's further events.
    pub fn resubscribe(&self, params: SubscribeToTaskRequest) -> Result<TaskEvents> {
        let task = self.task(&params.id)?;
        if task.status.state.is_terminal() {
            return Err(Self::terminal_state(&params.id, task.status.state));
        }
        let queue = self
            .existing_queue(&params.id)
            .ok_or_else(|| A2AError::TaskNotFound(params.id.clone()))?;
        Ok(TaskEvents {
            task_id: params.id,
            rx: queue.subscribe(),
            owns_queue: false,
        })
    }

    /// `tasks/get`.
    pub fn get(&self, params: GetTaskRequest) -> Result<Task> {
        self.task(&params.id)
    }

    /// `tasks/list`.
    pub fn list(&self, params: ListTasksRequest) -> ListTasksResponse {
        self.store.list(&params)
    }

    /// `tasks/cancel`: stop the running execution and let the executor
    /// write the canceled state.
    pub async fn cancel(&self, params: CancelTaskRequest) -> Result<Task> {
        let task = self.task(&params.id)?;
        if task.status.state.is_terminal() {
            return Err(A2AError::TaskNotCancelable(params.id));
        }
        if let Some(handle) = lock(&self.running).remove(&params.id) {
            handle.abort();
        }
        let queue = self.queue(&params.id);
        let rx = queue.subscribe();
        let executor = Arc::clone(&self.executor);
        let store = Arc::clone(&self.store);
        let ctx = RequestContext::new(task.id, task.context_id);
        tokio::spawn(async move {
            if let Err(e) = executor.cancel(&ctx, &queue).await {
                eprintln!("[A2A] Cancel failed for {}: {e}", ctx.task_id);
                let mut task = Task::new(&ctx.task_id, &ctx.context_id);
                task.status = TaskStatus::failed(e.to_string());
                store.save(&task);
                let _ = queue.send(task);
            }
        });
        let result = self.collect(rx, false).await;
        self.remove_queue(&params.id);
        result
    }

    /// Push configs need the capability on the card and an existing task.
    fn require_push(&self, task_id: &str) -> Result<()> {
        if !self.card.capabilities.push_notifications.unwrap_or(false) {
            return Err(A2AError::PushNotificationNotSupported);
        }
        self.task(task_id).map(|_| ())
    }

    fn save_push_config(
        &self,
        task_id: &str,
        config: &TaskPushNotificationConfig,
    ) -> Result<PushConfig> {
        if config.url.is_empty() {
            return Err(A2AError::InvalidParams(
                "push config URL cannot be empty".into(),
            ));
        }
        if !config.url.starts_with("http://") && !config.url.starts_with("https://") {
            return Err(A2AError::InvalidParams(
                "push config URL must be http or https".into(),
            ));
        }
        let saved = PushConfig {
            id: config
                .id
                .clone()
                .filter(|id| !id.is_empty())
                .unwrap_or_else(super::codec::uuid_v4),
            url: config.url.clone(),
            token: config.token.clone(),
            authentication: config.authentication.clone(),
        };
        lock(&self.push_configs)
            .entry(task_id.to_string())
            .or_default()
            .insert(saved.id.clone(), saved.clone());
        Ok(saved)
    }

    /// `tasks/pushNotificationConfig/create`.
    pub fn create_push_config(
        &self,
        params: TaskPushNotificationConfig,
    ) -> Result<TaskPushNotificationConfig> {
        let task_id = params.task_id.clone().unwrap_or_default();
        self.require_push(&task_id)?;
        let saved = self.save_push_config(&task_id, &params)?;
        Ok(saved.reply(params.tenant, params.task_id))
    }

    /// `tasks/pushNotificationConfig/get`.
    pub fn get_push_config(
        &self,
        params: GetTaskPushNotificationConfigRequest,
    ) -> Result<TaskPushNotificationConfig> {
        self.require_push(&params.task_id)?;
        let config = lock(&self.push_configs)
            .get(&params.task_id)
            .and_then(|c| c.get(&params.id))
            .cloned()
            .ok_or_else(|| A2AError::InvalidParams("push config not found".into()))?;
        Ok(config.reply(params.tenant, Some(params.task_id)))
    }

    /// `tasks/pushNotificationConfig/list`.
    pub fn list_push_configs(
        &self,
        params: ListTaskPushNotificationConfigsRequest,
    ) -> Result<ListTaskPushNotificationConfigsResponse> {
        self.require_push(&params.task_id)?;
        let configs = lock(&self.push_configs)
            .get(&params.task_id)
            .map(|c| {
                c.values()
                    .map(|config| config.reply(params.tenant.clone(), Some(params.task_id.clone())))
                    .collect()
            })
            .unwrap_or_default();
        Ok(ListTaskPushNotificationConfigsResponse {
            configs,
            next_page_token: None,
        })
    }

    /// `tasks/pushNotificationConfig/delete`.
    pub fn delete_push_config(
        &self,
        params: DeleteTaskPushNotificationConfigRequest,
    ) -> Result<()> {
        self.require_push(&params.task_id)?;
        if let Some(configs) = lock(&self.push_configs).get_mut(&params.task_id) {
            configs.remove(&params.id);
        }
        Ok(())
    }
}

/// A subscription to one task's events; ends after a terminal or
/// interrupted state, when the queue closes, or when dropped.
pub struct TaskEvents {
    task_id: String,
    rx: broadcast::Receiver<Task>,
    owns_queue: bool,
}

impl TaskEvents {
    /// The next event: `None` once the stream is over, `Err` when this
    /// subscriber fell behind and missed events.
    pub async fn next(&mut self) -> Option<Result<Task>> {
        match self.rx.recv().await {
            Ok(task) => Some(Ok(task)),
            Err(broadcast::error::RecvError::Closed) => None,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                Some(Err(A2AError::Other(format!("Lagged by {n} events"))))
            }
        }
    }
}

/// POST the task snapshot to a push endpoint, with its token and
/// credentials (`Bearer`/`Basic`), over the core HTTP transport. Runs on a
/// blocking thread: the transport is synchronous.
async fn push(config: &PushConfig, task: &Task) -> std::result::Result<(), String> {
    let body = serde_json::to_vec(task).map_err(|e| format!("failed to serialize task: {e}"))?;
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if let Some(token) = config.token.as_ref().filter(|t| !t.is_empty()) {
        headers.push(("X-A2A-Notification-Token".into(), token.clone()));
    }
    if let Some(auth) = &config.authentication {
        if let Some(credentials) = auth.credentials.as_ref().filter(|c| !c.is_empty()) {
            let scheme = match auth.scheme.to_lowercase().as_str() {
                "bearer" => Some("Bearer"),
                "basic" => Some("Basic"),
                _ => None,
            };
            if let Some(scheme) = scheme {
                headers.push(("Authorization".into(), format!("{scheme} {credentials}")));
            }
        }
    }
    let url = config.url.clone();
    let status = tokio::task::spawn_blocking(move || {
        let headers: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        susi_http_transport::http_call_with_body(
            "POST",
            &url,
            &headers,
            Some(&body),
            PUSH_TIMEOUT_SECS,
            PUSH_MAX_REDIRECTS,
        )
        .map(|reply| reply.status)
    })
    .await
    .map_err(|e| format!("failed to send push notification: {e}"))?
    .map_err(|e| format!("failed to send push notification: {e}"))?;
    if (200..300).contains(&status) {
        Ok(())
    } else {
        Err(format!(
            "push notification endpoint returned non-success status: {status}"
        ))
    }
}
