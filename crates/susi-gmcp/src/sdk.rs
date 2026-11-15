//! Canonical Susi-shaped GMCP server authoring SDK and transactional plugin
//! host lifecycle (`VC-203-020`, `docs/gmcp/contract-203-020.md`).

pub mod examples;

use crate::extensions::{ExtensionError, ExtensionRegistry, ExtensionStatus};
use crate::mcp::{
    model::*,
    schema::{self, StructSchema},
    service::{CancellationToken, RequestContext, RoleServer},
    task_manager::{TaskContext, TaskExit, TaskManager, TaskPrincipal},
    ErrorData, ServiceExt,
};
use crate::protocol::{
    compile_opt_schema, compile_schema, enforce_output_schema, finish_task_run, internal, invalid,
    Catalog, Change, CompletionHandler, GmcpService, PromptHandler, RegisteredTool, ResourceGuard,
    ResourceHandler, TaskToolHandler, ToolHandler, ToolMiddleware,
};
use crate::susi_config::extensions::{
    validate_manifest, verify_pack_dir_for_sdk, ExtensionManifest,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap},
    future::Future,
    path::PathBuf,
    sync::{
        atomic::{AtomicI64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot},
};

/// Canonical host capability identifiers advertised to plugins (`SDK-014`, `SDK-015`).
pub const HOST_CAPABILITIES: &[&str] = &[
    "gmcp.server.v1",
    "gmcp.tools.v1",
    "gmcp.resources.v1",
    "gmcp.prompts.v1",
    "gmcp.subscriptions.v1",
    "gmcp.mrtr.v1",
    "gmcp.tasks.v1",
    "gmcp.extensions.v1",
];

/// Declare a typed Rust struct with `Serialize` + `Deserialize` and derive its
/// JSON Schema `2020-12` (`StructSchema` + `SchemaType`) in one step (`SDK-002`).
#[macro_export]
macro_rules! sdk_schema {
    ($(
        $(#[doc = $sdoc:literal])*
        $vis:vis struct $name:ident {
            $( $(#[$($fattr:tt)*])* $fvis:vis $field:ident : $ty:ty ),* $(,)?
        }
    )*) => {$(
        $(#[doc = $sdoc])*
        #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
        $vis struct $name {
            $( $(#[$($fattr)*])* $fvis $field : $ty, )*
        }
        impl $crate::mcp::schema::StructSchema for $name {
            const NAME: &'static str = stringify!($name);
            fn object(defs: &mut $crate::mcp::model::JsonObject) -> $crate::mcp::model::JsonObject {
                #[allow(unused_mut)]
                let (mut properties, mut required) = ($crate::mcp::model::JsonObject::new(), Vec::new());
                $(
                    #[allow(unused_mut)]
                    let mut field = $crate::mcp::schema::Field::new(stringify!($field));
                    $( $crate::sdk_schema!(@attr field; $($fattr)*); )*
                    field.add::<$ty>(defs, &mut properties, &mut required);
                )*
                $crate::mcp::schema::object(&[$($sdoc),*], properties, required)
            }
        }
        impl $crate::mcp::schema::SchemaType for $name {
            const SERIALIZABLE: bool = false;
            fn schema(defs: &mut $crate::mcp::model::JsonObject) -> serde_json::Value {
                $crate::mcp::schema::reference::<$name>(defs)
            }
            fn default_value() -> Option<serde_json::Value> { None }
        }
    )*};
    (@attr $f:ident; doc = $doc:literal) => { $f.docs.push($doc); };
    (@attr $f:ident; serde(default)) => { $f.default = true; };
    (@attr $f:ident; serde(rename = $n:literal)) => { $f.name = $n; };
    (@attr $f:ident; serde(default, rename = $n:literal)) => { $f.default = true; $f.name = $n; };
    (@attr $f:ident; serde(rename = $n:literal, default)) => { $f.name = $n; $f.default = true; };
}

// ── MRTR Helpers (SDK-007) ───────────────────────────────────────────────

pub fn mrtr_elicitation(
    message: impl Into<String>,
    requested_schema: Value,
) -> Result<InputRequest, ErrorData> {
    serde_json::from_value(json!({
        "method": "elicitation/create",
        "params": { "mode": "form", "message": message.into(), "requestedSchema": requested_schema }
    }))
    .map_err(internal)
}

pub fn mrtr_sampling(
    prompt: impl Into<String>,
    max_tokens: u32,
) -> Result<InputRequest, ErrorData> {
    serde_json::from_value(json!({
        "method": "sampling/createMessage",
        "params": {
            "messages": [{ "role": "user", "content": { "type": "text", "text": prompt.into() } }],
            "maxTokens": max_tokens
        }
    }))
    .map_err(internal)
}

pub fn mrtr_roots_list() -> Result<InputRequest, ErrorData> {
    serde_json::from_value(json!({ "method": "roots/list", "params": {} })).map_err(internal)
}

pub fn mrtr_input_required(
    requests: InputRequests,
    request_state: Option<String>,
) -> Result<InputRequiredResult, ErrorData> {
    if requests.is_empty() && request_state.is_none() {
        return Err(invalid(
            "MRTR input_required requires at least one inputRequest or requestState",
        ));
    }
    Ok(InputRequiredResult {
        result_type: ResultType::INPUT_REQUIRED,
        input_requests: (!requests.is_empty()).then_some(requests),
        request_state,
        meta: None,
    })
}

fn ensure_cap(present: bool, required: ClientCapabilities) -> Result<(), ErrorData> {
    present
        .then_some(())
        .ok_or_else(|| ErrorData::missing_required_client_capability(required))
}

pub fn require_client_elicitation(ctx: &RequestContext<RoleServer>) -> Result<(), ErrorData> {
    let req = ClientCapabilities {
        elicitation: Some(Default::default()),
        ..Default::default()
    };
    ensure_cap(
        ctx.client_capabilities()
            .is_some_and(|c| c.elicitation.is_some()),
        req,
    )
}

pub fn require_client_sampling(ctx: &RequestContext<RoleServer>) -> Result<(), ErrorData> {
    let req = ClientCapabilities {
        sampling: Some(Default::default()),
        ..Default::default()
    };
    ensure_cap(
        ctx.client_capabilities()
            .is_some_and(|c| c.sampling.is_some()),
        req,
    )
}

pub fn require_client_roots(ctx: &RequestContext<RoleServer>) -> Result<(), ErrorData> {
    let req = ClientCapabilities {
        roots: Some(Default::default()),
        ..Default::default()
    };
    ensure_cap(
        ctx.client_capabilities().is_some_and(|c| c.roots.is_some()),
        req,
    )
}

// ── ServerBuilder (SDK-001..SDK-011) ─────────────────────────────────────

#[derive(Default)]
struct StagedItems {
    tools: Vec<(Tool, ToolHandler)>,
    task_tools: Vec<(Tool, TaskToolHandler)>,
    resources: Vec<(Resource, ResourceHandler)>,
    templates: Vec<(ResourceTemplate, ResourceHandler)>,
    prompts: Vec<(Prompt, PromptHandler)>,
    completions: Vec<(String, CompletionHandler)>,
    extensions: Vec<(String, ExtensionStatus, u32, JsonObject)>,
}

fn parse_typed_args<In: DeserializeOwned>(args: Option<JsonObject>) -> Result<In, String> {
    serde_json::from_value(Value::Object(args.unwrap_or_default()))
        .map_err(|e| format!("Invalid typed arguments: {e}"))
}

fn typed_err(msg: String) -> CallToolResponse {
    CallToolResult::error(vec![ContentBlock::text(msg)]).into()
}

/// Explicit Susi-shaped local MCP server builder with zero implicit global tools.
pub struct ServerBuilder {
    workspace: PathBuf,
    server_info: Implementation,
    instructions: Option<String>,
    durable_store: Option<PathBuf>,
    include_host_tools: bool,
    install_builtin_catalog: bool,
    resource_guard: Option<ResourceGuard>,
    middlewares: Vec<ToolMiddleware>,
    plugins: Vec<(PluginBundle, PluginPolicy)>,
    items: StagedItems,
}

impl ServerBuilder {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        workspace: impl Into<PathBuf>,
    ) -> Self {
        Self::from_identity(Implementation::new(name, version), workspace)
    }

    pub fn from_identity(server_info: Implementation, workspace: impl Into<PathBuf>) -> Self {
        Self {
            workspace: workspace.into(),
            server_info,
            instructions: None,
            durable_store: None,
            include_host_tools: false,
            install_builtin_catalog: false,
            resource_guard: None,
            middlewares: Vec::new(),
            plugins: Vec::new(),
            items: StagedItems::default(),
        }
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.server_info.title = Some(title.into());
        self
    }
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.server_info.description = Some(description.into());
        self
    }
    pub fn instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
    pub fn durable_tasks(mut self, store_dir: impl Into<PathBuf>) -> Self {
        self.durable_store = Some(store_dir.into());
        self
    }
    pub fn with_host_tools(mut self, enabled: bool) -> Self {
        self.include_host_tools = enabled;
        self
    }
    pub fn with_builtin_catalog(mut self) -> Self {
        self.install_builtin_catalog = true;
        self
    }
    pub fn tool(mut self, definition: Tool, handler: ToolHandler) -> Self {
        self.items.tools.push((definition, handler));
        self
    }

    pub fn tool_typed<In, F, Fut>(
        mut self,
        name: impl Into<Cow<'static, str>>,
        description: impl Into<Cow<'static, str>>,
        handler: F,
    ) -> Self
    where
        In: StructSchema + DeserializeOwned + Send + 'static,
        F: Fn(In, CallToolRequestParams, RequestContext<RoleServer>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CallToolResponse, ErrorData>> + Send + 'static,
    {
        let tool = Tool::new_with_raw(name, Some(description.into()), schema::root::<In>());
        let handler = Arc::new(handler);
        self.items.tools.push((
            tool,
            Arc::new(move |params, ctx| {
                let handler = Arc::clone(&handler);
                Box::pin(async move {
                    match parse_typed_args::<In>(params.arguments.clone()) {
                        Ok(parsed) => handler(parsed, params, ctx).await,
                        Err(msg) => Ok(typed_err(msg)),
                    }
                })
            }),
        ));
        self
    }

    pub fn tool_structured<In, Out, F, Fut>(
        mut self,
        name: impl Into<Cow<'static, str>>,
        description: impl Into<Cow<'static, str>>,
        handler: F,
    ) -> Self
    where
        In: StructSchema + DeserializeOwned + Send + 'static,
        Out: StructSchema + Serialize + Send + 'static,
        F: Fn(In, RequestContext<RoleServer>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Out, ErrorData>> + Send + 'static,
    {
        let mut tool = Tool::new_with_raw(name, Some(description.into()), schema::root::<In>());
        tool.output_schema = Some(Arc::new(schema::root::<Out>()));
        let handler = Arc::new(handler);
        self.items.tools.push((
            tool,
            Arc::new(move |params, ctx| {
                let handler = Arc::clone(&handler);
                Box::pin(async move {
                    let parsed = match parse_typed_args::<In>(params.arguments) {
                        Ok(v) => v,
                        Err(msg) => return Ok(typed_err(msg)),
                    };
                    let out = handler(parsed, ctx).await?;
                    Ok(
                        CallToolResult::structured(serde_json::to_value(out).map_err(internal)?)
                            .into(),
                    )
                })
            }),
        ));
        self
    }

    pub fn task_tool(mut self, definition: Tool, handler: TaskToolHandler) -> Self {
        self.items.task_tools.push((definition, handler));
        self
    }

    pub fn task_tool_typed<In, F, Fut>(
        mut self,
        name: impl Into<Cow<'static, str>>,
        description: impl Into<Cow<'static, str>>,
        handler: F,
    ) -> Self
    where
        In: StructSchema + DeserializeOwned + Send + 'static,
        F: Fn(In, TaskContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CallToolResult, TaskExit>> + Send + 'static,
    {
        let tool = Tool::new_with_raw(name, Some(description.into()), schema::root::<In>());
        let handler = Arc::new(handler);
        self.items.task_tools.push((
            tool,
            Arc::new(move |params, task_ctx| {
                let handler = Arc::clone(&handler);
                Box::pin(async move {
                    let raw = Value::Object(params.arguments.unwrap_or_default());
                    let parsed: In = serde_json::from_value(raw).map_err(|e| {
                        TaskExit::Error(invalid(&format!("Invalid task arguments: {e}")))
                    })?;
                    handler(parsed, task_ctx).await
                })
            }),
        ));
        self
    }

    pub fn resource(mut self, resource: Resource, handler: ResourceHandler) -> Self {
        self.items.resources.push((resource, handler));
        self
    }

    pub fn text_resource(
        self,
        uri: impl Into<String>,
        name: impl Into<String>,
        mime_type: impl Into<String>,
        body: impl Into<String>,
    ) -> Self {
        let (uri_str, mime_str, text_body) = (uri.into(), mime_type.into(), body.into());
        let resource = Resource::new(uri_str, name).with_mime_type(mime_str.clone());
        self.resource(
            resource,
            Arc::new(move |req, _ctx| {
                let (mime, text) = (mime_str.clone(), text_body.clone());
                Box::pin(async move { Ok(examples::text_read_response(req.uri, &mime, text)) })
            }),
        )
    }
    pub fn template(mut self, template: ResourceTemplate, handler: ResourceHandler) -> Self {
        self.items.templates.push((template, handler));
        self
    }
    pub fn resource_guard(mut self, guard: ResourceGuard) -> Self {
        self.resource_guard = Some(guard);
        self
    }
    pub fn prompt(mut self, prompt: Prompt, handler: PromptHandler) -> Self {
        self.items.prompts.push((prompt, handler));
        self
    }
    pub fn completion(mut self, key: impl Into<String>, handler: CompletionHandler) -> Self {
        self.items.completions.push((key.into(), handler));
        self
    }
    pub fn extension(
        mut self,
        id: impl Into<String>,
        status: ExtensionStatus,
        version: u32,
        settings: JsonObject,
    ) -> Self {
        self.items
            .extensions
            .push((id.into(), status, version, settings));
        self
    }
    pub fn middleware(mut self, middleware: ToolMiddleware) -> Self {
        self.middlewares.push(middleware);
        self
    }
    pub fn plugin(mut self, bundle: PluginBundle, policy: PluginPolicy) -> Self {
        self.plugins.push((bundle, policy));
        self
    }

    pub fn build(self) -> Result<GmcpService, ErrorData> {
        let tasks = match self.durable_store {
            Some(dir) => TaskManager::open(dir)?,
            None => TaskManager::new(),
        };
        let (changes, _) = tokio::sync::broadcast::channel(256);
        let mut ext_registry = ExtensionRegistry::shipped();
        for (id, status, version, settings) in self.items.extensions {
            ext_registry
                .register(&id, status, version, settings)
                .map_err(|e| invalid(&e.message()))?;
        }
        let service = GmcpService {
            workspace: self.workspace,
            server_info: self.server_info,
            instructions: self.instructions,
            include_host_tools: self.include_host_tools,
            catalog: Default::default(),
            changes,
            tasks,
            legacy_subscriptions: Default::default(),
            log_level: Default::default(),
            extensions: Arc::new(susi_paths::sync::RwLock::new(ext_registry)),
            middlewares: Arc::new(susi_paths::sync::RwLock::new(self.middlewares)),
            plugins: Default::default(),
        };
        if self.install_builtin_catalog {
            crate::catalog::install(&service)?;
        }
        if let Some(guard) = self.resource_guard {
            service.set_resource_guard(guard);
        }
        for (tool, handler) in self.items.tools {
            service.register_tool(tool, handler)?;
        }
        for (tool, handler) in self.items.task_tools {
            service.register_task_tool(tool, handler)?;
        }
        for (resource, handler) in self.items.resources {
            service.register_resource(resource, handler);
        }
        for (template, handler) in self.items.templates {
            service.register_template(template, handler);
        }
        for (prompt, handler) in self.items.prompts {
            service.register_prompt(prompt, handler);
        }
        for (key, handler) in self.items.completions {
            service.register_completion(key, handler);
        }
        for (bundle, policy) in self.plugins {
            service
                .attach_plugin(bundle, &policy)
                .map_err(|e| invalid(&e.to_string()))?;
        }
        Ok(service)
    }
}

// ── Atomic Hot Reload (SDK-013) ──────────────────────────────────────────

#[derive(Default)]
pub struct CatalogUpdate {
    pub tools: Vec<(Tool, ToolHandler)>,
    pub resources: Vec<(Resource, ResourceHandler)>,
    pub templates: Vec<(ResourceTemplate, ResourceHandler)>,
    pub prompts: Vec<(Prompt, PromptHandler)>,
    pub completions: Vec<(String, CompletionHandler)>,
    pub resource_guard: Option<Option<ResourceGuard>>,
}

impl CatalogUpdate {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_tool(mut self, tool: Tool, handler: ToolHandler) -> Self {
        self.tools.push((tool, handler));
        self
    }
    pub fn with_resource(mut self, resource: Resource, handler: ResourceHandler) -> Self {
        self.resources.push((resource, handler));
        self
    }
    pub fn with_template(mut self, template: ResourceTemplate, handler: ResourceHandler) -> Self {
        self.templates.push((template, handler));
        self
    }
    pub fn with_prompt(mut self, prompt: Prompt, handler: PromptHandler) -> Self {
        self.prompts.push((prompt, handler));
        self
    }
    pub fn with_completion(mut self, key: impl Into<String>, handler: CompletionHandler) -> Self {
        self.completions.push((key.into(), handler));
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReloadReport {
    pub tools_changed: bool,
    pub resources_changed: bool,
    pub prompts_changed: bool,
    pub updated_uris: Vec<String>,
}

// ── Plugin Lifecycle (SDK-014..SDK-016) ──────────────────────────────────

#[derive(Debug, Clone)]
pub struct PluginPolicy {
    pub granted_permissions: BTreeSet<String>,
    pub host_capabilities: BTreeSet<String>,
}

impl Default for PluginPolicy {
    fn default() -> Self {
        Self::strict()
    }
}

impl PluginPolicy {
    pub fn strict() -> Self {
        Self {
            granted_permissions: BTreeSet::new(),
            host_capabilities: HOST_CAPABILITIES.iter().map(|s| (*s).to_string()).collect(),
        }
    }
    pub fn grant_permission(mut self, perm: impl Into<String>) -> Self {
        self.granted_permissions.insert(perm.into());
        self
    }
    pub fn without_host_capability(mut self, cap: &str) -> Self {
        self.host_capabilities.remove(cap);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    InvalidManifest(String),
    IntegrityFailed(String),
    PermissionDenied(String, Vec<String>),
    IncompatibleCapability(String, Vec<String>),
    SchemaRejected(String, String),
    ExtensionRejected(String, String),
    Collision(&'static str, String, String),
    AlreadyLoaded(String),
    NotLoaded(String),
    InitFailed(String, String),
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidManifest(m) => write!(f, "invalid plugin manifest: {m}"),
            Self::IntegrityFailed(m) => write!(f, "plugin artifact integrity failed: {m}"),
            Self::PermissionDenied(id, m) => {
                write!(f, "plugin `{id}` ungranted permissions: {}", m.join(", "))
            }
            Self::IncompatibleCapability(id, m) => write!(
                f,
                "plugin `{id}` missing host capabilities: {}",
                m.join(", ")
            ),
            Self::SchemaRejected(id, d) => write!(f, "plugin `{id}` schema rejected: {d}"),
            Self::ExtensionRejected(id, d) => write!(f, "plugin `{id}` extension rejected: {d}"),
            Self::Collision(kind, id, key) => {
                write!(f, "plugin `{id}` {kind} `{key}` collides with catalog")
            }
            Self::AlreadyLoaded(id) => write!(f, "plugin `{id}` is already loaded"),
            Self::NotLoaded(id) => write!(f, "plugin `{id}` is not loaded"),
            Self::InitFailed(id, r) => write!(f, "plugin `{id}` initializer failed: {r}"),
        }
    }
}

impl std::error::Error for PluginError {}

pub type PluginInitHook = Arc<dyn Fn(&CancellationToken) -> Result<(), String> + Send + Sync>;

pub struct PluginBundle {
    pub manifest: ExtensionManifest,
    pub pack_root: Option<PathBuf>,
    pub tools: Vec<(Tool, ToolHandler)>,
    pub task_tools: Vec<(Tool, TaskToolHandler)>,
    pub resources: Vec<(Resource, ResourceHandler)>,
    pub templates: Vec<(ResourceTemplate, ResourceHandler)>,
    pub prompts: Vec<(Prompt, PromptHandler)>,
    pub completions: Vec<(String, CompletionHandler)>,
    pub extensions: Vec<(String, ExtensionStatus, u32, JsonObject)>,
    pub initializer: Option<PluginInitHook>,
    pub on_unload: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) host_synced: bool,
}

impl PluginBundle {
    pub fn new(manifest: ExtensionManifest) -> Self {
        Self {
            manifest,
            pack_root: None,
            tools: Vec::new(),
            task_tools: Vec::new(),
            resources: Vec::new(),
            templates: Vec::new(),
            prompts: Vec::new(),
            completions: Vec::new(),
            extensions: Vec::new(),
            initializer: None,
            on_unload: None,
            host_synced: false,
        }
    }
    pub fn from_pack_dir(pack_root: impl Into<PathBuf>) -> Result<Self, PluginError> {
        let root = pack_root.into();
        let (manifest, _) = verify_pack_dir_for_sdk(&root).map_err(PluginError::IntegrityFailed)?;
        Ok(Self::new(manifest).with_pack_root(root))
    }
    pub fn with_pack_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.pack_root = Some(root.into());
        self
    }
    pub fn with_tool(mut self, tool: Tool, handler: ToolHandler) -> Self {
        self.tools.push((tool, handler));
        self
    }
    pub fn with_task_tool(mut self, tool: Tool, handler: TaskToolHandler) -> Self {
        self.task_tools.push((tool, handler));
        self
    }
    pub fn with_resource(mut self, resource: Resource, handler: ResourceHandler) -> Self {
        self.resources.push((resource, handler));
        self
    }
    pub fn with_template(mut self, template: ResourceTemplate, handler: ResourceHandler) -> Self {
        self.templates.push((template, handler));
        self
    }
    pub fn with_prompt(mut self, prompt: Prompt, handler: PromptHandler) -> Self {
        self.prompts.push((prompt, handler));
        self
    }
    pub fn with_completion(mut self, key: impl Into<String>, handler: CompletionHandler) -> Self {
        self.completions.push((key.into(), handler));
        self
    }
    pub fn with_extension(
        mut self,
        id: impl Into<String>,
        status: ExtensionStatus,
        version: u32,
        settings: JsonObject,
    ) -> Self {
        self.extensions.push((id.into(), status, version, settings));
        self
    }
    pub fn with_initializer(mut self, hook: PluginInitHook) -> Self {
        self.initializer = Some(hook);
        self
    }
    pub fn with_on_unload(mut self, hook: Arc<dyn Fn() + Send + Sync>) -> Self {
        self.on_unload = Some(hook);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLoadedReport {
    pub id: String,
    pub version: String,
    pub missing_optional: Vec<String>,
    pub verified_digests: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginUnloadReport {
    pub id: String,
    pub removed_tools: usize,
    pub removed_resources: usize,
    pub removed_prompts: usize,
    pub removed_extensions: usize,
    pub cancelled_tasks: usize,
}

#[derive(Clone)]
pub(crate) struct LoadedPlugin {
    pub(crate) manifest: ExtensionManifest,
    pub(crate) tools: BTreeSet<String>,
    pub(crate) resources: BTreeSet<String>,
    pub(crate) templates: BTreeSet<String>,
    pub(crate) prompts: BTreeSet<String>,
    pub(crate) completions: BTreeSet<String>,
    pub(crate) extensions: BTreeSet<String>,
    pub(crate) cancel: CancellationToken,
    pub(crate) spawned_tasks: Arc<Mutex<Vec<(String, TaskPrincipal)>>>,
    pub(crate) on_unload: Option<Arc<dyn Fn() + Send + Sync>>,
    pub(crate) host_synced: bool,
}

fn stage_unique<V>(
    items: impl IntoIterator<Item = (String, V)>,
    dup_msg: &'static str,
) -> Result<BTreeMap<String, V>, ErrorData> {
    let mut map = BTreeMap::new();
    for (k, v) in items {
        if map.insert(k, v).is_some() {
            return Err(invalid(dup_msg));
        }
    }
    Ok(map)
}

fn retain_plugin_owned<V: Clone>(
    staged: &mut BTreeMap<String, V>,
    existing: &BTreeMap<String, V>,
    owned: impl Fn(&str) -> bool,
) {
    for (k, v) in existing {
        if owned(k) {
            staged.insert(k.clone(), v.clone());
        }
    }
}

fn diff_non_plugin<'a, V: 'a, D: PartialEq + 'a>(
    existing: &'a BTreeMap<String, V>,
    staged: &'a BTreeMap<String, V>,
    owned: impl Fn(&str) -> bool,
    proj: impl Fn(&'a V) -> &'a D,
) -> bool {
    let old: BTreeMap<_, _> = existing
        .iter()
        .filter(|(k, _)| !owned(k))
        .map(|(k, v)| (k.as_str(), proj(v)))
        .collect();
    let new: BTreeMap<_, _> = staged.iter().map(|(k, v)| (k.as_str(), proj(v))).collect();
    old != new
}

fn ensure_no_collision<V>(
    plugin_id: &str,
    kind: &'static str,
    keys: impl IntoIterator<Item = String>,
    active: &BTreeMap<String, V>,
) -> Result<(), PluginError> {
    for key in keys {
        if active.contains_key(&key) {
            return Err(PluginError::Collision(kind, plugin_id.to_string(), key));
        }
    }
    Ok(())
}

fn commit_owned<V>(
    items: impl IntoIterator<Item = (String, V)>,
    dst: &mut BTreeMap<String, V>,
) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    for (k, v) in items {
        keys.insert(k.clone());
        dst.insert(k, v);
    }
    keys
}

type CompiledSchemas = (
    Arc<crate::schema_check::Validator>,
    Option<Arc<crate::schema_check::Validator>>,
);

fn compile_plugin_tool_schemas(
    plugin_id: &str,
    tool: &Tool,
) -> Result<CompiledSchemas, PluginError> {
    let map_err =
        |e: ErrorData| PluginError::SchemaRejected(plugin_id.to_string(), e.message.to_string());
    let input = compile_schema(&Value::Object((*tool.input_schema).clone())).map_err(map_err)?;
    let output = compile_opt_schema(tool.output_schema.as_ref()).map_err(map_err)?;
    Ok((input, output))
}

impl GmcpService {
    pub fn loaded_plugins(&self) -> Vec<ExtensionManifest> {
        self.plugins
            .read()
            .values()
            .map(|p| p.manifest.clone())
            .collect()
    }

    pub fn reload_catalog(&self, update: CatalogUpdate) -> Result<ReloadReport, ErrorData> {
        let mut staged_tools = BTreeMap::new();
        for (tool, handler) in update.tools {
            let name = tool.name.to_string();
            if staged_tools.contains_key(&name) {
                return Err(invalid(&format!(
                    "Duplicate tool `{name}` in reload_catalog"
                )));
            }
            let input = compile_schema(&Value::Object((*tool.input_schema).clone()))?;
            let output = compile_opt_schema(tool.output_schema.as_ref())?;
            staged_tools.insert(
                name,
                RegisteredTool {
                    definition: tool,
                    handler,
                    input,
                    output,
                },
            );
        }
        let mut staged_resources = stage_unique(
            update
                .resources
                .into_iter()
                .map(|(r, h)| (r.uri.clone(), (r, h))),
            "Duplicate resource URI",
        )?;
        let mut staged_templates = stage_unique(
            update
                .templates
                .into_iter()
                .map(|(t, h)| (t.uri_template.clone(), (t, h))),
            "Duplicate template",
        )?;
        let mut staged_prompts = stage_unique(
            update
                .prompts
                .into_iter()
                .map(|(p, h)| (p.name.clone(), (p, h))),
            "Duplicate prompt",
        )?;
        let mut staged_completions = stage_unique(update.completions, "Duplicate completion")?;

        let report = {
            let plugins = self.plugins.read();
            for p in plugins.values() {
                if staged_tools.keys().any(|k| p.tools.contains(k))
                    || staged_resources.keys().any(|k| p.resources.contains(k))
                    || staged_templates.keys().any(|k| p.templates.contains(k))
                    || staged_prompts.keys().any(|k| p.prompts.contains(k))
                    || staged_completions.keys().any(|k| p.completions.contains(k))
                {
                    return Err(invalid(
                        "reload_catalog collides with an active plugin registration",
                    ));
                }
            }

            let mut catalog = self.catalog.write();
            let own_tool = |k: &str| plugins.values().any(|p| p.tools.contains(k));
            let own_res = |k: &str| plugins.values().any(|p| p.resources.contains(k));
            let own_tpl = |k: &str| plugins.values().any(|p| p.templates.contains(k));
            let own_pr = |k: &str| plugins.values().any(|p| p.prompts.contains(k));
            let own_cmp = |k: &str| plugins.values().any(|p| p.completions.contains(k));

            let tools_changed =
                diff_non_plugin(&catalog.tools, &staged_tools, own_tool, |v| &v.definition)
                    || !staged_tools.is_empty();
            let resources_changed =
                diff_non_plugin(&catalog.resources, &staged_resources, own_res, |(r, _)| r)
                    || diff_non_plugin(&catalog.templates, &staged_templates, own_tpl, |(t, _)| t);
            let prompts_changed =
                diff_non_plugin(&catalog.prompts, &staged_prompts, own_pr, |(p, _)| p);
            let updated_uris: Vec<String> = staged_resources
                .keys()
                .filter(|u| catalog.resources.contains_key(*u) && !own_res(u))
                .cloned()
                .collect();

            retain_plugin_owned(&mut staged_tools, &catalog.tools, own_tool);
            retain_plugin_owned(&mut staged_resources, &catalog.resources, own_res);
            retain_plugin_owned(&mut staged_templates, &catalog.templates, own_tpl);
            retain_plugin_owned(&mut staged_prompts, &catalog.prompts, own_pr);
            retain_plugin_owned(&mut staged_completions, &catalog.completions, own_cmp);

            let resource_guard = update
                .resource_guard
                .unwrap_or_else(|| catalog.resource_guard.clone());
            *catalog = Catalog {
                tools: staged_tools,
                resources: staged_resources,
                templates: staged_templates,
                prompts: staged_prompts,
                completions: staged_completions,
                resource_guard,
            };
            ReloadReport {
                tools_changed,
                resources_changed,
                prompts_changed,
                updated_uris,
            }
        };

        self.emit_catalog_changes(
            report.tools_changed,
            report.resources_changed,
            report.prompts_changed,
        );
        for uri in &report.updated_uris {
            let _ = self.changes.send(Change::Resource(uri.clone()));
        }
        Ok(report)
    }

    fn emit_catalog_changes(&self, tools: bool, resources: bool, prompts: bool) {
        for (changed, ev) in [
            (tools, Change::Tools),
            (resources, Change::Resources),
            (prompts, Change::Prompts),
        ] {
            if changed {
                let _ = self.changes.send(ev);
            }
        }
    }

    pub fn attach_plugin(
        &self,
        bundle: PluginBundle,
        policy: &PluginPolicy,
    ) -> Result<PluginLoadedReport, PluginError> {
        validate_manifest(&bundle.manifest).map_err(PluginError::InvalidManifest)?;
        let plugin_id = bundle.manifest.id.clone();

        let verified_digests = match &bundle.pack_root {
            Some(root) => {
                let (disk_manifest, digests) =
                    verify_pack_dir_for_sdk(root).map_err(PluginError::IntegrityFailed)?;
                if disk_manifest != bundle.manifest {
                    return Err(PluginError::IntegrityFailed(format!(
                        "plugin `{plugin_id}` manifest drift vs disk"
                    )));
                }
                digests
            }
            None => BTreeMap::new(),
        };

        let missing = |req: &[String], have: &BTreeSet<String>| -> Vec<String> {
            req.iter().filter(|x| !have.contains(*x)).cloned().collect()
        };
        let missing_perms = missing(&bundle.manifest.permissions, &policy.granted_permissions);
        if !missing_perms.is_empty() {
            return Err(PluginError::PermissionDenied(plugin_id, missing_perms));
        }
        let missing_caps = missing(&bundle.manifest.requires, &policy.host_capabilities);
        if !missing_caps.is_empty() {
            return Err(PluginError::IncompatibleCapability(plugin_id, missing_caps));
        }
        let missing_optional = missing(&bundle.manifest.optional, &policy.host_capabilities);

        let cancel = CancellationToken::new();
        let spawned_tasks: Arc<Mutex<Vec<(String, TaskPrincipal)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let mut staged_tools: BTreeMap<String, RegisteredTool> = BTreeMap::new();

        for (tool, handler) in bundle.tools {
            let name = tool.name.to_string();
            if staged_tools.contains_key(&name) {
                return Err(PluginError::Collision("tool", plugin_id, name));
            }
            let (input, output) = compile_plugin_tool_schemas(&plugin_id, &tool)?;
            let plugin_ct = cancel.clone();
            let wrapped: ToolHandler = Arc::new(move |req, ctx| {
                if plugin_ct.is_cancelled() {
                    return Box::pin(async { Err(invalid("Plugin is unloaded")) });
                }
                handler(req, ctx)
            });
            staged_tools.insert(
                name,
                RegisteredTool {
                    definition: tool,
                    handler: wrapped,
                    input,
                    output,
                },
            );
        }

        for (tool, handler) in bundle.task_tools {
            let name = tool.name.to_string();
            if staged_tools.contains_key(&name) {
                return Err(PluginError::Collision("tool", plugin_id, name));
            }
            let (input, output) = compile_plugin_tool_schemas(&plugin_id, &tool)?;
            let (tasks, tracked, plugin_ct, out_check) = (
                self.tasks.clone(),
                Arc::clone(&spawned_tasks),
                cancel.clone(),
                output.clone(),
            );
            let task_wrapper: ToolHandler = Arc::new(move |request, context| {
                let (tasks, handler, tracked, plugin_ct, out_check) = (
                    tasks.clone(),
                    Arc::clone(&handler),
                    Arc::clone(&tracked),
                    plugin_ct.clone(),
                    out_check.clone(),
                );
                Box::pin(async move {
                    if plugin_ct.is_cancelled() {
                        return Err(invalid("Plugin is unloaded"));
                    }
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
                    let owner_record = owner.clone();
                    let task = tasks.try_spawn_for(owner, Default::default(), move |task_ctx| {
                        let (progress_lease, plugin_ct) =
                            (progress_lease.clone(), plugin_ct.clone());
                        Box::pin(async move {
                            let res = finish_task_run(&task_ctx, &progress_lease, None, async {
                                tokio::select! {
                                    () = plugin_ct.cancelled() => Err(TaskExit::Cancelled),
                                    r = handler(request, task_ctx.clone()) => r,
                                }
                            });
                            Ok(enforce_output_schema(
                                out_check.as_deref(),
                                res.await?,
                                "Task output does not match outputSchema",
                            ))
                        })
                    })?;
                    tracked
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((task.task_id.clone(), owner_record));
                    Ok(CreateTaskResult::new(task).into())
                })
            });
            staged_tools.insert(
                name,
                RegisteredTool {
                    definition: tool,
                    handler: task_wrapper,
                    input,
                    output,
                },
            );
        }

        let mut staged_ext_probe = ExtensionRegistry::new();
        for (ext_id, status, version, settings) in &bundle.extensions {
            staged_ext_probe
                .register(ext_id, *status, *version, settings.clone())
                .map_err(|e| match e {
                    ExtensionError::Collision(k) => {
                        PluginError::Collision("extension", plugin_id.clone(), k)
                    }
                    ExtensionError::NotNamespaced(_)
                    | ExtensionError::RequiredUnsupported(_)
                    | ExtensionError::Downgrade { .. } => {
                        PluginError::ExtensionRejected(plugin_id.clone(), e.message())
                    }
                })?;
        }

        let (emit_tools, emit_resources, emit_prompts, version) = {
            let mut plugins = self.plugins.write();
            if plugins.contains_key(&plugin_id) {
                return Err(PluginError::AlreadyLoaded(plugin_id));
            }
            let mut cat = self.catalog.write();
            let mut exts = self.extensions.write();
            let pid = &plugin_id;
            ensure_no_collision(pid, "tool", staged_tools.keys().cloned(), &cat.tools)?;
            let r_keys = bundle.resources.iter().map(|(r, _)| r.uri.clone());
            ensure_no_collision(pid, "resource", r_keys, &cat.resources)?;
            let t_keys = bundle.templates.iter().map(|(t, _)| t.uri_template.clone());
            ensure_no_collision(pid, "template", t_keys, &cat.templates)?;
            let p_keys = bundle.prompts.iter().map(|(p, _)| p.name.clone());
            ensure_no_collision(pid, "prompt", p_keys, &cat.prompts)?;
            let c_keys = bundle.completions.iter().map(|(k, _)| k.clone());
            ensure_no_collision(pid, "completion", c_keys, &cat.completions)?;
            for (ext_id, _, _, _) in &bundle.extensions {
                if exts.contains(ext_id) {
                    return Err(PluginError::Collision(
                        "extension",
                        plugin_id,
                        ext_id.clone(),
                    ));
                }
            }
            if let Some(init) = &bundle.initializer {
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| init(&cancel)));
                if let Err(reason) = ran.unwrap_or_else(|_| Err("initializer panicked".into())) {
                    cancel.cancel();
                    return Err(PluginError::InitFailed(plugin_id, reason));
                }
            }
            let tools = commit_owned(staged_tools, &mut cat.tools);
            let r_items = bundle
                .resources
                .into_iter()
                .map(|(r, h)| (r.uri.clone(), (r, h)));
            let resources = commit_owned(r_items, &mut cat.resources);
            let t_items = bundle
                .templates
                .into_iter()
                .map(|(t, h)| (t.uri_template.clone(), (t, h)));
            let templates = commit_owned(t_items, &mut cat.templates);
            let p_items = bundle
                .prompts
                .into_iter()
                .map(|(p, h)| (p.name.clone(), (p, h)));
            let prompts = commit_owned(p_items, &mut cat.prompts);
            let completions = commit_owned(bundle.completions, &mut cat.completions);
            let mut owned_exts = BTreeSet::new();
            for (ext_id, status, version, settings) in bundle.extensions {
                let _ = exts.register(&ext_id, status, version, settings);
                owned_exts.insert(ext_id);
            }
            let emit = (
                !tools.is_empty(),
                !resources.is_empty() || !templates.is_empty(),
                !prompts.is_empty(),
                bundle.manifest.version.clone(),
            );
            plugins.insert(
                plugin_id.clone(),
                LoadedPlugin {
                    manifest: bundle.manifest,
                    tools,
                    resources,
                    templates,
                    prompts,
                    completions,
                    extensions: owned_exts,
                    cancel,
                    spawned_tasks,
                    on_unload: bundle.on_unload,
                    host_synced: bundle.host_synced,
                },
            );
            emit
        };
        self.emit_catalog_changes(emit_tools, emit_resources, emit_prompts);
        Ok(PluginLoadedReport {
            id: plugin_id,
            version,
            missing_optional,
            verified_digests,
        })
    }

    pub fn unload_plugin(&self, plugin_id: &str) -> Result<PluginUnloadReport, PluginError> {
        fn rm_keys<V>(keys: &BTreeSet<String>, map: &mut BTreeMap<String, V>) -> usize {
            keys.iter().filter(|k| map.remove(*k).is_some()).count()
        }
        let loaded = self
            .plugins
            .write()
            .remove(plugin_id)
            .ok_or_else(|| PluginError::NotLoaded(plugin_id.to_string()))?;
        loaded.cancel.cancel();
        let spawned = std::mem::take(
            &mut *loaded
                .spawned_tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        let cancelled_tasks = spawned
            .into_iter()
            .filter(|(id, owner)| self.tasks.cancel_task_for(id, owner).is_ok())
            .count();
        let (removed_tools, removed_resources, removed_prompts, removed_extensions) = {
            let mut cat = self.catalog.write();
            let mut exts = self.extensions.write();
            let rt = rm_keys(&loaded.tools, &mut cat.tools);
            let rr = rm_keys(&loaded.resources, &mut cat.resources)
                + rm_keys(&loaded.templates, &mut cat.templates);
            let rp = rm_keys(&loaded.prompts, &mut cat.prompts);
            let _ = rm_keys(&loaded.completions, &mut cat.completions);
            let re = loaded
                .extensions
                .iter()
                .filter(|k| exts.unregister(k))
                .count();
            (rt, rr, rp, re)
        };
        if let Some(cb) = loaded.on_unload {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cb()));
        }
        self.emit_catalog_changes(
            removed_tools > 0,
            removed_resources > 0,
            removed_prompts > 0,
        );
        Ok(PluginUnloadReport {
            id: plugin_id.to_string(),
            removed_tools,
            removed_resources,
            removed_prompts,
            removed_extensions,
            cancelled_tasks,
        })
    }

    pub fn sync_active_pack(
        &self,
        policy: &PluginPolicy,
        decorate: impl FnOnce(PluginBundle) -> PluginBundle,
    ) -> Result<Option<PluginLoadedReport>, PluginError> {
        use crate::susi_config::extensions::{
            active_pack, invalidate_extension_caches, DEFAULT_PACK_ID,
        };
        invalidate_extension_caches();
        let pack = active_pack();
        let active_id = (pack.id != DEFAULT_PACK_ID).then_some(pack.id.as_str());
        let stale: Vec<String> = self
            .plugins
            .read()
            .iter()
            .filter(|(id, p)| p.host_synced && active_id != Some(id.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            let _ = self.unload_plugin(&id);
        }
        if active_id.is_none() {
            return Ok(None);
        }
        if self.plugins.read().contains_key(&pack.id) {
            if let Err(reason) = verify_pack_dir_for_sdk(&pack.root) {
                let _ = self.unload_plugin(&pack.id);
                return Err(PluginError::IntegrityFailed(reason));
            }
            return Ok(None);
        }
        let mut bundle = decorate(PluginBundle::from_pack_dir(&pack.root)?);
        bundle.host_synced = true;
        self.attach_plugin(bundle, policy).map(Some)
    }

    pub fn call_tool_sync(&self, name: &str, args: Value) -> Result<Value, String> {
        let (svc, tool) = (self.clone(), name.to_string());
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(async move {
                let h = TestHarness::spawn(svc);
                let p = json!({ "name": tool, "arguments": args });
                let out = h.request_modern("tools/call", p, Default::default()).await;
                h.shutdown().await;
                out
            })
        })
        .join()
        .map_err(|_| "direct tool worker thread panicked".to_string())?
    }

    pub fn bind_direct_server(&self, server: &str) -> Result<(), ErrorData> {
        if matches!(
            server,
            "" | "susi" | "native" | "gmcp" | "local" | "builtin" | "core"
        ) {
            return Err(invalid(
                "direct server namespace must not shadow builtin tools",
            ));
        }
        crate::direct_call::clear_server_registrations(server);
        let tools: Vec<(String, Value)> = self
            .catalog
            .read()
            .tools
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    Value::Object((*v.definition.input_schema).clone()),
                )
            })
            .collect();
        for (name, schema) in tools {
            crate::direct_call::register_tool_schema(server, &name, schema);
            let (svc, tool_name) = (self.clone(), name.clone());
            let handler = Arc::new(move |args: &Value, _ws: &std::path::Path| {
                let reply = svc
                    .call_tool_sync(&tool_name, args.clone())
                    .map_err(susi_error::EaiError::protocol)?;
                if let Some(err) = reply.get("error").filter(|e| !e.is_null()) {
                    return Err(susi_error::EaiError::protocol(err.to_string()));
                }
                let res = &reply["result"];
                let text = res["content"][0]["text"]
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| res.to_string());
                (res["isError"] != json!(true))
                    .then_some(text)
                    .ok_or_else(|| {
                        susi_error::EaiError::protocol(
                            res["content"][0]["text"].as_str().unwrap_or("tool error"),
                        )
                    })
            });
            crate::direct_call::register_tool_handler(server, &name, handler);
        }
        Ok(())
    }

    pub fn unbind_direct_server(server: &str) {
        crate::direct_call::clear_server_registrations(server);
    }
}

// ── Deterministic In-Memory TestHarness (SDK-012) ────────────────────────

pub struct TestHarness {
    service: GmcpService,
    writer: tokio::sync::Mutex<tokio::io::WriteHalf<tokio::io::DuplexStream>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    notifications: mpsc::UnboundedReceiver<Value>,
    next_id: AtomicI64,
    server_task: tokio::task::JoinHandle<()>,
    reader_task: tokio::task::JoinHandle<()>,
}

impl TestHarness {
    pub fn spawn(service: GmcpService) -> Self {
        Self::spawn_with_limit(service, crate::mcp::transport::DEFAULT_MAX_LINE_LENGTH)
    }

    pub fn spawn_with_limit(service: GmcpService, max_line_bytes: usize) -> Self {
        let (client_io, server_io) = tokio::io::duplex(65_536);
        let (server_read, server_write) = tokio::io::split(server_io);
        let (client_read, client_write) = tokio::io::split(client_io);

        let session_service = service.session();
        let server_task = tokio::spawn(async move {
            let transport = crate::jsonrpc_gate::StdioGate::new(
                crate::stdio::BoundedLines::new(server_read, max_line_bytes),
                server_write,
            );
            if let Ok(running) = session_service.serve(transport).await {
                let _ = running.waiting().await;
            }
        });

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = Arc::clone(&pending);
        let (notif_tx, notif_rx) = mpsc::unbounded_channel();

        let reader_task = tokio::spawn(async move {
            let mut lines = BufReader::new(client_read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(val) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let is_resp = val.get("method").is_none()
                    && (val.get("result").is_some() || val.get("error").is_some());
                if is_resp {
                    let key = val.get("id").map(ToString::to_string).unwrap_or_default();
                    if let Some(tx) = pending_reader
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&key)
                    {
                        let _ = tx.send(val);
                    } else {
                        let _ = notif_tx.send(val);
                    }
                } else {
                    let _ = notif_tx.send(val);
                }
            }
        });

        Self {
            service,
            writer: tokio::sync::Mutex::new(client_write),
            pending,
            notifications: notif_rx,
            next_id: AtomicI64::new(1),
            server_task,
            reader_task,
        }
    }

    pub fn service(&self) -> &GmcpService {
        &self.service
    }

    pub async fn request_raw(&self, request: Value) -> Result<Value, String> {
        let key = request
            .get("id")
            .map(ToString::to_string)
            .unwrap_or_else(|| "null".to_string());
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone(), tx);
        let mut bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        {
            let mut w = self.writer.lock().await;
            w.write_all(&bytes).await.map_err(|e| e.to_string())?;
            w.flush().await.map_err(|e| e.to_string())?;
        }
        match tokio::time::timeout(Duration::from_secs(5), rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err("harness channel closed before response".into()),
            Err(_) => {
                self.pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&key);
                Err(format!("timed out awaiting response for id {key}"))
            }
        }
    }

    pub async fn request_modern(
        &self,
        method: &str,
        mut params: Value,
        capabilities: ClientCapabilities,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if !params.is_object() {
            params = json!({});
        }
        let caps = serde_json::to_value(capabilities).map_err(|e| e.to_string())?;
        if let Some(obj) = params.as_object_mut() {
            let meta = obj.entry("_meta").or_insert_with(|| json!({}));
            if let Some(m) = meta.as_object_mut() {
                m.entry(META_PROTOCOL_VERSION)
                    .or_insert_with(|| json!("2026-07-28"));
                m.entry(META_CLIENT_CAPABILITIES).or_insert(caps);
            }
        }
        self.request_raw(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await
    }

    pub async fn initialize_legacy(
        &self,
        protocol_version: &str,
        capabilities: ClientCapabilities,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let init = self
            .request_raw(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": {
                    "protocolVersion": protocol_version,
                    "capabilities": capabilities,
                    "clientInfo": { "name": "sdk-test-harness", "version": "1.0.0" }
                }
            }))
            .await?;
        self.notify("notifications/initialized", json!({})).await?;
        Ok(init)
    }

    pub async fn request_session(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.request_raw(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let mut bytes =
            serde_json::to_vec(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
                .map_err(|e| e.to_string())?;
        bytes.push(b'\n');
        let mut w = self.writer.lock().await;
        w.write_all(&bytes).await.map_err(|e| e.to_string())?;
        w.flush().await.map_err(|e| e.to_string())
    }

    pub async fn next_notification(&mut self, timeout: Duration) -> Option<Value> {
        tokio::time::timeout(timeout, self.notifications.recv())
            .await
            .ok()
            .flatten()
    }

    pub async fn shutdown(self) {
        drop(self.writer);
        let _ = tokio::time::timeout(Duration::from_millis(250), self.server_task).await;
        self.reader_task.abort();
    }
}
