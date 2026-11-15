//! MCP application handlers. Wire validation, negotiation, framing, cancellation,
//! request correlation and version-specific serialization belong to `crate::mcp`.
#![allow(deprecated)] // Legacy MCP compatibility during the protocol deprecation window.
use crate::mcp::{
    model::*,
    service::{
        NotificationContext, RequestContext, RoleServer, SubscriptionContext, SubscriptionSendError,
    },
    ErrorData, ServerHandler,
};
use crate::tools::ToolRegistry;
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use susi_paths::sync::RwLock;
use tokio::sync::broadcast;

pub type ProtocolFuture<T> = Pin<Box<dyn Future<Output = Result<T, ErrorData>> + Send>>;
/// Providers can use the full typed response (including multimodal content,
/// structured output, MRTR elicitation/sampling/roots, and task handles).
pub type ToolHandler = Arc<
    dyn Fn(CallToolRequestParams, RequestContext<RoleServer>) -> ProtocolFuture<CallToolResponse>
        + Send
        + Sync,
>;
/// Next stage in a composable [`ToolMiddleware`] chain.
pub type ToolNext = ToolHandler;
/// Composable outer-to-inner tool middleware executed after `jsonrpc_gate` and
/// compiled `inputSchema` validation and before `outputSchema` enforcement.
pub type ToolMiddleware = Arc<
    dyn Fn(
            CallToolRequestParams,
            RequestContext<RoleServer>,
            ToolNext,
        ) -> ProtocolFuture<CallToolResponse>
        + Send
        + Sync,
>;
/// Largest payload one `resources/read` serves; a bigger read is refused, never truncated.
pub const MAX_RESOURCE_BYTES: usize = 8 * 1024 * 1024;
/// Access control run before a read or subscribe reaches its provider.
pub type ResourceGuard = Arc<dyn Fn(&str) -> Result<(), ErrorData> + Send + Sync>;
pub type ResourceHandler = Arc<
    dyn Fn(
            ReadResourceRequestParams,
            RequestContext<RoleServer>,
        ) -> ProtocolFuture<ReadResourceResponse>
        + Send
        + Sync,
>;
pub type PromptHandler = Arc<
    dyn Fn(GetPromptRequestParams, RequestContext<RoleServer>) -> ProtocolFuture<GetPromptResponse>
        + Send
        + Sync,
>;
pub type TaskToolHandler = Arc<
    dyn Fn(
            CallToolRequestParams,
            crate::mcp::task_manager::TaskContext,
        ) -> crate::mcp::task_manager::TaskFuture
        + Send
        + Sync,
>;
pub type CompletionHandler = Arc<
    dyn Fn(CompleteRequestParams, RequestContext<RoleServer>) -> ProtocolFuture<CompleteResult>
        + Send
        + Sync,
>;

/// Revisions a stateless request may declare in
/// `_meta["io.modelcontextprotocol/protocolVersion"]` — the modern era
/// plus its inline-compatible legacy revisions. This is the canonical
/// registry: the `data.supported` list every `-32022` answer carries in
/// `jsonrpc_gate`, and the per-request validation set for the stateless
/// lifecycle contract (contract-203-003).
pub const MODERN_REQUEST_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2026-07-28"];

/// Cacheability baseline the `server/discover` result advertises:
/// immediately stale, never shared across authorization contexts.
/// contract-203-003 pins both values; a later positive `ttlMs` change
/// must revisit `cacheScope` deliberately.
pub const DISCOVER_TTL_MS: u64 = 0;
pub const DISCOVER_CACHE_SCOPE: CacheScope = CacheScope::Private;

#[derive(Clone, Debug)]
pub(crate) enum Change {
    Tools,
    Resources,
    Prompts,
    Resource(String),
}

/// MCP Gateway Proxy: The control plane intercepts all MCP requests
/// (from local or external BYOA agents) to enforce capability tokens and policies.
pub struct McpGatewayProxy;
impl McpGatewayProxy {
    pub fn evaluate_tool_policy(tool_name: &str) -> Result<(), String> {
        // In Phase 1, we lay the foundation for capability checks.
        // If a tool name requires high privilege (e.g., restricted ops),
        // we assert the presence of a minted CapabilityToken (simulated here).
        if tool_name.starts_with("susi_restricted_") || tool_name.starts_with("system_") {
            return Err(format!("Agent lacks CapabilityToken for privileged tool '{}'. Please request a token from the Capability Mint Agent.", tool_name));
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(crate) struct RegisteredTool {
    pub(crate) definition: Tool,
    pub(crate) handler: ToolHandler,
    pub(crate) input: Arc<crate::schema_check::Validator>,
    pub(crate) output: Option<Arc<crate::schema_check::Validator>>,
}

#[derive(Default)]
pub(crate) struct Catalog {
    pub(crate) tools: BTreeMap<String, RegisteredTool>,
    pub(crate) resources: BTreeMap<String, (Resource, ResourceHandler)>,
    pub(crate) templates: BTreeMap<String, (ResourceTemplate, ResourceHandler)>,
    pub(crate) prompts: BTreeMap<String, (Prompt, PromptHandler)>,
    pub(crate) completions: BTreeMap<String, CompletionHandler>,
    pub(crate) resource_guard: Option<ResourceGuard>,
}

pub use crate::mcp::service::{
    MAX_ACTIVE_SUBSCRIPTIONS_PER_SESSION, MAX_LOG_NOTIFICATIONS_PER_REQUEST,
    MAX_NOTIFICATION_PAYLOAD_BYTES, MAX_RESOURCE_SUBSCRIPTIONS_PER_FILTER,
};

#[derive(Clone)]
pub struct GmcpService {
    pub(crate) workspace: PathBuf,
    pub(crate) server_info: Implementation,
    pub(crate) instructions: Option<String>,
    pub(crate) include_host_tools: bool,
    pub(crate) catalog: Arc<RwLock<Catalog>>,
    pub(crate) changes: broadcast::Sender<Change>,
    pub tasks: crate::mcp::task_manager::TaskManager,
    pub(crate) legacy_subscriptions: Arc<RwLock<BTreeSet<String>>>,
    pub(crate) log_level: Arc<RwLock<Option<LoggingLevel>>>,
    pub(crate) extensions: Arc<RwLock<crate::extensions::ExtensionRegistry>>,
    pub(crate) middlewares: Arc<RwLock<Vec<ToolMiddleware>>>,
    pub(crate) plugins: Arc<RwLock<BTreeMap<String, crate::sdk::LoadedPlugin>>>,
}

impl GmcpService {
    /// Workspace root bound to this service instance.
    pub fn workspace(&self) -> &std::path::Path {
        &self.workspace
    }
    /// Append a composable tool middleware in deterministic outer-to-inner order.
    pub fn register_middleware(&self, middleware: ToolMiddleware) {
        self.middlewares.write().push(middleware);
    }
    /// The extension registry that drives advertised capabilities.
    pub fn extensions(&self) -> Arc<RwLock<crate::extensions::ExtensionRegistry>> {
        self.extensions.clone()
    }
    pub fn new(workspace: PathBuf) -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            workspace,
            server_info: Self::server_identity(),
            instructions: None,
            include_host_tools: true,
            catalog: Default::default(),
            changes,
            tasks: Default::default(),
            legacy_subscriptions: Default::default(),
            log_level: Default::default(),
            extensions: Arc::new(RwLock::new(crate::extensions::ExtensionRegistry::shipped())),
            middlewares: Default::default(),
            plugins: Default::default(),
        }
    }
    /// Construct a service backed by a durable disk-persisted task store (fail-closed if corrupt).
    pub fn open_durable(workspace: PathBuf, store_dir: PathBuf) -> Result<Self, ErrorData> {
        let (changes, _) = broadcast::channel(256);
        Ok(Self {
            workspace,
            server_info: Self::server_identity(),
            instructions: None,
            include_host_tools: true,
            catalog: Default::default(),
            changes,
            tasks: crate::mcp::task_manager::TaskManager::open(store_dir)?,
            legacy_subscriptions: Default::default(),
            log_level: Default::default(),
            extensions: Arc::new(RwLock::new(crate::extensions::ExtensionRegistry::shipped())),
            middlewares: Default::default(),
            plugins: Default::default(),
        })
    }
    /// Isolate legacy session preferences while sharing the application catalog.
    pub fn session(&self) -> Self {
        Self {
            legacy_subscriptions: Default::default(),
            log_level: Default::default(),
            ..self.clone()
        }
    }
    /// Canonical `serverInfo` value advertised in `server/discover`.
    pub fn server_identity() -> Implementation {
        Implementation::new("susi-gmcp", env!("CARGO_PKG_VERSION"))
    }
    /// Cacheable `server/discover` payload pinned by VC-203-003.
    pub fn discover_result(&self) -> DiscoverResult {
        DiscoverResult::from_server_info(ProtocolVersion::KNOWN_VERSIONS.to_vec(), self.get_info())
            .with_ttl_ms(DISCOVER_TTL_MS)
            .with_cache_scope(DISCOVER_CACHE_SCOPE)
    }
    pub fn register_tool(&self, tool: Tool, handler: ToolHandler) -> Result<(), ErrorData> {
        let input = compile_schema(&Value::Object((*tool.input_schema).clone()))?;
        let output = compile_opt_schema(tool.output_schema.as_ref())?;
        self.catalog.write().tools.insert(
            tool.name.to_string(),
            RegisteredTool {
                definition: tool,
                handler,
                input,
                output,
            },
        );
        let _ = self.changes.send(Change::Tools);
        Ok(())
    }
    pub fn register_task_tool(
        &self,
        tool: Tool,
        handler: TaskToolHandler,
    ) -> Result<(), ErrorData> {
        let (tasks, output) = (
            self.tasks.clone(),
            compile_opt_schema(tool.output_schema.as_ref())?,
        );
        self.register_tool(
            tool,
            Arc::new(move |request, context| {
                let (tasks, handler, output) = (tasks.clone(), handler.clone(), output.clone());
                Box::pin(async move {
                    if !context
                        .client_capabilities()
                        .is_some_and(|c| c.supports_tasks())
                    {
                        return Err(ErrorData::missing_required_client_capability(
                            ClientCapabilities::builder().enable_tasks().build(),
                        ));
                    }
                    let progress_lease = context.progress_lease();
                    let owner = context.principal().clone();
                    let task = tasks.try_spawn_for(owner, Default::default(), move |context| {
                        let progress_lease = progress_lease.clone();
                        Box::pin(async move {
                            let res = finish_task_run(&context, &progress_lease, None, async {
                                handler(request, context.clone()).await
                            });
                            let out = res.await?;
                            Ok(enforce_output_schema(
                                output.as_deref(),
                                out,
                                "Task output does not match outputSchema",
                            ))
                        })
                    })?;
                    Ok(CreateTaskResult::new(task).into())
                })
            }),
        )
    }
    pub fn register_resource(&self, resource: Resource, handler: ResourceHandler) {
        let uri = resource.uri.clone();
        let replaced = self
            .catalog
            .write()
            .resources
            .insert(uri.clone(), (resource, handler))
            .is_some();
        let _ = self.changes.send(Change::Resources);
        if replaced {
            self.resource_updated(uri);
        }
    }
    pub fn register_template(&self, template: ResourceTemplate, handler: ResourceHandler) {
        self.catalog
            .write()
            .templates
            .insert(template.uri_template.clone(), (template, handler));
        let _ = self.changes.send(Change::Resources);
    }
    pub fn register_prompt(&self, prompt: Prompt, handler: PromptHandler) {
        self.catalog
            .write()
            .prompts
            .insert(prompt.name.clone(), (prompt, handler));
        let _ = self.changes.send(Change::Prompts);
    }
    /// Key is the prompt name or resource-template URI supplied in completion/ref.
    pub fn register_completion(&self, key: String, handler: CompletionHandler) {
        self.catalog.write().completions.insert(key, handler);
    }
    pub fn resource_updated(&self, uri: String) {
        let _ = self.changes.send(Change::Resource(uri));
    }
    pub fn set_resource_guard(&self, guard: ResourceGuard) {
        self.catalog.write().resource_guard = Some(guard);
    }
    /// First step of every resource operation: a bounded absolute URI, no `..` segment, the guard.
    fn admit_resource(&self, uri: &str) -> Result<(), ErrorData> {
        let plain = uri.to_ascii_lowercase().replace("%2e", ".");
        let plain = plain.replace("%2f", "/").replace("%5c", "/");
        let traversal = plain.split(['/', '\\', '?', '#']).any(|part| part == "..");
        let shaped = uri.len() <= 2048 && !uri.contains(char::is_whitespace);
        if traversal || !shaped || !crate::content::is_valid_uri(uri) {
            return Err(invalid("Invalid resource URI"));
        }
        let guard = self.catalog.read().resource_guard.clone();
        guard.map_or(Ok(()), |guard| guard(uri))
    }
    pub(crate) fn tools(&self) -> Result<Vec<Tool>, ErrorData> {
        let mut tools = BTreeMap::new();
        if self.include_host_tools {
            for tool in ToolRegistry::list_tools() {
                // Built-in handlers carry the typed schema their `#[tool]`
                // declares; dynamically discovered proxies (external
                // `server:tool`, reflexes) forward arguments unchanged and say so
                // (their own schema is dropped at discovery: `args::passthrough`).
                let input_schema = crate::tools::args::core_input_schema(&tool.name)
                    .unwrap_or_else(crate::tools::args::passthrough);
                let definition = Tool::new_with_raw(
                    tool.name.clone(),
                    Some(tool.description.into()),
                    input_schema,
                );
                tools.insert(tool.name, definition);
            }
        }
        for (name, tool) in &self.catalog.read().tools {
            tools.insert(name.clone(), tool.definition.clone());
        }
        Ok(tools.into_values().collect())
    }
}

pub(crate) fn compile_opt_schema(
    schema: Option<&Arc<JsonObject>>,
) -> Result<Option<Arc<crate::schema_check::Validator>>, ErrorData> {
    schema
        .map(|s| compile_schema(&Value::Object((**s).clone())))
        .transpose()
}

pub(crate) async fn finish_task_run<F>(
    task: &crate::mcp::task_manager::TaskContext,
    lease: &Option<Arc<crate::mcp::service::ProgressLease>>,
    flag: Option<Arc<AtomicBool>>,
    fut: F,
) -> Result<CallToolResult, crate::mcp::task_manager::TaskExit>
where
    F: Future<Output = Result<CallToolResult, crate::mcp::task_manager::TaskExit>>,
{
    let watch = (flag.is_some() || lease.is_some()).then(|| {
        let (task, lease) = (task.clone(), lease.clone());
        tokio::spawn(async move {
            task.cancelled().await;
            if let Some(flag) = flag {
                flag.store(true, Ordering::Release);
            }
            if let Some(lease) = lease {
                lease.stop();
            }
        })
    });
    let result = fut.await;
    if let Some(w) = watch {
        w.abort();
    }
    let result = result?;
    if task.is_cancel_requested() {
        return Err(crate::mcp::task_manager::TaskExit::Cancelled);
    }
    if result.is_error != Some(true) {
        if let Some(lease) = lease {
            let _ = lease.notify_edge(1.0, None).await;
        }
    }
    Ok(result)
}

pub(crate) fn compile_schema(
    schema: &Value,
) -> Result<Arc<crate::schema_check::Validator>, ErrorData> {
    fn bounded(value: &Value, depth: usize, remaining: &mut usize) -> bool {
        if depth > 64 || *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match value {
            Value::Object(map) => map.values().all(|v| bounded(v, depth + 1, remaining)),
            Value::Array(array) => array.iter().all(|v| bounded(v, depth + 1, remaining)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
        }
    }
    if !bounded(schema, 0, &mut 10000) {
        return Err(invalid("Schema exceeds complexity limit"));
    }
    // No network or file retrieval: only local `$defs`/`$ref`; providers
    // bundle external references. Unsupported keywords fail here, at
    // registration (see `schema_check`).
    crate::schema_check::Validator::compile(schema)
        .map(Arc::new)
        .map_err(|e| invalid(&format!("Invalid tool schema: {e}")))
}

pub(crate) fn internal(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}
pub(crate) fn invalid(message: &str) -> ErrorData {
    ErrorData::invalid_params(message.to_owned(), None)
}

/// Cursors are bound to the complete catalog snapshot, preventing silent skips
/// when a dynamic catalog changes between pages. Clients then restart listing.
pub(crate) fn page<T: serde::Serialize + Clone>(
    items: Vec<T>,
    request: Option<PaginatedRequestParams>,
) -> Result<(Vec<T>, Option<String>), ErrorData> {
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(&items)
        .map_err(internal)?
        .hash(&mut hash);
    let revision = hash.finish();
    let offset = match request.and_then(|r| r.cursor) {
        None => 0,
        Some(cursor) => {
            let (generation, offset) = cursor
                .split_once(':')
                .ok_or_else(|| invalid("Invalid cursor"))?;
            if generation != revision.to_string() {
                return Err(invalid("Catalog changed; restart listing without a cursor"));
            }
            let offset = offset
                .parse::<usize>()
                .map_err(|_| invalid("Invalid cursor"))?;
            if offset == 0 || offset >= items.len() {
                return Err(invalid("Invalid cursor"));
            }
            offset
        }
    };
    let end = offset.saturating_add(100).min(items.len());
    Ok((
        items[offset..end].to_vec(),
        (end < items.len()).then(|| format!("{revision}:{end}")),
    ))
}

fn page_entries<T: serde::Serialize + Clone, H>(
    map: &BTreeMap<String, (T, H)>,
    request: Option<PaginatedRequestParams>,
) -> Result<(Vec<T>, Option<String>), ErrorData> {
    page(map.values().map(|(v, _)| v.clone()).collect(), request)
}

impl ServerHandler for GmcpService {
    /// The advertised registry — every revision this server implements.
    /// Pinned explicitly so the `server/discover` payload and the
    /// negotiated-legacy path share one source of truth.
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::KNOWN_VERSIONS)
    }
    /// `server/discover`: the stateless probe every client may invoke
    /// without a handshake. Serves the canonical result built by
    /// `discover_result` — no session lookup, no negotiated state.
    async fn discover(
        &self,
        _context: RequestContext<RoleServer>,
    ) -> Result<DiscoverResult, ErrorData> {
        Ok(self.discover_result())
    }
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig {
            server_info: self.server_info.clone(),
            instructions: self.instructions.clone(),
            ..ServerConfig::default()
        };
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_tool_list_changed()
            .enable_resources()
            .enable_resources_list_changed()
            .enable_resources_subscribe()
            .enable_prompts()
            .enable_prompts_list_changed()
            .enable_completions()
            .enable_logging()
            .build();
        // Extensions come from the one typed registry, never ad-hoc.
        let map = self.extensions.read().capability_map();
        info.capabilities.extensions = if map.is_empty() { None } else { Some(map) };
        info
    }
    async fn set_level(
        &self,
        request: SetLevelRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        *self.log_level.write() = Some(request.level);
        Ok(())
    }
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.admit_resource(&request.uri)?;
        if !self.catalog.read().resources.contains_key(&request.uri) {
            return Err(ErrorData::resource_not_found("Unknown resource", None));
        }
        self.legacy_subscriptions.write().insert(request.uri);
        Ok(())
    }
    async fn unsubscribe(
        &self,
        request: UnsubscribeRequestParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.legacy_subscriptions.write().remove(&request.uri);
        Ok(())
    }
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let (service, mut changes) = (self.clone(), self.changes.subscribe());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = tick.tick() => if context.peer.is_transport_closed() { break; },
                    change = changes.recv() => {
                        let Ok(change) = change else { break; };
                        let res = match change {
                            Change::Tools => context.peer.notify_tool_list_changed().await,
                            Change::Resources => context.peer.notify_resource_list_changed().await,
                            Change::Prompts => context.peer.notify_prompt_list_changed().await,
                            Change::Resource(uri) => {
                                if service.admit_resource(&uri).is_err()
                                    || !service.legacy_subscriptions.read().contains(&uri)
                                {
                                    continue;
                                }
                                context.peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri)).await
                            }
                        };
                        if res.is_err() { break; }
                    }
                }
            }
        });
    }
    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let (items, next) = page(self.tools()?, request)?;
        Ok(ListToolsResult::paged(items, next))
    }
    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools().ok()?.into_iter().find(|t| t.name == name)
    }
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        // --- PHASE 1: MCP GATEWAY PROXY ---
        // Intercepts tool calls to enforce SUSI Swarm OS capability tokens and governance policy.
        if let Err(violation) = McpGatewayProxy::evaluate_tool_policy(&request.name) {
            return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "Governance Violation: {}",
                violation
            ))])
            .into());
        }
        // --- END GATEWAY ---
        let registered = self
            .catalog
            .read()
            .tools
            .get(request.name.as_ref())
            .cloned();
        if let Some(tool) = registered {
            let arguments = Value::Object(request.arguments.clone().unwrap_or_default());
            if !tool.input.is_valid(&arguments) {
                return Ok(CallToolResult::error(vec![ContentBlock::text(
                    "Arguments do not match inputSchema",
                )])
                .into());
            }
            let log_ctx = context.clone();
            let tool_name = request.name.to_string();
            let middlewares = self.middlewares.read().clone();
            let mut dispatch: ToolNext = tool.handler.clone();
            for mw in middlewares.into_iter().rev() {
                let inner = dispatch;
                dispatch = Arc::new(move |req, ctx| mw(req, ctx, inner.clone()));
            }
            let mut response = dispatch(request, context).await?;
            if let CallToolResponse::Complete(res) = response {
                let mut checked = enforce_output_schema(
                    tool.output.as_deref(),
                    res,
                    "Tool output does not match outputSchema",
                );
                checked.result_type = Some(ResultType::COMPLETE);
                response = checked.into();
            }
            emit_tool_completion_log(self, &log_ctx, &tool_name, &response).await?;
            return Ok(response);
        }
        if !self.tools()?.iter().any(|t| t.name == request.name) {
            return Err(invalid("Unknown tool"));
        }
        let workspace = self.workspace.clone();
        let name = request.name.into_owned();
        let args = Value::Object(request.arguments.unwrap_or_default());
        let progress_lease = match context.progress_lease() {
            Some(lease) => Some(lease),
            None => context
                .meta
                .parse_progress_token()?
                .map(|t| context.peer.enter_progress(t).map(Arc::new))
                .transpose()?,
        };
        if self.get_info().capabilities.supports_tasks()
            && context
                .client_capabilities()
                .is_some_and(|c| c.supports_tasks())
            && context
                .protocol_version()
                .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28)
        {
            use crate::mcp::task_manager::TaskExit;
            let (task_name, task_lease) = (name.clone(), progress_lease.clone());
            let owner = context.principal().clone();
            let task = self
                .tasks
                .try_spawn_for(owner, Default::default(), move |task| {
                    let progress_lease = task_lease.clone();
                    Box::pin(async move {
                        if task.is_cancel_requested() {
                            return Err(TaskExit::Cancelled);
                        }
                        let cancel = Some(Arc::new(AtomicBool::new(task.is_cancel_requested())));
                        let run = run_watched(
                            progress_lease.as_deref(),
                            (task_name, args),
                            workspace,
                            cancel.clone(),
                            false,
                        );
                        finish_task_run(&task, &progress_lease, cancel, async move {
                            run.await.map_err(TaskExit::from)
                        })
                        .await
                    })
                })?;
            let response = CallToolResponse::Task(CreateTaskResult::new(task));
            emit_tool_completion_log(self, &context, &name, &response).await?;
            return Ok(response);
        }
        let (cancel, cancel_watch) = if name == "mission_run" {
            let flag = Arc::new(AtomicBool::new(context.ct.is_cancelled()));
            let (observed, ct) = (flag.clone(), context.ct.clone());
            let watch = tokio::spawn(async move {
                ct.cancelled().await;
                observed.store(true, Ordering::Release);
            });
            (Some(flag), Some(watch))
        } else {
            (None, None)
        };
        let result = run_watched(
            progress_lease.as_deref(),
            (name.clone(), args),
            workspace,
            cancel,
            true,
        )
        .await;
        if let Some(watch) = cancel_watch {
            watch.abort();
        }
        let result = result?;
        if result.is_error != Some(true) && !context.ct.is_cancelled() {
            if let Some(lease) = progress_lease.as_ref() {
                lease
                    .notify_edge(1.0, Some("Tool finished"))
                    .await
                    .map_err(internal)?;
            }
        }
        let response: CallToolResponse = result.into();
        emit_tool_completion_log(self, &context, &name, &response).await?;
        Ok(response)
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let (items, next) = page_entries(&self.catalog.read().resources, request)?;
        Ok(ListResourcesResult::paged(items, next))
    }
    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let (items, next) = page_entries(&self.catalog.read().templates, request)?;
        Ok(ListResourceTemplatesResult::paged(items, next))
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        self.admit_resource(&request.uri)?;
        let read = async {
            // Template providers own URI matching and return RESOURCE_NOT_FOUND for a miss.
            let handlers: Vec<_> = {
                let catalog = self.catalog.read();
                match catalog.resources.get(&request.uri) {
                    Some((_, handler)) => vec![handler.clone()],
                    None => catalog.templates.values().map(|(_, h)| h.clone()).collect(),
                }
            };
            for handler in handlers {
                match handler(request.clone(), context.clone()).await {
                    Err(e) if e.code == ErrorCode::RESOURCE_NOT_FOUND => continue,
                    result => return result.and_then(seal_resource),
                }
            }
            Err(ErrorData::resource_not_found(
                "Unknown resource",
                Some(json!({"uri":request.uri})),
            ))
        };
        tokio::select! {
            result = read => result,
            () = context.ct.cancelled() => Err(internal("Resource read cancelled")),
        }
    }
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let (items, next) = page_entries(&self.catalog.read().prompts, request)?;
        Ok(ListPromptsResult::paged(items, next))
    }
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let entry = self
            .catalog
            .read()
            .prompts
            .get(&request.name)
            .cloned()
            .ok_or_else(|| invalid("Unknown prompt"))?;
        for argument in entry.0.arguments.unwrap_or_default() {
            if argument.required == Some(true)
                && !request
                    .arguments
                    .as_ref()
                    .is_some_and(|a| a.contains_key(&argument.name))
            {
                return Err(invalid("Missing required prompt argument"));
            }
        }
        let mut response = entry.1(request, context).await?;
        if let GetPromptResponse::Complete(result) = &mut response {
            result.result_type = Some(ResultType::COMPLETE);
        }
        Ok(response)
    }
    async fn complete(
        &self,
        request: CompleteRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CompleteResult, ErrorData> {
        let key = match &request.r#ref {
            Reference::Prompt(p) => &p.name,
            Reference::Resource(r) => &r.uri,
        };
        let handler = self
            .catalog
            .read()
            .completions
            .get(key)
            .cloned()
            .ok_or_else(|| invalid("Unknown completion reference"))?;
        let mut result = handler(request, context).await?;
        result.result_type = Some(ResultType::COMPLETE);
        if result.completion.values.len() > 100 {
            result.completion.values.truncate(100);
            result.completion.has_more = Some(true);
        }
        Ok(result)
    }
    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
        context: &RequestContext<RoleServer>,
    ) -> Result<Option<SubscriptionFilter>, ErrorData> {
        let mut filter = requested.clone();
        if let Some(uris) = filter.resource_subscriptions.take() {
            let mut seen = BTreeSet::new();
            let admitted: Vec<_> = uris
                .into_iter()
                .filter(|u| self.admit_resource(u).is_ok() && seen.insert(u.clone()))
                .collect();
            filter.resource_subscriptions = (!admitted.is_empty()).then_some(admitted);
        }
        if let Some(ids) = requested.task_ids.as_deref() {
            let admitted = self.tasks.authorize_tasks_for(context.principal(), ids)?;
            filter.task_ids = (!admitted.is_empty()).then_some(admitted);
        }
        Ok(Some(filter))
    }
    fn prepare_subscription(&self) -> Box<dyn std::any::Any + Send> {
        Box::new((self.changes.subscribe(), self.tasks.subscribe_events()))
    }
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        use broadcast::error::RecvError::{Closed, Lagged};
        type SubState = (
            broadcast::Receiver<Change>,
            broadcast::Receiver<(crate::mcp::task_manager::TaskPrincipal, DetailedTask)>,
        );
        fn keep_going(r: Result<(), SubscriptionSendError>) -> Result<bool, ErrorData> {
            match r {
                Ok(()) | Err(SubscriptionSendError::NotificationNotAccepted(_)) => Ok(true),
                Err(SubscriptionSendError::SubscriptionClosed) => Ok(false),
                Err(e) => Err(internal(e)),
            }
        }
        let (mut changes, mut task_events) = context
            .take_prepared::<SubState>()
            .unwrap_or_else(|| (self.changes.subscribe(), self.tasks.subscribe_events()));
        let sink = context.sink();
        if let Some(ids) = &context.accepted().task_ids {
            for snap in self.tasks.snapshots_for(context.principal(), ids) {
                if snap.status().is_terminal() && !keep_going(sink.notify_task_status(&snap).await)?
                {
                    return Ok(());
                }
            }
        }
        loop {
            let result = tokio::select! {
                _ = context.cancelled() => return Ok(()),
                msg = changes.recv() => match msg {
                    Ok(Change::Tools) => sink.notify_tool_list_changed().await,
                    Ok(Change::Prompts) => sink.notify_prompt_list_changed().await,
                    Ok(Change::Resources) => sink.notify_resource_list_changed().await,
                    Ok(Change::Resource(uri)) if self.admit_resource(&uri).is_ok() => {
                        sink.notify_resource_updated(uri).await
                    }
                    Ok(Change::Resource(_)) => continue,
                    Err(Closed) => return Ok(()),
                    Err(Lagged(n)) => return Err(internal(format!("Subscription lagged ({n} events dropped)"))),
                },
                ev = task_events.recv() => match ev {
                    Ok((owner, task)) if owner == *context.principal() => sink.notify_task_status(&task).await,
                    Ok(_) => continue,
                    Err(Closed) => return Ok(()),
                    Err(Lagged(n)) => return Err(internal(format!("Task subscription lagged ({n} events dropped)"))),
                }
            };
            if !keep_going(result)? {
                return Ok(());
            }
        }
    }
    async fn get_task(
        &self,
        request: GetTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        self.tasks
            .get_task_for(&request.task_id, context.principal())
            .map(GetTaskResult::new)
    }
    async fn update_task(
        &self,
        request: UpdateTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks.update_task_for(
            &request.task_id,
            context.principal(),
            request.input_responses,
        )
    }
    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks
            .cancel_task_for(&request.task_id, context.principal())
    }
}

/// Seal a provider's read: `contents` never empty (ambiguous with a missing resource),
/// strict base64 blobs, well-formed MIME types, at most [`MAX_RESOURCE_BYTES`]; then cache hints.
fn seal_resource(mut response: ReadResourceResponse) -> Result<ReadResourceResponse, ErrorData> {
    if let ReadResourceResponse::Complete(result) = &mut response {
        let (mut bytes, mut sound) = (0, !result.contents.is_empty());
        for content in &result.contents {
            let (m, b, binary) = match content {
                ResourceContents::TextResourceContents {
                    mime_type: m,
                    text: b,
                    ..
                } => (m, b, false),
                ResourceContents::BlobResourceContents {
                    mime_type: m,
                    blob: b,
                    ..
                } => (m, b, true),
            };
            bytes += b.len();
            sound &= m.as_deref().is_none_or(crate::content::is_valid_mime_type)
                && (!binary || crate::content::is_valid_base64(b));
        }
        if !sound || bytes > MAX_RESOURCE_BYTES {
            return Err(internal("Resource read is empty, malformed or oversized"));
        }
        result.result_type = Some(ResultType::COMPLETE);
        result.ttl_ms.get_or_insert(0);
        result.cache_scope.get_or_insert(CacheScope::Private);
    }
    Ok(response)
}

/// Least gap between relayed progress notifications: at most about ten a
/// second. A line that arrives sooner waits, and a newer one replaces it.
const PROGRESS_GAP: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) fn enforce_output_schema(
    schema: Option<&crate::schema_check::Validator>,
    result: CallToolResult,
    msg: &str,
) -> CallToolResult {
    if let Some(s) = schema {
        if result.is_error != Some(true)
            && !result
                .structured_content
                .as_ref()
                .is_some_and(|v| s.is_valid(v))
        {
            return CallToolResult::error(vec![ContentBlock::text(msg)]);
        }
    }
    result
}

async fn run_watched(
    lease: Option<&crate::mcp::service::ProgressLease>,
    (name, args): (String, Value),
    workspace: PathBuf,
    cancel: Option<crate::susi_core::progress::Cancel>,
    propagate_edge_err: bool,
) -> Result<CallToolResult, ErrorData> {
    let Some(lease) = lease else {
        return execute_registry(name, args, workspace, None, cancel).await;
    };
    let start = lease.notify_edge(0.0, Some("Tool started")).await;
    if propagate_edge_err {
        start.map_err(internal)?;
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let sink: crate::susi_core::progress::Sink = Arc::new(move |l: &str| {
        let _ = tx.send(l.to_string());
    });
    let relay = tokio::spawn(relay_progress(
        lease.peer.clone(),
        lease.token().clone(),
        rx,
        lease.stop_token(),
    ));
    let res = execute_registry(name, args, workspace, Some(sink), cancel).await;
    let _ = relay.await;
    res
}

async fn emit_tool_completion_log(
    service: &GmcpService,
    ctx: &RequestContext<RoleServer>,
    tool: &str,
    res: &CallToolResponse,
) -> Result<(), ErrorData> {
    let threshold = if ctx
        .protocol_version()
        .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28)
    {
        ctx.meta.log_level()
    } else {
        *service.log_level.read()
    };
    let (level, data) = if let CallToolResponse::Complete(r) = res {
        if r.is_error == Some(true) {
            let err: Vec<_> = r
                .content
                .iter()
                .filter_map(|b| b.as_text().map(|t| t.text.as_str()))
                .collect();
            (
                LoggingLevel::Error,
                json!({"tool": tool, "error": err.join(" ")}),
            )
        } else {
            (LoggingLevel::Info, json!("Tool finished"))
        }
    } else {
        (LoggingLevel::Info, json!("Tool finished"))
    };
    ctx.emit_logging_message(
        threshold,
        level,
        Some(service.server_info.name.clone()),
        data,
    )
    .await
    .map(|_| ())
    .map_err(internal)
}

/// Relay a watched tool's progress lines strictly between 0.0 and 1.0,
/// coalesced to [`PROGRESS_GAP`], stopping immediately on cancellation.
async fn relay_progress(
    peer: crate::mcp::service::Peer,
    token: ProgressToken,
    mut lines: tokio::sync::mpsc::UnboundedReceiver<String>,
    stop: crate::mcp::service::CancellationToken,
) {
    let (mut sent, mut last, mut pending) = (0u32, None::<tokio::time::Instant>, None::<String>);
    loop {
        if stop.is_cancelled() {
            return;
        }
        let next = tokio::select! {
            _ = stop.cancelled() => return,
            r = async {
                if let (Some(_), Some(at)) = (&pending, last) {
                    tokio::time::timeout_at(at + PROGRESS_GAP, lines.recv())
                        .await
                        .map_or(Some(None), |n| n.map(Some))
                } else {
                    lines.recv().await.map(Some)
                }
            } => r,
        };
        let closed = next.is_none();
        if let Some(Some(line)) = next {
            pending = Some(line);
        }
        if closed || last.is_none_or(|at| at.elapsed() >= PROGRESS_GAP) {
            if let Some(line) = pending.take() {
                if stop.is_cancelled() {
                    return;
                }
                sent += 1;
                let progress = f64::from(sent) / (f64::from(sent) + 1.0);
                let param = ProgressNotificationParam::new(token.clone(), progress)
                    .with_total(1.0)
                    .with_message(line);
                if peer.notify_progress(param).await.is_err() {
                    return;
                }
                last = Some(tokio::time::Instant::now());
            }
        }
        if closed {
            return;
        }
    }
}

async fn execute_registry(
    name: String,
    args: Value,
    workspace: PathBuf,
    progress: Option<crate::susi_core::progress::Sink>,
    cancel: Option<crate::susi_core::progress::Cancel>,
) -> Result<CallToolResult, ErrorData> {
    let text = tokio::task::spawn_blocking(move || {
        crate::susi_core::progress::with_sink(progress, || {
            crate::susi_core::progress::with_cancel(cancel, || {
                ToolRegistry::execute_tool(&name, &args, &workspace)
            })
        })
    })
    .await
    .map_err(internal)?;
    Ok(if is_tool_result_error(&text) {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(text)])
    })
}

pub(crate) fn is_tool_result_error(text: &str) -> bool {
    let text = text.trim_start();
    // A JSON document is a result, never a flattened error: a report row's
    // `detail` (sorted first) can quote an `... Error:` line near the head.
    if (text.starts_with('{') || text.starts_with('['))
        && serde_json::from_str::<serde_json::Value>(text).is_ok()
    {
        return false;
    }
    if "Error:|[Error]|[FAIL]|[CAPABILITY_GAP]|Protocol Error:|Reflex Error:|Governance Violation:|[RECOVERY]"
        .split('|')
        .any(|prefix| text.starts_with(prefix))
    {
        return true;
    }
    // Any other flattened `EaiError` display ("Sandbox Error: ...",
    // "I/O Error: ...") near the head; later mentions are content.
    let head: String = text.chars().take(64).collect();
    head.contains(" Error:") || head.contains(" Violation:")
}
