//! The MCP server session: lifecycle selection (`initialize` or per-request
//! metadata), request dispatch to a [`ServerHandler`], concurrent request
//! handling with cancellation, and the peer handle handlers use to notify
//! the client.
#![allow(deprecated)]

use super::message::*;
use super::model::*;
use super::transport::{IntoTransport, Transport};
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
#[derive(Clone)]
pub struct Peer {
    tx: mpsc::Sender<(ServerNotification, Responder)>,
    info: Arc<RwLock<Option<Arc<InitializeRequestParams>>>>,
    request_metadata_required: Arc<AtomicBool>,
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
    pub async fn notify_progress(
        &self,
        params: ProgressNotificationParam,
    ) -> Result<(), ServiceError> {
        self.send_notification(ServerNotification::new("notifications/progress", params))
            .await
    }
    pub async fn notify_logging_message(
        &self,
        params: LoggingMessageNotificationParam,
    ) -> Result<(), ServiceError> {
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
    pub async fn notify_resource_list_changed(&self) -> Result<(), ServiceError> {
        self.send_notification(ServerNotification::no_params(
            "notifications/resources/list_changed",
        ))
        .await
    }
    pub async fn notify_tool_list_changed(&self) -> Result<(), ServiceError> {
        self.send_notification(ServerNotification::no_params(
            "notifications/tools/list_changed",
        ))
        .await
    }
    pub async fn notify_prompt_list_changed(&self) -> Result<(), ServiceError> {
        self.send_notification(ServerNotification::no_params(
            "notifications/prompts/list_changed",
        ))
        .await
    }
}

/// Everything a request handler knows about the request it serves.
#[derive(Debug, Clone)]
pub struct RequestContext<R = RoleServer> {
    pub ct: CancellationToken,
    pub id: RequestId,
    pub meta: RequestMetaObject,
    pub peer: Peer,
    _role: std::marker::PhantomData<R>,
}

impl RequestContext<RoleServer> {
    pub fn new(id: RequestId, peer: Peer) -> Self {
        Self {
            ct: CancellationToken::new(),
            id,
            meta: RequestMetaObject::default(),
            peer,
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
    pub fn client_info(&self) -> Option<Implementation> {
        if self.peer.request_metadata_required() {
            self.meta.client_info()
        } else {
            self.meta
                .client_info()
                .or_else(|| self.peer.peer_info().map(|i| i.client_info.clone()))
        }
    }
    pub fn client_capabilities(&self) -> Option<ClientCapabilities> {
        if self.peer.request_metadata_required() {
            self.meta.client_capabilities()
        } else {
            self.meta
                .client_capabilities()
                .or_else(|| self.peer.peer_info().map(|i| i.capabilities.clone()))
        }
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
    pub async fn notify_tool_list_changed(&self) -> Result<(), SubscriptionSendError> {
        const M: &str = "notifications/tools/list_changed";
        let ok = self.accepted.tools_list_changed == Some(true);
        self.send(ServerNotification::no_params(M), ok, M).await
    }
    pub async fn notify_prompt_list_changed(&self) -> Result<(), SubscriptionSendError> {
        const M: &str = "notifications/prompts/list_changed";
        let ok = self.accepted.prompts_list_changed == Some(true);
        self.send(ServerNotification::no_params(M), ok, M).await
    }
    pub async fn notify_resource_list_changed(&self) -> Result<(), SubscriptionSendError> {
        const M: &str = "notifications/resources/list_changed";
        let ok = self.accepted.resources_list_changed == Some(true);
        self.send(ServerNotification::no_params(M), ok, M).await
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
        let notification = ServerNotification::new(M, ResourceUpdatedNotificationParam::new(uri));
        self.send(notification, ok, M).await
    }
}

/// One established `subscriptions/listen`; its acknowledgment is already sent.
pub struct SubscriptionContext {
    request: RequestContext,
    sink: SubscriptionSink,
    _active: DropGuard,
}

impl SubscriptionContext {
    async fn establish(
        request: RequestContext,
        accepted: SubscriptionFilter,
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
            _active: active.drop_guard(),
        })
    }
    pub fn sink(&self) -> &SubscriptionSink {
        &self.sink
    }
    pub fn accepted(&self) -> &SubscriptionFilter {
        &self.sink.accepted
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

fn tasks_gate<H: ServerHandler>(
    handler: &H,
    context: &RequestContext,
    method: &'static str,
) -> Result<(), ErrorData> {
    if !handler.get_info().capabilities.supports_tasks() {
        return Err(ErrorData::method_not_found(method));
    }
    if context
        .client_capabilities()
        .is_some_and(|c| c.supports_tasks())
    {
        Ok(())
    } else {
        Err(ErrorData::missing_required_client_capability(
            ClientCapabilities::builder().enable_tasks().build(),
        ))
    }
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
    if inline {
        if let Some(requested) = requested.as_ref() {
            let supported = handler.supported_protocol_versions();
            if !supported.contains(requested) {
                return Err(ErrorData::unsupported_protocol_version(
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
    let legacy = uses_legacy_lifecycle(protocol_version.as_ref(), requires_metadata);
    let result = match request {
        ClientRequest::Initialize(params) => handler
            .initialize(params, context)
            .await
            .map(ServerResult::Initialize),
        ClientRequest::Discover => handler.discover(context).await.map(ServerResult::Discover),
        ClientRequest::Ping if !legacy => Err(ErrorData::method_not_found("ping")),
        ClientRequest::Ping => handler.ping(context).await.map(|()| ServerResult::Empty),
        ClientRequest::Complete(params) => handler
            .complete(params, context)
            .await
            .map(ServerResult::Complete),
        ClientRequest::SetLevel(params) => handler
            .set_level(params, context)
            .await
            .map(|()| ServerResult::Empty),
        ClientRequest::GetPrompt(params) => handler
            .get_prompt(params, context)
            .await
            .map(ServerResult::from),
        ClientRequest::ListPrompts(params) => handler
            .list_prompts(params, context)
            .await
            .map(ServerResult::ListPrompts),
        ClientRequest::ListResources(params) => handler
            .list_resources(params, context)
            .await
            .map(ServerResult::ListResources),
        ClientRequest::ListResourceTemplates(params) => handler
            .list_resource_templates(params, context)
            .await
            .map(ServerResult::ListResourceTemplates),
        ClientRequest::ReadResource(params) => handler
            .read_resource(params, context)
            .await
            .map(ServerResult::from),
        ClientRequest::SubscriptionsListen(_) if legacy => {
            Err(ErrorData::method_not_found("subscriptions/listen"))
        }
        ClientRequest::SubscriptionsListen(params) => {
            let requested = params.notifications;
            let Some(candidate) = handler.accepted_subscription_filter(&requested) else {
                return Err(ErrorData::method_not_found("subscriptions/listen"));
            };
            let info = handler.get_info();
            let advertised = requested.supported_by(&info.capabilities);
            let accepted = requested.intersection(&candidate).intersection(&advertised);
            let id = context.id.clone();
            let subscription = SubscriptionContext::establish(context, accepted).await?;
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
        ClientRequest::Subscribe(_) if !legacy => {
            Err(ErrorData::method_not_found("resources/subscribe"))
        }
        ClientRequest::Subscribe(params) => handler
            .subscribe(params, context)
            .await
            .map(|()| ServerResult::Empty),
        ClientRequest::Unsubscribe(_) if !legacy => {
            Err(ErrorData::method_not_found("resources/unsubscribe"))
        }
        ClientRequest::Unsubscribe(params) => handler
            .unsubscribe(params, context)
            .await
            .map(|()| ServerResult::Empty),
        ClientRequest::CallTool(params) => {
            let declared_tasks = context
                .client_capabilities()
                .is_some_and(|c| c.supports_tasks());
            let response = handler.call_tool(params, context).await?;
            if matches!(response, CallToolResponse::Task(_)) && !declared_tasks {
                return Err(ErrorData::missing_required_client_capability(
                    ClientCapabilities::builder().enable_tasks().build(),
                ));
            }
            Ok(ServerResult::from(response))
        }
        ClientRequest::ListTools(params) => handler
            .list_tools(params, context)
            .await
            .map(ServerResult::ListTools),
        ClientRequest::Custom { method, .. } => {
            Err(ErrorData::new(ErrorCode::METHOD_NOT_FOUND, method, None))
        }
        ClientRequest::GetTask(params) => {
            tasks_gate(handler, &context, "tasks/get")?;
            handler
                .get_task(params, context)
                .await
                .map(ServerResult::GetTask)
        }
        ClientRequest::UpdateTask(params) => {
            tasks_gate(handler, &context, "tasks/update")?;
            handler
                .update_task(params, context)
                .await
                .map(|()| ServerResult::TaskAck)
        }
        ClientRequest::CancelTask(params) => {
            tasks_gate(handler, &context, "tasks/cancel")?;
            handler
                .cancel_task(params, context)
                .await
                .map(|()| ServerResult::TaskAck)
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

async fn establish<S: ServerHandler, T: Transport>(
    service: S,
    transport: &mut T,
    ct: CancellationToken,
) -> Result<(S, Peer, PeerRx, VecDeque<ClientJsonRpcMessage>), ServerInitializeError> {
    let (peer, peer_rx) = Peer::new(None);
    loop {
        let Some(message) = transport.receive().await else {
            return Err(ServerInitializeError::ConnectionClosed(
                "initialize request".to_string(),
            ));
        };
        let ClientJsonRpcMessage::Request(request) = message else {
            return Err(ServerInitializeError::ExpectedInitializeRequest);
        };
        let JsonRpcRequest {
            id,
            method,
            request,
            meta,
        } = request;
        match request {
            ClientRequest::Ping => {
                send_now(
                    transport,
                    ServerJsonRpcMessage::response(ServerResult::Empty, id),
                )
                .await?;
            }
            ClientRequest::Initialize(params) => {
                let requested = params.protocol_version.clone();
                let mut negotiated = params.clone();
                peer.set_peer_info(negotiated.clone());
                let context = RequestContext {
                    ct: ct.child_token(),
                    id: id.clone(),
                    meta,
                    peer: peer.clone(),
                    _role: std::marker::PhantomData,
                };
                let mut init = match handle_request(
                    &service,
                    ClientRequest::Initialize(params),
                    context,
                )
                .await
                {
                    Ok(ServerResult::Initialize(init)) => init,
                    Ok(_) => return Err(ServerInitializeError::ExpectedInitializeRequest),
                    Err(e) => {
                        send_now(transport, ServerJsonRpcMessage::error(e.clone(), Some(id)))
                            .await?;
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
                        send_now(transport, ServerJsonRpcMessage::error(e.clone(), Some(id)))
                            .await?;
                        return Err(ServerInitializeError::InitializeFailed(e));
                    }
                };
                negotiated.protocol_version = init.protocol_version.clone();
                peer.set_peer_info(negotiated);
                send_now(
                    transport,
                    ServerJsonRpcMessage::response(ServerResult::Initialize(init), id),
                )
                .await?;
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
                        send_now(
                            transport,
                            ServerJsonRpcMessage::error(missing_metadata_error(&missing), Some(id)),
                        )
                        .await?;
                        continue;
                    }
                };
                if matches!(request, ClientRequest::Discover) {
                    // No lifecycle yet: the probe's peer cannot reach the client.
                    let (bootstrap, _) = Peer::new(None);
                    let context = RequestContext {
                        ct: ct.child_token(),
                        id: id.clone(),
                        meta,
                        peer: bootstrap,
                        _role: std::marker::PhantomData,
                    };
                    let answer = match handle_request(&service, request, context).await {
                        Ok(result) => ServerJsonRpcMessage::response(result, id),
                        Err(error) => ServerJsonRpcMessage::error(error, Some(id)),
                    };
                    send_now(transport, answer).await?;
                    continue;
                }
                let supported = service.supported_protocol_versions();
                if !supported.contains(&version) {
                    send_now(
                        transport,
                        ServerJsonRpcMessage::error(
                            ErrorData::unsupported_protocol_version(version, &supported),
                            Some(id),
                        ),
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
    let loop_service = service.clone();
    let loop_peer = peer.clone();
    let loop_ct = ct.child_token();
    let handle = tokio::spawn(async move {
        let service = loop_service;
        let peer = loop_peer;
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
                    let Some(id) = answer.answered_id() else {
                        continue;
                    };
                    // A cancelled request's answer is dropped.
                    let Some(request_ct) = in_flight.remove(id) else {
                        continue;
                    };
                    request_ct.cancel();
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
                    let context = RequestContext {
                        ct: request_ct.child_token(),
                        id: request.id.clone(),
                        meta: request.meta,
                        peer: peer.clone(),
                        _role: std::marker::PhantomData,
                    };
                    in_flight.insert(request.id.clone(), request_ct);
                    let service = service.clone();
                    let sink = sink_tx.clone();
                    let id = request.id;
                    let body = request.request;
                    tokio::spawn(async move {
                        let answer = match handle_request(&*service, body, context).await {
                            Ok(result) => ServerJsonRpcMessage::response(result, id),
                            Err(error) => ServerJsonRpcMessage::error(error, Some(id)),
                        };
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
                let Some(id) = answer.answered_id() else {
                    continue;
                };
                let Some(request_ct) = in_flight.remove(id) else {
                    continue;
                };
                request_ct.cancel();
                if transport.send(answer).await.is_err() {
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
