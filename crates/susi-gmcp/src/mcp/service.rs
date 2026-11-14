//! The MCP server session: lifecycle selection (`initialize` or per-request
//! metadata), request dispatch to a [`ServerHandler`], concurrent request
//! handling with cancellation, and the peer handle handlers use to notify
//! the client.
#![allow(deprecated)]

use super::message::*;
use super::model::*;
use super::transport::{IntoTransport, Transport};
use serde_json::Value;
use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, RwLock, Weak,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot, Notify};

// ── Cancellation ─────────────────────────────────────────────────────────

struct TokenInner {
    cancelled: AtomicBool,
    notify: Notify,
    children: Mutex<Vec<Weak<TokenInner>>>,
}

impl TokenInner {
    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        self.notify.notify_waiters();
        let children =
            std::mem::take(&mut *self.children.lock().unwrap_or_else(|e| e.into_inner()));
        for child in children.into_iter().filter_map(|c| c.upgrade()) {
            child.cancel();
        }
    }
}

/// A cooperative cancellation signal; cancelling a token cancels every
/// child derived from it.
#[derive(Clone)]
pub struct CancellationToken(Arc<TokenInner>);

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(TokenInner {
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
            children: Mutex::new(Vec::new()),
        }))
    }
    pub fn child_token(&self) -> Self {
        let child = Self::new();
        let mut children = self.0.children.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_cancelled() {
            child.cancel();
        } else {
            children.retain(|c| c.strong_count() > 0);
            children.push(Arc::downgrade(&child.0));
        }
        child
    }
    pub fn cancel(&self) {
        self.0.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }
    pub async fn cancelled(&self) {
        loop {
            let notified = self.0.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
    /// Cancel this token when the guard drops (unless disarmed).
    pub fn drop_guard(self) -> DropGuard {
        DropGuard(Some(self))
    }
}

pub struct DropGuard(Option<CancellationToken>);

impl DropGuard {
    pub fn disarm(mut self) -> CancellationToken {
        self.0.take().unwrap_or_default()
    }
}

impl Drop for DropGuard {
    fn drop(&mut self) {
        if let Some(token) = self.0.take() {
            token.cancel();
        }
    }
}

// ── Errors ───────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum ServiceError {
    TransportClosed,
    TransportSend(String),
    McpError(ErrorData),
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TransportClosed => f.write_str("Transport closed"),
            Self::TransportSend(e) => write!(f, "Transport send error: {e}"),
            Self::McpError(e) => write!(f, "Mcp error: {e}"),
        }
    }
}

impl std::error::Error for ServiceError {}

/// Why a session ended before it was established.
#[derive(Debug)]
pub enum ServerInitializeError {
    ExpectedInitializeRequest,
    ConnectionClosed(String),
    InitializeFailed(ErrorData),
    Transport(String),
    Cancelled,
}

impl std::fmt::Display for ServerInitializeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExpectedInitializeRequest => {
                f.write_str("expect initialized request, but received another message")
            }
            Self::ConnectionClosed(when) => write!(f, "connection closed: {when}"),
            Self::InitializeFailed(e) => write!(f, "initialize failed: {e}"),
            Self::Transport(e) => write!(f, "Send message error {e}"),
            Self::Cancelled => f.write_str("Cancelled"),
        }
    }
}

impl std::error::Error for ServerInitializeError {}

// ── Peer and contexts ────────────────────────────────────────────────────

/// Marker for the server side of a session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoleServer;

type Responder = oneshot::Sender<Result<(), ServiceError>>;

/// The handle a handler uses to send notifications to its client.
/// Maximum concurrent active `subscriptions/listen` streams per session.
pub const MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION: usize = 16;
/// Maximum resource URIs permitted in a single `subscriptions/listen` filter.
pub const MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER: usize = 256;
/// Maximum serialized JSON bytes per `notifications/message` or `notifications/progress`.
pub const MAX_NOTIFICATION_PAYLOAD_BYTES: usize = 16 * 1024;
/// Maximum operational `notifications/message` notifications permitted per request.
pub const MAX_LOG_NOTIFICATIONS_PER_REQUEST: usize = 64;

const SENSITIVE_KEYS: &str = "authorization password passwd secret token api_key apikey credential credentials private_key access_token refresh_token cookie";

fn truncate_utf8_bytes(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    const SUFFIX: &str = "...[truncated]";
    if max_bytes <= SUFFIX.len() {
        return text[..text.floor_char_boundary(max_bytes)].to_string();
    }
    format!(
        "{}{SUFFIX}",
        &text[..text.floor_char_boundary(max_bytes - SUFFIX.len())]
    )
}

fn is_sensitive_field_name(key: &str) -> bool {
    SENSITIVE_KEYS
        .split(' ')
        .any(|k| k.eq_ignore_ascii_case(key))
}

fn scrub_inline_kv_secrets(text: &str) -> String {
    let (mut out, mut rest) = (String::with_capacity(text.len()), text);
    while let Some(pos) = rest.find(['=', ':']) {
        let start = rest[..pos]
            .rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-')
            .map_or(0, |i| i + 1);
        let after = &rest[pos + 1..];
        if is_sensitive_field_name(&rest[start..pos]) && !after.starts_with("[REDACTED]") {
            let trimmed = after.trim_start_matches([' ', '\t']);
            let end = trimmed
                .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | '}'))
                .unwrap_or(trimmed.len());
            out.push_str(&rest[..=pos + after.len() - trimmed.len()]);
            out.push_str("[REDACTED]");
            rest = &trimmed[end..];
        } else {
            out.push_str(&rest[..=pos]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn sanitize_notification_text(raw: &str, max_bytes: usize) -> String {
    let sink = crate::susi_core::untrusted_content::wrap_tool_output("notification", raw)
        .redacted_for_sink();
    truncate_utf8_bytes(
        &scrub_inline_kv_secrets(&crate::susi_error::redact::mask_env_credentials(&sink)),
        max_bytes,
    )
}

fn sanitize_json_value(value: Value, depth: usize) -> Value {
    if depth > 16 {
        return Value::String("[truncated]".to_string());
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => value,
        Value::String(s) => Value::String(sanitize_notification_text(
            &s,
            MAX_NOTIFICATION_PAYLOAD_BYTES / 2,
        )),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(128)
                .map(|v| sanitize_json_value(v, depth + 1))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.into_iter()
                .take(128)
                .map(|(k, v)| {
                    if is_sensitive_field_name(&k) {
                        (k, Value::String("[REDACTED]".to_string()))
                    } else {
                        (k, sanitize_json_value(v, depth + 1))
                    }
                })
                .collect(),
        ),
    }
}

fn clamp_text_field<P: serde::Serialize>(
    params: &mut P,
    raw: &str,
    mut apply: impl FnMut(&mut P, String),
) -> bool {
    let sanitized = sanitize_notification_text(raw, MAX_NOTIFICATION_PAYLOAD_BYTES);
    let mut bound = sanitized.len().min(MAX_NOTIFICATION_PAYLOAD_BYTES);
    loop {
        apply(params, truncate_utf8_bytes(&sanitized, bound));
        if serde_json::to_vec(params).is_ok_and(|b| b.len() <= MAX_NOTIFICATION_PAYLOAD_BYTES) {
            return true;
        }
        if bound == 0 {
            return false;
        }
        bound /= 2;
    }
}

fn sanitize_logging_param(
    mut params: LoggingMessageNotificationParam,
) -> LoggingMessageNotificationParam {
    params.logger = params.logger.map(|l| sanitize_notification_text(&l, 256));
    params.data = sanitize_json_value(params.data, 0);
    if !serde_json::to_vec(&params).is_ok_and(|b| b.len() <= MAX_NOTIFICATION_PAYLOAD_BYTES) {
        let raw = if let Value::String(s) = &params.data {
            s.clone()
        } else {
            params.data.to_string()
        };
        if !clamp_text_field(&mut params, &raw, |p, s| p.data = Value::String(s)) {
            params.logger = None;
            params.data = Value::String("[truncated]".to_string());
        }
    }
    params
}

fn sanitize_progress_param(
    mut params: ProgressNotificationParam,
) -> Result<ProgressNotificationParam, ServiceError> {
    let bad = (!params.progress.is_finite() || params.progress < 0.0)
        .then_some("value")
        .or_else(|| {
            params
                .total
                .is_some_and(|t| !t.is_finite() || t < 0.0)
                .then_some("total")
        });
    if let Some(field) = bad {
        return Err(ServiceError::McpError(ErrorData::invalid_params(
            format!("Progress {field} must be finite and non-negative"),
            None,
        )));
    }
    if let Some(msg) = params.message.take() {
        if !clamp_text_field(&mut params, &msg, |p, s| p.message = Some(s)) {
            params.message = None;
        }
    }
    Ok(params)
}

fn progress_token_key(token: &ProgressToken) -> String {
    serde_json::to_string(token).unwrap_or_else(|_| "\"invalid\"".to_string())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
struct ProgressEntry {
    lease_id: u64,
    last_progress: Option<f64>,
    stop: CancellationToken,
}

/// RAII lease tying an active `progressToken` to its request or task lifetime.
#[derive(Debug)]
pub struct ProgressLease {
    pub(crate) peer: Peer,
    key: String,
    token: ProgressToken,
    lease_id: u64,
    stop: CancellationToken,
}

impl ProgressLease {
    pub fn token(&self) -> &ProgressToken {
        &self.token
    }
    pub fn stop_token(&self) -> CancellationToken {
        self.stop.clone()
    }
    pub fn stop(&self) {
        self.stop.cancel();
    }
    /// Emit a boundary (`0.0` or `1.0`) progress update if the lease is active.
    pub async fn notify_edge(
        &self,
        progress: f64,
        message: Option<&str>,
    ) -> Result<(), ServiceError> {
        if self.stop.is_cancelled() {
            return Ok(());
        }
        let mut param =
            ProgressNotificationParam::new(self.token.clone(), progress).with_total(1.0);
        if let Some(msg) = message {
            param = param.with_message(msg);
        }
        self.peer.notify_progress(param).await
    }
}

impl Drop for ProgressLease {
    fn drop(&mut self) {
        self.stop.cancel();
        let mut guard = lock(&self.peer.progress_tokens);
        if guard
            .get(&self.key)
            .is_some_and(|e| e.lease_id == self.lease_id)
        {
            guard.remove(&self.key);
        }
    }
}

/// RAII lease enforcing the per-session cap on active `subscriptions/listen` streams.
#[derive(Debug)]
pub struct SubscriptionLease {
    active: Arc<std::sync::atomic::AtomicUsize>,
}

impl Drop for SubscriptionLease {
    fn drop(&mut self) {
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct Peer {
    tx: mpsc::Sender<(ServerNotification, Responder)>,
    info: Arc<RwLock<Option<Arc<InitializeRequestParams>>>>,
    request_metadata_required: Arc<AtomicBool>,
    progress_tokens: Arc<std::sync::Mutex<std::collections::HashMap<String, ProgressEntry>>>,
    next_progress_lease: Arc<std::sync::atomic::AtomicU64>,
    active_subscriptions: Arc<std::sync::atomic::AtomicUsize>,
}

impl std::fmt::Debug for Peer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Peer").finish_non_exhaustive()
    }
}

impl Peer {
    fn new(
        info: Option<InitializeRequestParams>,
    ) -> (Self, mpsc::Receiver<(ServerNotification, Responder)>) {
        let (tx, rx) = mpsc::channel(1024);
        (
            Self {
                tx,
                info: Arc::new(RwLock::new(info.map(Arc::new))),
                request_metadata_required: Default::default(),
                progress_tokens: Default::default(),
                next_progress_lease: Default::default(),
                active_subscriptions: Default::default(),
            },
            rx,
        )
    }
    pub fn peer_info(&self) -> Option<Arc<InitializeRequestParams>> {
        self.info.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
    pub fn set_peer_info(&self, info: InitializeRequestParams) {
        *self.info.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(info));
    }
    fn require_request_metadata(&self) {
        self.request_metadata_required
            .store(true, Ordering::Release);
    }
    pub fn request_metadata_required(&self) -> bool {
        self.request_metadata_required.load(Ordering::Acquire)
    }
    pub fn is_transport_closed(&self) -> bool {
        self.tx.is_closed()
    }
    pub async fn send_notification(
        &self,
        notification: ServerNotification,
    ) -> Result<(), ServiceError> {
        let (responder, receiver) = oneshot::channel();
        self.tx
            .send((notification, responder))
            .await
            .map_err(|_| ServiceError::TransportClosed)?;
        receiver.await.map_err(|_| ServiceError::TransportClosed)?
    }
    /// Reserve an active `subscriptions/listen` slot within the per-session cap.
    pub fn enter_subscription(&self) -> Result<SubscriptionLease, ErrorData> {
        self.active_subscriptions
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |c| {
                (c < MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION).then_some(c + 1)
            })
            .map(|_| SubscriptionLease {
                active: self.active_subscriptions.clone(),
            })
            .map_err(|_| {
                ErrorData::invalid_params("Too many concurrent subscriptions in session", None)
            })
    }

    /// Reserve an active `progressToken` in this session, rejecting active duplicates (`-32602`).
    pub fn enter_progress(&self, token: ProgressToken) -> Result<ProgressLease, ErrorData> {
        let key = progress_token_key(&token);
        let mut guard = lock(&self.progress_tokens);
        if guard.contains_key(&key) {
            return Err(ErrorData::invalid_params(
                "Duplicate active progressToken",
                None,
            ));
        }
        let lease_id = self.next_progress_lease.fetch_add(1, Ordering::Relaxed);
        let stop = CancellationToken::new();
        guard.insert(
            key.clone(),
            ProgressEntry {
                lease_id,
                last_progress: None,
                stop: stop.clone(),
            },
        );
        Ok(ProgressLease {
            peer: self.clone(),
            key,
            token,
            lease_id,
            stop,
        })
    }

    pub async fn notify_progress(
        &self,
        params: ProgressNotificationParam,
    ) -> Result<(), ServiceError> {
        let params = sanitize_progress_param(params)?;
        let key = progress_token_key(&params.progress_token);
        {
            let mut guard = lock(&self.progress_tokens);
            if let Some(entry) = guard.get_mut(&key) {
                if entry.stop.is_cancelled() {
                    return Err(ServiceError::McpError(ErrorData::invalid_request(
                        "Progress cancelled",
                        None,
                    )));
                }
                if entry
                    .last_progress
                    .is_some_and(|prev| params.progress <= prev)
                {
                    return Err(ServiceError::McpError(ErrorData::invalid_params(
                        "Progress value must strictly increase",
                        None,
                    )));
                }
                entry.last_progress = Some(params.progress);
            }
        }
        self.send_notification(ServerNotification::new("notifications/progress", params))
            .await
    }
    pub async fn notify_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
    ) -> Result<(), ServiceError> {
        let params = sanitize_logging_param(params);
        self.send_notification(ServerNotification::new("notifications/message", params))
            .await
    }
    pub async fn notify_resource_updated(
        &self,
        params: ResourceUpdatedNotificationParam,
    ) -> Result<(), ServiceError> {
        self.send_notification(ServerNotification::new(
            "notifications/resources/updated",
            params,
        ))
        .await
    }
    async fn send_empty(&self, method: &'static str) -> Result<(), ServiceError> {
        self.send_notification(ServerNotification::no_params(method))
            .await
    }
    pub async fn notify_resource_list_changed(&self) -> Result<(), ServiceError> {
        self.send_empty("notifications/resources/list_changed")
            .await
    }
    pub async fn notify_tool_list_changed(&self) -> Result<(), ServiceError> {
        self.send_empty("notifications/tools/list_changed").await
    }
    pub async fn notify_prompt_list_changed(&self) -> Result<(), ServiceError> {
        self.send_empty("notifications/prompts/list_changed").await
    }
}

/// Everything a request handler knows about the request it serves.
#[derive(Debug, Clone)]
pub struct RequestContext<R = RoleServer> {
    pub ct: CancellationToken,
    pub id: RequestId,
    pub meta: RequestMetaObject,
    pub peer: Peer,
    pub(crate) progress_lease: Option<Arc<ProgressLease>>,
    pub(crate) log_count: Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) is_subscription_stream: bool,
    _role: std::marker::PhantomData<R>,
}

impl<R> RequestContext<R> {
    pub fn progress_lease(&self) -> Option<Arc<ProgressLease>> {
        self.progress_lease.clone()
    }
    pub fn is_subscription_stream(&self) -> bool {
        self.is_subscription_stream
    }
    /// Emit an operational `notifications/message` scoped to this request.
    pub async fn emit_logging_message(
        &self,
        threshold: Option<LoggingLevel>,
        level: LoggingLevel,
        logger: Option<String>,
        data: serde_json::Value,
    ) -> Result<bool, ServiceError> {
        if self.is_subscription_stream
            || !threshold.is_some_and(|min| min.allows(level))
            || self.log_count.fetch_add(1, Ordering::Relaxed) >= MAX_LOG_NOTIFICATIONS_PER_REQUEST
        {
            return Ok(false);
        }
        let params = sanitize_logging_param(LoggingMessageNotificationParam {
            level,
            logger,
            data,
            meta: None,
        });
        let note = ServerNotification::new("notifications/message", params)
            .with_target_request(self.id.clone());
        self.peer.send_notification(note).await?;
        Ok(true)
    }
}

impl RequestContext<RoleServer> {
    pub fn new(id: RequestId, peer: Peer) -> Self {
        Self::with_meta(
            CancellationToken::new(),
            id,
            RequestMetaObject::default(),
            peer,
        )
    }
    fn with_meta(
        ct: CancellationToken,
        id: RequestId,
        meta: RequestMetaObject,
        peer: Peer,
    ) -> Self {
        Self {
            ct,
            id,
            meta,
            peer,
            progress_lease: None,
            log_count: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            is_subscription_stream: false,
            _role: std::marker::PhantomData,
        }
    }
    /// The revision this request runs under: its own `_meta` version, else
    /// the one negotiated at `initialize`.
    pub fn protocol_version(&self) -> Option<ProtocolVersion> {
        self.meta
            .protocol_version()
            .or_else(|| self.peer.peer_info().map(|i| i.protocol_version.clone()))
    }
    fn meta_or_peer<T>(
        &self,
        meta: Option<T>,
        from_peer: impl FnOnce(&InitializeRequestParams) -> T,
    ) -> Option<T> {
        if self.peer.request_metadata_required() {
            meta
        } else {
            meta.or_else(|| self.peer.peer_info().map(|i| from_peer(&i)))
        }
    }
    pub fn client_info(&self) -> Option<Implementation> {
        self.meta_or_peer(self.meta.client_info(), |i| i.client_info.clone())
    }
    pub fn client_capabilities(&self) -> Option<ClientCapabilities> {
        self.meta_or_peer(self.meta.client_capabilities(), |i| i.capabilities.clone())
    }
}

#[derive(Debug, Clone)]
pub struct NotificationContext<R = RoleServer> {
    pub meta: NotificationMetaObject,
    pub peer: Peer,
    _role: std::marker::PhantomData<R>,
}

/// Failure to send through a [`SubscriptionSink`].
#[derive(Debug)]
pub enum SubscriptionSendError {
    SubscriptionClosed,
    UnsupportedNotification(&'static str),
    NotificationNotAccepted(&'static str),
    Service(ServiceError),
}

impl std::fmt::Display for SubscriptionSendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SubscriptionClosed => f.write_str("subscription is no longer active"),
            Self::UnsupportedNotification(n) => {
                write!(
                    f,
                    "notification is not allowed on a subscription stream: {n}"
                )
            }
            Self::NotificationNotAccepted(n) => {
                write!(
                    f,
                    "notification was not accepted for this subscription: {n}"
                )
            }
            Self::Service(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for SubscriptionSendError {}

/// Sends the change notifications one `subscriptions/listen` accepted,
/// each tagged with the subscription's id.
#[derive(Debug, Clone)]
pub struct SubscriptionSink {
    peer: Peer,
    id: RequestId,
    accepted: Arc<SubscriptionFilter>,
    active: CancellationToken,
}

impl SubscriptionSink {
    async fn send(
        &self,
        mut notification: ServerNotification,
        accepted: bool,
        name: &'static str,
    ) -> Result<(), SubscriptionSendError> {
        if matches!(
            notification.method.as_ref(),
            "notifications/message" | "notifications/progress"
        ) {
            return Err(SubscriptionSendError::UnsupportedNotification(name));
        }
        if self.active.is_cancelled() {
            return Err(SubscriptionSendError::SubscriptionClosed);
        }
        if !accepted {
            return Err(SubscriptionSendError::NotificationNotAccepted(name));
        }
        notification.set_meta(META_SUBSCRIPTION_ID, self.id.clone().into_json_value());
        self.peer
            .send_notification(notification)
            .await
            .map_err(SubscriptionSendError::Service)
    }
    async fn changed(
        &self,
        m: &'static str,
        ok: Option<bool>,
    ) -> Result<(), SubscriptionSendError> {
        self.send(ServerNotification::no_params(m), ok == Some(true), m)
            .await
    }
    pub async fn notify_tool_list_changed(&self) -> Result<(), SubscriptionSendError> {
        self.changed(
            "notifications/tools/list_changed",
            self.accepted.tools_list_changed,
        )
        .await
    }
    pub async fn notify_prompt_list_changed(&self) -> Result<(), SubscriptionSendError> {
        self.changed(
            "notifications/prompts/list_changed",
            self.accepted.prompts_list_changed,
        )
        .await
    }
    pub async fn notify_resource_list_changed(&self) -> Result<(), SubscriptionSendError> {
        self.changed(
            "notifications/resources/list_changed",
            self.accepted.resources_list_changed,
        )
        .await
    }
    pub async fn notify_resource_updated(
        &self,
        uri: impl Into<String>,
    ) -> Result<(), SubscriptionSendError> {
        const M: &str = "notifications/resources/updated";
        let uri = uri.into();
        let ok = self
            .accepted
            .resource_subscriptions
            .as_ref()
            .is_some_and(|uris| uris.contains(&uri));
        self.send(
            ServerNotification::new(M, ResourceUpdatedNotificationParam::new(uri)),
            ok,
            M,
        )
        .await
    }
}

/// One established `subscriptions/listen`; its acknowledgment is already sent.
pub struct SubscriptionContext {
    request: RequestContext,
    sink: SubscriptionSink,
    prepared: std::sync::Mutex<Option<Box<dyn std::any::Any + Send>>>,
    _active: DropGuard,
}

impl SubscriptionContext {
    async fn establish(
        request: RequestContext,
        accepted: SubscriptionFilter,
        prepared: Box<dyn std::any::Any + Send>,
    ) -> Result<Self, ErrorData> {
        let active = request.ct.child_token();
        let mut ack = ServerNotification::new(
            "notifications/subscriptions/acknowledged",
            serde_json::json!({ "notifications": accepted }),
        );
        ack.set_meta(META_SUBSCRIPTION_ID, request.id.clone().into_json_value());
        request.peer.send_notification(ack).await.map_err(|e| {
            ErrorData::internal_error(format!("failed to acknowledge subscription: {e}"), None)
        })?;
        let sink = SubscriptionSink {
            peer: request.peer.clone(),
            id: request.id.clone(),
            accepted: Arc::new(accepted),
            active: active.clone(),
        };
        Ok(Self {
            request,
            sink,
            prepared: std::sync::Mutex::new(Some(prepared)),
            _active: active.drop_guard(),
        })
    }
    pub fn sink(&self) -> &SubscriptionSink {
        &self.sink
    }
    pub fn accepted(&self) -> &SubscriptionFilter {
        &self.sink.accepted
    }
    /// Take state captured by [`ServerHandler::prepare_subscription`] before ack.
    pub fn take_prepared<T: 'static>(&self) -> Option<T> {
        lock(&self.prepared)
            .take()
            .and_then(|b| b.downcast::<T>().ok())
            .map(|b| *b)
    }
    pub async fn cancelled(&self) {
        self.request.ct.cancelled().await;
    }
}

// ── The handler contract ─────────────────────────────────────────────────

/// The application side of an MCP server. Every method has the default an
/// MCP server without that capability answers with.
#[allow(unused_variables)]
pub trait ServerHandler: Sized + Send + Sync + 'static {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::default()
    }
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::KNOWN_VERSIONS)
    }
    fn ping(
        &self,
        context: RequestContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Ok(()))
    }
    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<InitializeResult, ErrorData>> + Send + '_ {
        context.peer.set_peer_info(request.clone());
        std::future::ready(negotiate_initialize(self, &request))
    }
    fn discover(
        &self,
        context: RequestContext,
    ) -> impl Future<Output = Result<DiscoverResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(DiscoverResult::from_server_info(
            self.supported_protocol_versions().into_owned(),
            self.get_info(),
        )))
    }
    fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<CompleteResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(CompleteResult::default()))
    }
    fn set_level(
        &self,
        request: SetLevelRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("logging/setLevel")))
    }
    fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<GetPromptResponse, ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("prompts/get")))
    }
    fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext,
    ) -> impl Future<Output = Result<ListPromptsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListPromptsResult::default()))
    }
    fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult::default()))
    }
    fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext,
    ) -> impl Future<Output = Result<ListResourceTemplatesResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListResourceTemplatesResult::default()))
    }
    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<ReadResourceResponse, ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("resources/read")))
    }
    /// The part of a requested filter this server accepts; `None` leaves
    /// `subscriptions/listen` unimplemented.
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        None
    }
    /// Capture handler state before `notifications/subscriptions/acknowledged`
    /// is sent so concurrent events emitted right after ack are not lost.
    fn prepare_subscription(&self) -> Box<dyn std::any::Any + Send> {
        Box::new(())
    }
    fn listen(
        &self,
        context: SubscriptionContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        async move {
            context.cancelled().await;
            Ok(())
        }
    }
    fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("resources/subscribe")))
    }
    fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("resources/unsubscribe")))
    }
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("tools/call")))
    }
    fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListToolsResult::default()))
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        None
    }
    fn on_initialized(&self, context: NotificationContext) -> impl Future<Output = ()> + Send + '_ {
        std::future::ready(())
    }
    fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<GetTaskResult, ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("tasks/get")))
    }
    fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("tasks/update")))
    }
    fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext,
    ) -> impl Future<Output = Result<(), ErrorData>> + Send + '_ {
        std::future::ready(Err(ErrorData::method_not_found("tasks/cancel")))
    }
}

/// The `initialize` answer: the server info with the version negotiated
/// against [`ServerHandler::supported_protocol_versions`].
pub fn negotiate_initialize<H: ServerHandler>(
    handler: &H,
    request: &InitializeRequestParams,
) -> Result<InitializeResult, ErrorData> {
    let mut info = handler.get_info();
    info.protocol_version = negotiate_protocol_version(
        &request.protocol_version,
        std::mem::take(&mut info.protocol_version),
        &handler.supported_protocol_versions(),
    )?;
    Ok(info)
}

/// Echo the client's version when it has a handshake and is supported;
/// otherwise answer the preferred (or newest) handshake version.
pub fn negotiate_protocol_version(
    requested: &ProtocolVersion,
    preferred: ProtocolVersion,
    supported: &[ProtocolVersion],
) -> Result<ProtocolVersion, ErrorData> {
    if requested.has_initialize() && supported.contains(requested) {
        return Ok(requested.clone());
    }
    if preferred.has_initialize() {
        return Ok(preferred);
    }
    supported
        .iter()
        .filter(|v| v.has_initialize())
        .max_by(|a, b| a.as_str().cmp(b.as_str()))
        .cloned()
        .ok_or_else(|| ErrorData::unsupported_protocol_version(requested.clone(), supported))
}

fn uses_legacy_lifecycle(version: Option<&ProtocolVersion>, inline: bool) -> bool {
    !inline && version.is_none_or(ProtocolVersion::has_initialize)
}

fn requires_tasks_err() -> ErrorData {
    ErrorData::missing_required_client_capability(
        ClientCapabilities::builder().enable_tasks().build(),
    )
}

fn requires_tasks(ctx: &RequestContext) -> Result<(), ErrorData> {
    ctx.client_capabilities()
        .is_some_and(|c| c.supports_tasks())
        .then_some(())
        .ok_or_else(requires_tasks_err)
}

fn tasks_gate<H: ServerHandler>(
    handler: &H,
    context: &RequestContext,
    method: &'static str,
) -> Result<(), ErrorData> {
    if !handler.get_info().capabilities.supports_tasks() {
        return Err(ErrorData::method_not_found(method));
    }
    requires_tasks(context)
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn handle_listen<H: ServerHandler>(
    handler: &H,
    params: SubscriptionsListenRequestParams,
    mut context: RequestContext,
) -> Result<ServerResult, ErrorData> {
    let requested = params.notifications;
    if requested
        .resource_subscriptions
        .as_ref()
        .is_some_and(|u| u.len() > MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER)
    {
        return Err(ErrorData::invalid_params(
            "Too many resource subscriptions requested",
            None,
        ));
    }
    let Some(candidate) = handler.accepted_subscription_filter(&requested) else {
        return Err(ErrorData::method_not_found("subscriptions/listen"));
    };
    let _lease = context.peer.enter_subscription()?;
    let info = handler.get_info();
    let advertised = requested.supported_by(&info.capabilities);
    let accepted = candidate.intersection(&requested).intersection(&advertised);
    let id = context.id.clone();
    context.is_subscription_stream = true;
    let prepared = handler.prepare_subscription();
    let subscription = SubscriptionContext::establish(context, accepted, prepared).await?;
    handler.listen(subscription).await.map(|()| {
        let mut meta = JsonObject::new();
        meta.insert(META_SUBSCRIPTION_ID.to_string(), id.into_json_value());
        if let Ok(server_info) = serde_json::to_value(info.server_info) {
            meta.insert(META_SERVER_INFO.to_string(), server_info);
        }
        ServerResult::SubscriptionsListen(
            serde_json::json!({"resultType": "complete", "_meta": meta}),
        )
    })
}

/// Dispatch one request: per-request version and metadata checks, the
/// lifecycle gates, the handler, then the era-specific result shape.
pub async fn handle_request<H: ServerHandler>(
    handler: &H,
    request: ClientRequest,
    context: RequestContext,
) -> Result<ServerResult, ErrorData> {
    let protocol_version = context.protocol_version();
    let modern = protocol_version
        .as_ref()
        .is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str());
    let requested = context.meta.protocol_version();
    let inline = !matches!(request, ClientRequest::Initialize(_));
    use ErrorData as E;
    if inline {
        if let Some(requested) = requested.as_ref() {
            let supported = handler.supported_protocol_versions();
            if !supported.contains(requested) {
                return Err(E::unsupported_protocol_version(
                    requested.clone(),
                    &supported,
                ));
            }
        }
    }
    let requires_metadata = inline
        && (matches!(request, ClientRequest::Discover)
            || context.peer.request_metadata_required()
            || requested
                .as_ref()
                .is_some_and(|v| v.as_str() >= ProtocolVersion::V_2026_07_28.as_str()));
    if requires_metadata {
        let missing = context
            .meta
            .missing_required_keys(&ProtocolVersion::V_2026_07_28);
        if !missing.is_empty() {
            return Err(missing_metadata_error(&missing));
        }
    }
    let mut ctx = context;
    ctx.meta.parse_log_level()?;
    let progress_lease = ctx
        .meta
        .parse_progress_token()?
        .map(|t| ctx.peer.enter_progress(t).map(Arc::new))
        .transpose()?;
    ctx.progress_lease = progress_lease.clone();
    let _cancel_watch = progress_lease.map(|lease| {
        let ct = ctx.ct.clone();
        AbortOnDrop(tokio::spawn(async move {
            ct.cancelled().await;
            lease.stop();
        }))
    });
    let legacy = uses_legacy_lifecycle(protocol_version.as_ref(), requires_metadata);
    let init = ctx
        .peer
        .peer_info()
        .is_some_and(|i| i.protocol_version.has_initialize());
    use ClientRequest as Q;
    use ServerResult as S;
    let result = match request {
        Q::Initialize(p) => handler.initialize(p, ctx).await.map(S::Initialize),
        Q::Discover => handler.discover(ctx).await.map(S::Discover),
        Q::Ping if !legacy => Err(E::method_not_found("ping")),
        Q::Ping => handler.ping(ctx).await.map(|()| S::Empty),
        Q::Complete(p) => handler.complete(p, ctx).await.map(S::Complete),
        Q::SetLevel(_) if !legacy && !init => Err(E::method_not_found("logging/setLevel")),
        Q::SetLevel(p) => handler.set_level(p, ctx).await.map(|()| S::Empty),
        Q::GetPrompt(p) => handler.get_prompt(p, ctx).await.map(S::from),
        Q::ListPrompts(p) => handler.list_prompts(p, ctx).await.map(S::ListPrompts),
        Q::ListResources(p) => handler.list_resources(p, ctx).await.map(S::ListResources),
        Q::ListResourceTemplates(p) => handler
            .list_resource_templates(p, ctx)
            .await
            .map(S::ListResourceTemplates),
        Q::ReadResource(p) => handler.read_resource(p, ctx).await.map(S::from),
        Q::SubscriptionsListen(_) if legacy => Err(E::method_not_found("subscriptions/listen")),
        Q::SubscriptionsListen(p) => handle_listen(handler, p, ctx).await,
        Q::Subscribe(_) if !legacy => Err(E::method_not_found("resources/subscribe")),
        Q::Subscribe(p) => handler.subscribe(p, ctx).await.map(|()| S::Empty),
        Q::Unsubscribe(_) if !legacy => Err(E::method_not_found("resources/unsubscribe")),
        Q::Unsubscribe(p) => handler.unsubscribe(p, ctx).await.map(|()| S::Empty),
        Q::CallTool(p) => {
            let declared_tasks = requires_tasks(&ctx).is_ok();
            let response = handler.call_tool(p, ctx).await?;
            if matches!(response, CallToolResponse::Task(_)) && !declared_tasks {
                return Err(requires_tasks_err());
            }
            Ok(S::from(response))
        }
        Q::ListTools(p) => handler.list_tools(p, ctx).await.map(S::ListTools),
        Q::Custom { method, .. } => Err(E::new(ErrorCode::METHOD_NOT_FOUND, method, None)),
        Q::GetTask(p) => {
            tasks_gate(handler, &ctx, "tasks/get")?;
            handler.get_task(p, ctx).await.map(S::GetTask)
        }
        Q::UpdateTask(p) => {
            tasks_gate(handler, &ctx, "tasks/update")?;
            handler.update_task(p, ctx).await.map(|()| S::TaskAck)
        }
        Q::CancelTask(p) => {
            tasks_gate(handler, &ctx, "tasks/cancel")?;
            handler.cancel_task(p, ctx).await.map(|()| S::TaskAck)
        }
    };
    result
        .and_then(|mut result| {
            if result.is_input_required() && !modern {
                return Err(ErrorData::invalid_request(
                    "InputRequiredResult requires negotiated protocol version 2026-07-28 or newer",
                    None,
                ));
            }
            if modern {
                result.fill_missing_cache_hints();
            } else {
                result.strip_result_type_for_legacy_peer();
            }
            Ok(result)
        })
        .map_err(|mut error| {
            if modern && error.code == ErrorCode::RESOURCE_NOT_FOUND {
                error.code = ErrorCode::INVALID_PARAMS;
            }
            error
        })
}

fn missing_metadata_error(missing: &[&str]) -> ErrorData {
    ErrorData::invalid_params(
        format!(
            "request _meta is missing or has malformed required fields: {}",
            missing.join(", ")
        ),
        None,
    )
}

async fn handle_notification<H: ServerHandler>(
    handler: &H,
    notification: ClientNotification,
    peer: Peer,
) {
    if matches!(notification.kind, ClientNotificationKind::Initialized) {
        let context = NotificationContext {
            meta: notification.meta,
            peer,
            _role: std::marker::PhantomData,
        };
        handler.on_initialized(context).await;
    }
}

// ── Serving a transport ──────────────────────────────────────────────────

#[derive(Debug)]
pub enum QuitReason {
    Cancelled,
    Closed,
}

/// A served session; dropping it cancels the session.
pub struct RunningService<S> {
    service: Arc<S>,
    peer: Peer,
    handle: Option<tokio::task::JoinHandle<QuitReason>>,
    ct: CancellationToken,
}

impl<S> RunningService<S> {
    pub fn service(&self) -> &S {
        &self.service
    }
    pub fn peer(&self) -> &Peer {
        &self.peer
    }
    pub fn cancellation_token(&self) -> CancellationToken {
        self.ct.clone()
    }
    /// Wait until the session ends.
    pub async fn waiting(mut self) -> Result<QuitReason, tokio::task::JoinError> {
        match self.handle.take() {
            Some(handle) => handle.await,
            None => Ok(QuitReason::Closed),
        }
    }
    pub async fn cancel(self) -> Result<QuitReason, tokio::task::JoinError> {
        self.ct.cancel();
        self.waiting().await
    }
}

impl<S> Drop for RunningService<S> {
    fn drop(&mut self) {
        if self.handle.is_some() {
            self.ct.cancel();
        }
    }
}

/// Serve a handler on a transport (stdio, a byte stream or an HTTP session).
pub trait ServiceExt: ServerHandler {
    fn serve<T, A>(
        self,
        transport: T,
    ) -> impl Future<Output = Result<RunningService<Self>, ServerInitializeError>> + Send
    where
        T: IntoTransport<A> + Send,
    {
        serve_server(self, transport.into_transport(), CancellationToken::new())
    }
}

impl<S: ServerHandler> ServiceExt for S {}

/// Establish a session: answer pings and `server/discover` probes until an
/// `initialize` (legacy lifecycle) or a first request carrying complete
/// per-request metadata (2026-07-28 lifecycle) arrives.
pub async fn serve_server<S: ServerHandler, T: Transport>(
    service: S,
    mut transport: T,
    ct: CancellationToken,
) -> Result<RunningService<S>, ServerInitializeError> {
    let cancelled = ct.clone();
    tokio::select! {
        result = establish(service, &mut transport, ct.clone()) => {
            let (service, peer, peer_rx, initial) = result?;
            Ok(serve_inner(service, transport, (peer, peer_rx), initial, ct))
        }
        _ = cancelled.cancelled() => Err(ServerInitializeError::Cancelled),
    }
}

type PeerRx = mpsc::Receiver<(ServerNotification, Responder)>;

async fn send_now<T: Transport>(
    transport: &mut T,
    message: ServerJsonRpcMessage,
) -> Result<(), ServerInitializeError> {
    transport
        .send(message)
        .await
        .map_err(|e| ServerInitializeError::Transport(e.to_string()))
}

async fn send_err<T: Transport>(
    transport: &mut T,
    error: ErrorData,
    id: RequestId,
) -> Result<(), ServerInitializeError> {
    send_now(transport, ServerJsonRpcMessage::error(error, Some(id))).await
}

fn into_rpc_message(res: Result<ServerResult, ErrorData>, id: RequestId) -> ServerJsonRpcMessage {
    match res {
        Ok(result) => ServerJsonRpcMessage::response(result, id),
        Err(error) => ServerJsonRpcMessage::error(error, Some(id)),
    }
}

fn finish_answer(
    in_flight: &mut HashMap<RequestId, CancellationToken>,
    answer: &ServerJsonRpcMessage,
) -> bool {
    let Some(ct) = answer.answered_id().and_then(|id| in_flight.remove(id)) else {
        return false;
    };
    ct.cancel();
    true
}

async fn establish<S: ServerHandler, T: Transport>(
    service: S,
    transport: &mut T,
    ct: CancellationToken,
) -> Result<(S, Peer, PeerRx, VecDeque<ClientJsonRpcMessage>), ServerInitializeError> {
    let (peer, peer_rx) = Peer::new(None);
    loop {
        let Some(message) = transport.receive().await else {
            return Err(ServerInitializeError::ConnectionClosed(
                "initialize request".into(),
            ));
        };
        // Before a lifecycle exists a notification or stray response is
        // ignored, never fatal: a 2026-07-28 client sends no `initialize`.
        let ClientJsonRpcMessage::Request(request) = message else {
            continue;
        };
        let JsonRpcRequest {
            id,
            method,
            request,
            meta,
        } = request;
        match request {
            ClientRequest::Ping => {
                let msg = ServerJsonRpcMessage::response(ServerResult::Empty, id);
                send_now(transport, msg).await?;
            }
            ClientRequest::Initialize(params) => {
                let requested = params.protocol_version.clone();
                let mut negotiated = params.clone();
                peer.set_peer_info(negotiated.clone());
                let ctx =
                    RequestContext::with_meta(ct.child_token(), id.clone(), meta, peer.clone());
                let req = ClientRequest::Initialize(params);
                let mut init = match handle_request(&service, req, ctx).await {
                    Ok(ServerResult::Initialize(init)) => init,
                    Ok(_) => return Err(ServerInitializeError::ExpectedInitializeRequest),
                    Err(e) => {
                        send_err(transport, e.clone(), id).await?;
                        return Err(ServerInitializeError::InitializeFailed(e));
                    }
                };
                init.protocol_version = match negotiate_protocol_version(
                    &requested,
                    init.protocol_version,
                    &service.supported_protocol_versions(),
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        send_err(transport, e.clone(), id).await?;
                        return Err(ServerInitializeError::InitializeFailed(e));
                    }
                };
                negotiated.protocol_version = init.protocol_version.clone();
                peer.set_peer_info(negotiated);
                let msg = ServerJsonRpcMessage::response(ServerResult::Initialize(init), id);
                send_now(transport, msg).await?;
                return Ok((service, peer, peer_rx, VecDeque::new()));
            }
            request @ (ClientRequest::Discover
            | ClientRequest::Complete(_)
            | ClientRequest::SetLevel(_)
            | ClientRequest::GetPrompt(_)
            | ClientRequest::ListPrompts(_)
            | ClientRequest::ListResources(_)
            | ClientRequest::ListResourceTemplates(_)
            | ClientRequest::ReadResource(_)
            | ClientRequest::SubscriptionsListen(_)
            | ClientRequest::Subscribe(_)
            | ClientRequest::Unsubscribe(_)
            | ClientRequest::CallTool(_)
            | ClientRequest::ListTools(_)
            | ClientRequest::GetTask(_)
            | ClientRequest::UpdateTask(_)
            | ClientRequest::CancelTask(_)
            | ClientRequest::Custom { .. }) => {
                let missing = meta.missing_required_keys(&ProtocolVersion::V_2026_07_28);
                let version = match meta.protocol_version() {
                    Some(v) if missing.is_empty() => v,
                    _ => {
                        send_err(transport, missing_metadata_error(&missing), id).await?;
                        continue;
                    }
                };
                if matches!(request, ClientRequest::Discover) {
                    // No lifecycle yet: the probe's peer cannot reach the client.
                    let (bootstrap, _) = Peer::new(None);
                    let context =
                        RequestContext::with_meta(ct.child_token(), id.clone(), meta, bootstrap);
                    let answer =
                        into_rpc_message(handle_request(&service, request, context).await, id);
                    send_now(transport, answer).await?;
                    continue;
                }
                let supported = service.supported_protocol_versions();
                if !supported.contains(&version) {
                    send_err(
                        transport,
                        ErrorData::unsupported_protocol_version(version, &supported),
                        id,
                    )
                    .await?;
                    continue;
                }
                peer.require_request_metadata();
                let first = ClientJsonRpcMessage::Request(JsonRpcRequest {
                    id,
                    method,
                    request,
                    meta,
                });
                return Ok((service, peer, peer_rx, VecDeque::from([first])));
            }
        }
    }
}

/// Serve a session whose lifecycle the caller already established (the
/// stateless HTTP path: one request, peer info from its headers).
pub fn serve_directly<S: ServerHandler, T: Transport>(
    service: S,
    transport: T,
    peer_info: Option<InitializeRequestParams>,
    ct: CancellationToken,
) -> RunningService<S> {
    let (peer, peer_rx) = Peer::new(peer_info);
    serve_inner(service, transport, (peer, peer_rx), VecDeque::new(), ct)
}

fn serve_inner<S: ServerHandler, T: Transport>(
    service: S,
    mut transport: T,
    (peer, mut peer_rx): (Peer, PeerRx),
    mut batch: VecDeque<ClientJsonRpcMessage>,
    ct: CancellationToken,
) -> RunningService<S> {
    let service = Arc::new(service);
    let (svc, loop_peer, loop_ct) = (service.clone(), peer.clone(), ct.child_token());
    let handle = tokio::spawn(async move {
        let (service, peer) = (svc, loop_peer);
        let (sink_tx, mut sink_rx) = mpsc::channel::<ServerJsonRpcMessage>(64);
        let mut in_flight = HashMap::<RequestId, CancellationToken>::new();
        let mut sends = tokio::task::JoinSet::<()>::new();
        enum Event {
            Peer(ClientJsonRpcMessage),
            Answer(ServerJsonRpcMessage),
            Notify(ServerNotification, Responder),
            Sent,
        }
        let quit = loop {
            let event = match batch.pop_front() {
                Some(message) => Event::Peer(message),
                None => tokio::select! {
                    answer = sink_rx.recv() => match answer {
                        Some(answer) => Event::Answer(answer),
                        None => continue,
                    },
                    message = transport.receive() => match message {
                        Some(message) => Event::Peer(message),
                        None => break QuitReason::Closed,
                    },
                    notification = peer_rx.recv() => match notification {
                        Some((notification, responder)) => Event::Notify(notification, responder),
                        None => continue,
                    },
                    _ = sends.join_next(), if !sends.is_empty() => Event::Sent,
                    _ = loop_ct.cancelled() => break QuitReason::Cancelled,
                },
            };
            match event {
                Event::Sent => {}
                Event::Answer(answer) => {
                    if !finish_answer(&mut in_flight, &answer) {
                        continue;
                    }
                    let send = transport.send(answer);
                    sends.spawn(async move {
                        let _ = send.await;
                    });
                }
                Event::Notify(notification, responder) => {
                    let send = transport.send(ServerJsonRpcMessage::Notification(notification));
                    sends.spawn(async move {
                        let result = send
                            .await
                            .map_err(|e| ServiceError::TransportSend(e.to_string()));
                        let _ = responder.send(result);
                    });
                }
                Event::Peer(ClientJsonRpcMessage::Request(request)) => {
                    let request_ct = loop_ct.child_token();
                    let context = RequestContext::with_meta(
                        request_ct.child_token(),
                        request.id.clone(),
                        request.meta,
                        peer.clone(),
                    );
                    in_flight.insert(request.id.clone(), request_ct);
                    let service = service.clone();
                    let sink = sink_tx.clone();
                    let id = request.id;
                    let body = request.request;
                    tokio::spawn(async move {
                        let answer =
                            into_rpc_message(handle_request(&*service, body, context).await, id);
                        let _ = sink.send(answer).await;
                    });
                }
                Event::Peer(ClientJsonRpcMessage::Notification(notification)) => {
                    if let ClientNotificationKind::Cancelled(cancelled) = &notification.kind {
                        if let Some(token) = cancelled
                            .request_id
                            .as_ref()
                            .and_then(|id| in_flight.remove(id))
                        {
                            token.cancel();
                        }
                    }
                    let service = service.clone();
                    let peer = peer.clone();
                    tokio::spawn(async move {
                        handle_notification(&*service, notification, peer).await;
                    });
                }
                // No server→client requests are ever outstanding, so client
                // responses and errors correlate with nothing.
                Event::Peer(ClientJsonRpcMessage::Response { .. })
                | Event::Peer(ClientJsonRpcMessage::Error { .. }) => {}
            }
        };
        // Deliver answers already produced (or produced shortly) before
        // closing; EOF waits longer than an explicit cancel.
        let drain = match quit {
            QuitReason::Closed => Duration::from_secs(5),
            QuitReason::Cancelled => Duration::from_secs(2),
        };
        drop(sink_tx);
        let _ = tokio::time::timeout(drain, async {
            while sends.join_next().await.is_some() {}
            while let Some(answer) = sink_rx.recv().await {
                if finish_answer(&mut in_flight, &answer) && transport.send(answer).await.is_err() {
                    break;
                }
            }
        })
        .await;
        loop_ct.cancel();
        let _ = transport.close().await;
        quit
    });
    RunningService {
        service,
        peer,
        handle: Some(handle),
        ct,
    }
}
