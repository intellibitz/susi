//! MCP application handlers. Wire validation, negotiation, framing, cancellation,
//! request correlation and version-specific serialization belong to rmcp.
#![allow(deprecated)] // Legacy MCP compatibility during the protocol deprecation window.
use crate::tools::ToolRegistry;
use parking_lot::RwLock;
use rmcp::{
    model::*,
    service::{
        NotificationContext, RequestContext, RoleServer, SubscriptionContext, SubscriptionSendError,
    },
    ErrorData, ServerHandler,
};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};
use susi_vendor_mcp_server as rmcp;
use tokio::sync::broadcast;

pub type ProtocolFuture<T> = Pin<Box<dyn Future<Output = Result<T, ErrorData>> + Send>>;
/// Providers can use the full typed response (including multimodal content,
/// structured output, MRTR elicitation/sampling/roots, and task handles).
pub type ToolHandler = Arc<
    dyn Fn(CallToolRequestParams, RequestContext<RoleServer>) -> ProtocolFuture<CallToolResponse>
        + Send
        + Sync,
>;
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
    dyn Fn(CallToolRequestParams, rmcp::task_manager::TaskContext) -> rmcp::task_manager::TaskFuture
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
enum Change {
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
struct RegisteredTool {
    definition: Tool,
    handler: ToolHandler,
    input: Arc<crate::schema_check::Validator>,
    output: Option<Arc<crate::schema_check::Validator>>,
}

#[derive(Default)]
struct Catalog {
    tools: BTreeMap<String, RegisteredTool>,
    resources: BTreeMap<String, (Resource, ResourceHandler)>,
    templates: BTreeMap<String, (ResourceTemplate, ResourceHandler)>,
    prompts: BTreeMap<String, (Prompt, PromptHandler)>,
    completions: BTreeMap<String, CompletionHandler>,
}

#[derive(Clone)]
pub struct GmcpService {
    workspace: PathBuf,
    catalog: Arc<RwLock<Catalog>>,
    changes: broadcast::Sender<Change>,
    pub tasks: rmcp::task_manager::TaskManager,
    legacy_subscriptions: Arc<RwLock<BTreeSet<String>>>,
    log_level: Arc<RwLock<Option<LoggingLevel>>>,
    extensions: Arc<RwLock<crate::extensions::ExtensionRegistry>>,
}

impl GmcpService {
    /// The extension registry that drives advertised capabilities.
    pub fn extensions(&self) -> Arc<RwLock<crate::extensions::ExtensionRegistry>> {
        self.extensions.clone()
    }
    pub fn new(workspace: PathBuf) -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            workspace,
            catalog: Default::default(),
            changes,
            tasks: Default::default(),
            legacy_subscriptions: Default::default(),
            log_level: Default::default(),
            extensions: Arc::new(RwLock::new(crate::extensions::ExtensionRegistry::shipped())),
        }
    }
    /// Isolate legacy session preferences while sharing the application catalog.
    pub fn session(&self) -> Self {
        Self {
            legacy_subscriptions: Default::default(),
            log_level: Default::default(),
            ..self.clone()
        }
    }
    /// The canonical server identity — the `serverInfo` value every
    /// `server/discover` result carries under
    /// `_meta["io.modelcontextprotocol/serverInfo"]`. Self-reported
    /// metadata for display and debugging only; never consulted for
    /// behavior or security decisions.
    pub fn server_identity() -> Implementation {
        Implementation::new("susi-gmcp", env!("CARGO_PKG_VERSION"))
    }
    /// The complete, cacheable `server/discover` payload: canonical
    /// identity, capabilities, the advertised version registry, and the
    /// contract's pinned cache hints — explicit state owned here rather
    /// than ambient handler defaults (VC-203-003).
    pub fn discover_result(&self) -> DiscoverResult {
        DiscoverResult::from_server_info(ProtocolVersion::KNOWN_VERSIONS.to_vec(), self.get_info())
            .with_ttl_ms(DISCOVER_TTL_MS)
            .with_cache_scope(DISCOVER_CACHE_SCOPE)
    }
    pub fn register_tool(&self, tool: Tool, handler: ToolHandler) -> Result<(), ErrorData> {
        let input = compile_schema(&Value::Object((*tool.input_schema).clone()))?;
        let output = tool
            .output_schema
            .as_ref()
            .map(|s| compile_schema(&Value::Object((**s).clone())))
            .transpose()?;
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
        let tasks = self.tasks.clone();
        let output = tool
            .output_schema
            .as_ref()
            .map(|s| compile_schema(&Value::Object((**s).clone())))
            .transpose()?;
        self.register_tool(
            tool,
            Arc::new(move |request, context| {
                let tasks = tasks.clone();
                let handler = handler.clone();
                let output = output.clone();
                Box::pin(async move {
                    if !context
                        .client_capabilities()
                        .is_some_and(|c| c.supports_tasks())
                    {
                        return Err(ErrorData::missing_required_client_capability(
                            ClientCapabilities::builder().enable_tasks().build(),
                        ));
                    }
                    let task = tasks.spawn(Default::default(), move |context| {
                        Box::pin(async move {
                            let result = handler(request, context).await?;
                            if let Some(schema) = output {
                                if result.is_error != Some(true)
                                    && !result
                                        .structured_content
                                        .as_ref()
                                        .is_some_and(|value| schema.is_valid(value))
                                {
                                    return Ok(CallToolResult::error(vec![ContentBlock::text(
                                        "Task output does not match outputSchema",
                                    )]));
                                }
                            }
                            Ok(result)
                        })
                    });
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
    fn tools(&self) -> Result<Vec<Tool>, ErrorData> {
        let mut tools = BTreeMap::new();
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
        for (name, tool) in &self.catalog.read().tools {
            tools.insert(name.clone(), tool.definition.clone());
        }
        Ok(tools.into_values().collect())
    }
}

fn compile_schema(schema: &Value) -> Result<Arc<crate::schema_check::Validator>, ErrorData> {
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

fn internal(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::internal_error(error.to_string(), None)
}
fn invalid(message: &str) -> ErrorData {
    ErrorData::invalid_params(message.to_owned(), None)
}

/// Cursors are bound to the complete catalog snapshot, preventing silent skips
/// when a dynamic catalog changes between pages. Clients then restart listing.
fn page<T: serde::Serialize + Clone>(
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
        let mut info = ServerConfig::default();
        info.server_info = Self::server_identity();
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
        let mut changes = self.changes.subscribe();
        let subscriptions = self.legacy_subscriptions.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                tokio::select! {
                    _ = tick.tick() => { if context.peer.is_transport_closed() { break; } },
                    change = changes.recv() => {
                        let Ok(change) = change else { break; };
                        let result = match change {
                            Change::Tools => context.peer.notify_tool_list_changed().await,
                            Change::Resources => context.peer.notify_resource_list_changed().await,
                            Change::Prompts => context.peer.notify_prompt_list_changed().await,
                            Change::Resource(uri) => {
                                let subscribed = subscriptions.read().contains(&uri);
                                if !subscribed { continue; }
                                context.peer.notify_resource_updated(ResourceUpdatedNotificationParam::new(uri)).await
                            }
                        };
                        if result.is_err() { break; }
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
        let (tools, next_cursor) = page(self.tools()?, request)?;
        let mut result = ListToolsResult::default()
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private);
        result.tools = tools;
        result.next_cursor = next_cursor;
        Ok(result)
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
            let mut response = (tool.handler)(request, context).await?;
            if let CallToolResponse::Complete(result) = &mut response {
                result.result_type = Some(ResultType::COMPLETE);
            }
            if let (Some(schema), CallToolResponse::Complete(result)) = (&tool.output, &response) {
                if result.is_error != Some(true)
                    && !result
                        .structured_content
                        .as_ref()
                        .is_some_and(|value| schema.is_valid(value))
                {
                    return Ok(CallToolResult::error(vec![ContentBlock::text(
                        "Tool output does not match outputSchema",
                    )])
                    .into());
                }
            }
            return Ok(response);
        }
        if !self.tools()?.iter().any(|t| t.name == request.name) {
            return Err(invalid("Unknown tool"));
        }
        let workspace = self.workspace.clone();
        let name = request.name.into_owned();
        let args = Value::Object(request.arguments.unwrap_or_default());
        if context
            .client_capabilities()
            .is_some_and(|c| c.supports_tasks())
            && context
                .protocol_version()
                .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28)
        {
            let task = self
                .tasks
                .spawn(rmcp::task_manager::TaskOptions::default(), move |task| {
                    Box::pin(async move {
                        if task.is_cancel_requested() {
                            return Err(rmcp::task_manager::TaskExit::Cancelled);
                        }
                        // Synchronous registry tools other than `mission_run` cannot be
                        // interrupted after entry. A late cancellation remains a request,
                        // never a false claim of rollback.
                        let (cancel, cancel_watch) = if name == "mission_run" {
                            let flag = Arc::new(std::sync::atomic::AtomicBool::new(
                                task.is_cancel_requested(),
                            ));
                            let observed = flag.clone();
                            let watched_task = task.clone();
                            let watch = tokio::spawn(async move {
                                watched_task.cancelled().await;
                                observed.store(true, std::sync::atomic::Ordering::Release);
                            });
                            (Some(flag), Some(watch))
                        } else {
                            (None, None)
                        };
                        let result = execute_registry(name, args, workspace, None, cancel).await;
                        if let Some(watch) = cancel_watch {
                            watch.abort();
                        }
                        let result = result.map_err(rmcp::task_manager::TaskExit::from)?;
                        if task.is_cancel_requested() {
                            return Err(rmcp::task_manager::TaskExit::Cancelled);
                        }
                        Ok(result)
                    })
                });
            return Ok(CallToolResponse::Task(CreateTaskResult::new(task)));
        }
        let token = context.meta.get_progress_token();
        if let Some(token) = token.clone() {
            context
                .peer
                .notify_progress(
                    ProgressNotificationParam::new(token, 0.0)
                        .with_total(1.0)
                        .with_message("Tool started"),
                )
                .await
                .map_err(internal)?;
        }
        // `mission_run` checks this cooperative flag at phase boundaries and
        // stops early with status `CANCELLED` when `notifications/cancelled`
        // arrives; other synchronous registry tools run to completion.
        let (cancel, cancel_watch) = if name == "mission_run" {
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(
                context.ct.is_cancelled(),
            ));
            let observed = flag.clone();
            let ct = context.ct.clone();
            let watch = tokio::spawn(async move {
                ct.cancelled().await;
                observed.store(true, std::sync::atomic::Ordering::Release);
            });
            (Some(flag), Some(watch))
        } else {
            (None, None)
        };
        // A watched call relays the tool's own progress lines between the
        // started (0.0) and finished (1.0) notifications.
        let result = match token.clone() {
            Some(token) => {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                let sink: crate::susi_core::progress::Sink = Arc::new(move |line: &str| {
                    let _ = tx.send(line.to_string());
                });
                let relay = tokio::spawn(relay_progress(context.peer.clone(), token, rx));
                // The sink (and with it the channel's sender) is dropped
                // when the tool returns, which ends the relay.
                let result = execute_registry(name, args, workspace, Some(sink), cancel).await;
                let _ = relay.await;
                result
            }
            None => execute_registry(name, args, workspace, None, cancel).await,
        };
        if let Some(watch) = cancel_watch {
            watch.abort();
        }
        let result = result?;
        if let Some(token) = token.filter(|_| !context.ct.is_cancelled()) {
            context
                .peer
                .notify_progress(
                    ProgressNotificationParam::new(token, 1.0)
                        .with_total(1.0)
                        .with_message("Tool finished"),
                )
                .await
                .map_err(internal)?;
        }
        let threshold = if context
            .protocol_version()
            .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28)
        {
            context.meta.log_level()
        } else {
            *self.log_level.read()
        };
        if threshold.is_some_and(|level| matches!(level, LoggingLevel::Debug | LoggingLevel::Info))
        {
            context
                .peer
                .notify_logging_message(
                    LoggingMessageNotificationParam::new(
                        LoggingLevel::Info,
                        json!("Tool finished"),
                    )
                    .with_logger("susi-gmcp"),
                )
                .await
                .map_err(internal)?;
        }
        Ok(result.into())
    }

    async fn list_resources(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        let items = self
            .catalog
            .read()
            .resources
            .values()
            .map(|(r, _)| r.clone())
            .collect();
        let (resources, next_cursor) = page(items, request)?;
        let mut result = ListResourcesResult::default()
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private);
        result.resources = resources;
        result.next_cursor = next_cursor;
        Ok(result)
    }
    async fn list_resource_templates(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, ErrorData> {
        let items = self
            .catalog
            .read()
            .templates
            .values()
            .map(|(r, _)| r.clone())
            .collect();
        let (templates, next_cursor) = page(items, request)?;
        let mut result = ListResourceTemplatesResult::default()
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private);
        result.resource_templates = templates;
        result.next_cursor = next_cursor;
        Ok(result)
    }
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let handler = self
            .catalog
            .read()
            .resources
            .get(&request.uri)
            .map(|(_, h)| h.clone());
        if let Some(handler) = handler {
            return handler(request, context).await.map(normalize_resource);
        }
        // Template providers own URI matching and return RESOURCE_NOT_FOUND for a miss.
        let handlers: Vec<_> = self
            .catalog
            .read()
            .templates
            .values()
            .map(|(_, h)| h.clone())
            .collect();
        for handler in handlers {
            match handler(request.clone(), context.clone()).await {
                Err(e) if e.code == ErrorCode::RESOURCE_NOT_FOUND => continue,
                result => return result.map(normalize_resource),
            }
        }
        Err(ErrorData::resource_not_found(
            "Unknown resource",
            Some(json!({"uri":request.uri})),
        ))
    }
    async fn list_prompts(
        &self,
        request: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        let items = self
            .catalog
            .read()
            .prompts
            .values()
            .map(|(p, _)| p.clone())
            .collect();
        let (prompts, next_cursor) = page(items, request)?;
        let mut result = ListPromptsResult::default()
            .with_ttl_ms(0)
            .with_cache_scope(CacheScope::Private);
        result.prompts = prompts;
        result.next_cursor = next_cursor;
        Ok(result)
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
            _ => return Err(invalid("Unknown completion reference")),
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
    ) -> Option<SubscriptionFilter> {
        Some(requested.clone())
    }
    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let mut changes = self.changes.subscribe();
        loop {
            tokio::select! {
                _ = context.cancelled() => return Ok(()),
                change = changes.recv() => {
                    // A slow subscriber must restart rather than silently miss changes.
                    let change = change.map_err(internal)?;
                    let sink = context.sink();
                    let result = match change {
                        Change::Tools => sink.notify_tool_list_changed().await,
                        Change::Prompts => sink.notify_prompt_list_changed().await,
                        Change::Resources => sink.notify_resource_list_changed().await,
                        Change::Resource(uri) => sink.notify_resource_updated(uri).await,
                    };
                    // The SDK enforces the negotiated filter; unrequested events are skipped.
                    match result {
                        Ok(()) | Err(SubscriptionSendError::NotificationNotAccepted(_)) => {},
                        Err(SubscriptionSendError::SubscriptionClosed) => return Ok(()),
                        Err(error) => return Err(internal(error)),
                    }
                }
            }
        }
    }
    async fn get_task(
        &self,
        request: GetTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<GetTaskResult, ErrorData> {
        self.tasks
            .get_task(&request.task_id)
            .map(GetTaskResult::new)
    }
    async fn update_task(
        &self,
        request: UpdateTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks
            .update_task(&request.task_id, request.input_responses)
    }
    async fn cancel_task(
        &self,
        request: CancelTaskParams,
        _: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        self.tasks.cancel_task(&request.task_id)
    }
}

fn normalize_resource(mut response: ReadResourceResponse) -> ReadResourceResponse {
    if let ReadResourceResponse::Complete(result) = &mut response {
        result.result_type = Some(ResultType::COMPLETE);
        result.ttl_ms.get_or_insert(0);
        result.cache_scope.get_or_insert(CacheScope::Private);
    }
    response
}

/// Least gap between relayed progress notifications: at most about ten a
/// second. A line that arrives sooner waits, and a newer one replaces it.
const PROGRESS_GAP: std::time::Duration = std::time::Duration::from_millis(100);

/// Relay a watched tool's progress lines as `notifications/progress`,
/// strictly between the started (0.0) and finished (1.0) notifications and
/// rising with each line, coalesced to [`PROGRESS_GAP`]. Ends when the
/// tool's sink is dropped, after sending the last pending line.
async fn relay_progress(
    peer: rmcp::service::Peer<RoleServer>,
    token: ProgressToken,
    mut lines: tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let mut sent = 0u32;
    let mut last: Option<tokio::time::Instant> = None;
    let mut pending: Option<String> = None;
    loop {
        let next = match (&pending, last) {
            (Some(_), Some(at)) => {
                match tokio::time::timeout_at(at + PROGRESS_GAP, lines.recv()).await {
                    Ok(next) => next.map(Some),
                    // The gap has passed: send what is waiting.
                    Err(_) => Some(None),
                }
            }
            _ => lines.recv().await.map(Some),
        };
        let closed = next.is_none();
        if let Some(Some(line)) = next {
            pending = Some(line);
        }
        let due = closed || last.is_none_or(|at| at.elapsed() >= PROGRESS_GAP);
        if due {
            if let Some(line) = pending.take() {
                sent += 1;
                let progress = f64::from(sent) / (f64::from(sent) + 1.0);
                let _ = peer
                    .notify_progress(
                        ProgressNotificationParam::new(token.clone(), progress)
                            .with_total(1.0)
                            .with_message(line),
                    )
                    .await;
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
    if [
        "Error:",
        "[Error]",
        "[FAIL]",
        "[CAPABILITY_GAP]",
        "Protocol Error:",
        "Reflex Error:",
        "Governance Violation:",
        "[RECOVERY]",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
    {
        return true;
    }
    // Any other flattened `EaiError` display ("Sandbox Error: ...",
    // "I/O Error: ...") near the head; later mentions are content.
    let head: String = text.chars().take(64).collect();
    head.contains(" Error:") || head.contains(" Violation:")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_failures_remain_errors() {
        for prefix in [
            "Error:",
            "[FAIL]",
            "[CAPABILITY_GAP]",
            "Protocol Error:",
            "Reflex Error:",
            "Governance Violation:",
            "[RECOVERY]",
            "[Error]",
            "Sandbox Error:",
            "I/O Error:",
        ] {
            assert!(is_tool_result_error(&format!("  {prefix} detail")));
        }
        assert!(!is_tool_result_error("ok content"));
        assert!(!is_tool_result_error(
            "{\n  \"models\": [{\"detail\": \"Internal Engine Error: set KEY\"}]\n}"
        ));
        assert!(is_tool_result_error("[Error] {not json"));
        assert!(!is_tool_result_error(
            "Wrote 2048 bytes to notes/incident-review.txt; the file quotes an old Sandbox Error: line"
        ));
    }
    #[test]
    fn pagination_rejects_stale_and_invalid_cursors() {
        let items: Vec<_> = (0..101).collect();
        let (first, next) = page(items.clone(), None).unwrap();
        assert_eq!(first.len(), 100);
        let request = serde_json::from_value(json!({"cursor":next})).unwrap();
        assert_eq!(page(items, Some(request)).unwrap().0, vec![100]);
        let request = serde_json::from_value(json!({"cursor":next})).unwrap();
        assert!(page(vec![1, 2], Some(request)).is_err());
        let request = serde_json::from_value(json!({"cursor":"bad"})).unwrap();
        assert!(page(vec![1], Some(request)).is_err());
    }

    #[test]
    fn every_registered_mcp_tool_has_valid_typed_or_explicit_input_schema() {
        let service = GmcpService::new(std::env::temp_dir());
        let noop_handler: ToolHandler =
            Arc::new(|_req, _ctx| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }));
        let core_defs = crate::tools::args::core_tool_definitions();
        assert!(
            !core_defs.is_empty(),
            "CoreTools definitions must not be empty"
        );
        for def in &core_defs {
            service
                .register_tool(def.clone(), noop_handler.clone())
                .unwrap_or_else(|e| {
                    panic!(
                        "core tool `{}` failed GmcpService::register_tool: {}",
                        def.name, e.message
                    )
                });
        }

        let tools = service.tools().expect("tools catalog must load");
        assert!(
            tools.len() >= core_defs.len(),
            "MCP tools catalog must include all registered core tools"
        );

        let mut core_checked = 0usize;
        for tool in &tools {
            let schema_val = Value::Object((*tool.input_schema).clone());
            compile_schema(&schema_val).unwrap_or_else(|e| {
                panic!(
                    "tool `{}` input_schema failed compile_schema: {}",
                    tool.name, e.message
                )
            });
            assert_eq!(
                tool.input_schema.get("type").and_then(Value::as_str),
                Some("object"),
                "tool `{}` input_schema must declare root type \"object\"",
                tool.name
            );

            if crate::tools::args::core_input_schema(&tool.name).is_some() {
                core_checked += 1;
                assert!(
                    !tool
                        .input_schema
                        .contains_key(crate::tools::args::PASSTHROUGH_KEY),
                    "core tool `{}` must not use passthrough input schema",
                    tool.name
                );
                let props = tool
                    .input_schema
                    .get("properties")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| {
                        panic!(
                            "core tool `{}` must declare an object `properties` map",
                            tool.name
                        )
                    });
                let explicit_no_args = props.is_empty()
                    && tool
                        .input_schema
                        .get("additionalProperties")
                        .and_then(Value::as_bool)
                        == Some(false);
                assert!(
                    !props.is_empty() || explicit_no_args,
                    "core tool `{}` must have non-empty `properties` or explicit no-args (`additionalProperties: false`)",
                    tool.name
                );
            }
        }
        assert_eq!(
            core_checked,
            core_defs.len(),
            "every built-in CoreTools handler must be verified"
        );
    }

    #[test]
    fn compile_schema_enforces_depth_and_node_complexity_limits() {
        let valid_schema = json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 }
            },
            "required": ["query"],
            "additionalProperties": false
        });
        let validator = compile_schema(&valid_schema).expect("valid object schema must compile");
        assert!(validator.is_valid(&json!({ "query": "status" })));
        assert!(!validator.is_valid(&json!({ "query": "" })));
        assert!(!validator.is_valid(&json!({})));

        // Build a schema nested beyond the 64-level depth ceiling.
        let mut deep = json!({ "type": "string" });
        for _ in 0..70 {
            deep = json!({
                "type": "object",
                "properties": { "nested": deep }
            });
        }
        let deep_err = match compile_schema(&deep) {
            Ok(_) => panic!("schema exceeding depth limit must be rejected"),
            Err(err) => err,
        };
        assert_eq!(deep_err.code, ErrorCode::INVALID_PARAMS);
        assert!(
            deep_err.message.contains("Schema exceeds complexity limit"),
            "unexpected error message: {}",
            deep_err.message
        );

        let invalid_type = json!({
            "type": "not-a-valid-json-schema-type"
        });
        let type_err = match compile_schema(&invalid_type) {
            Ok(_) => panic!("malformed schema type must be rejected"),
            Err(err) => err,
        };
        assert_eq!(type_err.code, ErrorCode::INVALID_PARAMS);
        assert!(
            type_err.message.contains("Invalid tool schema"),
            "unexpected error message: {}",
            type_err.message
        );
    }

    #[test]
    fn register_tool_replaces_catalog_entry_and_validates_arguments() {
        let service = GmcpService::new(std::env::temp_dir());
        let schema_obj: JsonObject = serde_json::from_value(json!({
            "type": "object",
            "properties": {
                "count": { "type": "integer", "minimum": 1 }
            },
            "required": ["count"],
            "additionalProperties": false
        }))
        .expect("schema object");
        let tool = Tool::new_with_raw(
            "custom_counter",
            Some("Count items with strict schema validation".into()),
            Arc::new(schema_obj),
        );
        let handler: ToolHandler = Arc::new(|_req, _ctx| {
            Box::pin(async {
                Ok(CallToolResult::success(vec![ContentBlock::text("counted")]).into())
            })
        });
        service
            .register_tool(tool, handler)
            .expect("custom tool with valid schema must register");

        let fetched = service
            .get_tool("custom_counter")
            .expect("registered tool must appear in get_tool");
        assert_eq!(
            fetched.description.as_deref(),
            Some("Count items with strict schema validation")
        );

        let registered = service
            .catalog
            .read()
            .tools
            .get("custom_counter")
            .cloned()
            .expect("catalog entry must exist");
        assert!(registered.input.is_valid(&json!({ "count": 3 })));
        assert!(!registered.input.is_valid(&json!({ "count": 0 })));
        assert!(!registered.input.is_valid(&json!({ "count": "three" })));
        assert!(!registered.input.is_valid(&json!({})));

        let bad_schema: JsonObject = serde_json::from_value(json!({
            "type": "invalid_type_name"
        }))
        .expect("bad schema object");
        let bad_tool = Tool::new_with_raw("bad_tool", None, Arc::new(bad_schema));
        let bad_handler: ToolHandler =
            Arc::new(|_req, _ctx| Box::pin(async { Ok(CallToolResult::success(vec![]).into()) }));
        assert!(
            service.register_tool(bad_tool, bad_handler).is_err(),
            "register_tool must reject invalid inputSchema"
        );
    }
}
