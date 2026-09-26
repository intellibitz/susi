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
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::Arc,
};
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

#[derive(Clone, Debug)]
enum Change {
    Tools,
    Resources,
    Prompts,
    Resource(String),
}

#[derive(Clone)]
struct RegisteredTool {
    definition: Tool,
    handler: ToolHandler,
    input: Arc<jsonschema::Validator>,
    output: Option<Arc<jsonschema::Validator>>,
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
}

impl GmcpService {
    pub fn new(workspace: PathBuf) -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            workspace,
            catalog: Default::default(),
            changes,
            tasks: Default::default(),
            legacy_subscriptions: Default::default(),
            log_level: Default::default(),
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
            let definition = serde_json::from_value(json!({
                "name": tool.name, "description": tool.description,
                "inputSchema": {"type":"object", "additionalProperties":true}
            }))
            .map_err(internal)?;
            tools.insert(tool.name, definition);
        }
        for (name, tool) in &self.catalog.read().tools {
            tools.insert(name.clone(), tool.definition.clone());
        }
        Ok(tools.into_values().collect())
    }
}

fn compile_schema(schema: &Value) -> Result<Arc<jsonschema::Validator>, ErrorData> {
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
    // External network/file retrieval is disabled at the dependency level.
    // Local $defs/$ref are supported; providers must bundle external references.
    jsonschema::draft202012::options()
        .with_pattern_options(
            jsonschema::PatternOptions::fancy_regex()
                .backtrack_limit(20_000)
                .size_limit(1_000_000),
        )
        .build(schema)
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
    fn get_info(&self) -> ServerConfig {
        let mut info = ServerConfig::default();
        info.server_info = Implementation::new("susi-gmcp", env!("CARGO_PKG_VERSION"));
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
            .enable_tasks()
            .build();
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
                        // Synchronous registry tools cannot be interrupted after entry. A late
                        // cancellation remains a request, never a false claim of rollback.
                        execute_registry(name, args, workspace)
                            .await
                            .map_err(Into::into)
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
        let result = execute_registry(name, args, workspace).await?;
        if let Some(token) = token {
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

async fn execute_registry(
    name: String,
    args: Value,
    workspace: PathBuf,
) -> Result<CallToolResult, ErrorData> {
    let text =
        tokio::task::spawn_blocking(move || ToolRegistry::execute_tool(&name, &args, &workspace))
            .await
            .map_err(internal)?;
    Ok(if is_tool_result_error(&text) {
        CallToolResult::error(vec![ContentBlock::text(text)])
    } else {
        CallToolResult::success(vec![ContentBlock::text(text)])
    })
}

fn is_tool_result_error(text: &str) -> bool {
    let text = text.trim_start();
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
}
