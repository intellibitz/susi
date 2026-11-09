//! The MCP wire model GMCP serves: protocol versions, JSON-RPC ids and
//! errors, capabilities, tools, content, resources, prompts, completions,
//! subscriptions, tasks and the multi-round-trip (MRTR) result.
//!
//! Field names, optional-field omission and parse strictness follow the
//! revisions GMCP negotiates (2024-11-05 through 2026-07-28), so a value
//! parses here exactly when a conforming peer's would.
#![allow(deprecated)]

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::{borrow::Cow, collections::BTreeMap, sync::Arc};

pub type JsonObject<F = Value> = serde_json::Map<String, F>;

/// `serde_json::Value` with an object root.
pub fn object(value: Value) -> JsonObject {
    if let Value::Object(map) = value {
        map
    } else {
        JsonObject::new()
    }
}

// ── Protocol versions ────────────────────────────────────────────────────

#[derive(Debug, Clone, Eq, PartialEq, Hash, PartialOrd)]
pub struct ProtocolVersion(Cow<'static, str>);

impl Default for ProtocolVersion {
    fn default() -> Self {
        Self::LATEST
    }
}

impl std::fmt::Display for ProtocolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl ProtocolVersion {
    pub const V_2026_07_28: Self = Self(Cow::Borrowed("2026-07-28"));
    pub const V_2025_11_25: Self = Self(Cow::Borrowed("2025-11-25"));
    pub const V_2025_06_18: Self = Self(Cow::Borrowed("2025-06-18"));
    pub const V_2025_03_26: Self = Self(Cow::Borrowed("2025-03-26"));
    pub const V_2024_11_05: Self = Self(Cow::Borrowed("2024-11-05"));
    pub const LATEST: Self = Self::V_2026_07_28;
    /// The first revision that requires the standard `Mcp-*` HTTP headers.
    pub const STANDARD_HEADERS: Self = Self::V_2026_07_28;
    /// The first revision without an `initialize` handshake.
    pub const NO_INITIALIZE: Self = Self::V_2026_07_28;
    /// Every revision this server implements, oldest first.
    pub const KNOWN_VERSIONS: &'static [Self] = &[
        Self::V_2024_11_05,
        Self::V_2025_03_26,
        Self::V_2025_06_18,
        Self::V_2025_11_25,
        Self::V_2026_07_28,
    ];

    pub fn new(version: impl Into<String>) -> Self {
        Self(Cow::Owned(version.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revisions before 2026-07-28 negotiate once, in `initialize`.
    pub fn has_initialize(&self) -> bool {
        self.as_str() < Self::NO_INITIALIZE.as_str()
    }
}

impl Serialize for ProtocolVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::KNOWN_VERSIONS
            .iter()
            .find(|v| v.as_str() == s)
            .cloned()
            .unwrap_or(Self(Cow::Owned(s))))
    }
}

// ── JSON-RPC ids and progress tokens ─────────────────────────────────────

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum NumberOrString {
    Number(i64),
    String(Arc<str>),
}

pub type RequestId = NumberOrString;

impl NumberOrString {
    pub fn into_json_value(self) -> Value {
        match self {
            Self::Number(n) => Value::from(n),
            Self::String(s) => Value::String(s.to_string()),
        }
    }
}

impl std::fmt::Display for NumberOrString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(n) => n.fmt(f),
            Self::String(s) => s.fmt(f),
        }
    }
}

impl Serialize for NumberOrString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Number(n) => n.serialize(serializer),
            Self::String(s) => s.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for NumberOrString {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        match Value::deserialize(deserializer)? {
            Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Ok(Self::Number(i))
                } else if n.as_u64().is_some() {
                    Err(D::Error::custom("Number too large for i64"))
                } else {
                    // `1.0` and `1e2` are integers written in float form.
                    const MAX_EXACT: f64 = 9_007_199_254_740_992.0;
                    match n.as_f64() {
                        Some(f) if f.fract() == 0.0 && f.abs() < MAX_EXACT => {
                            Ok(Self::Number(f as i64))
                        }
                        _ => Err(D::Error::custom("Expected an integer")),
                    }
                }
            }
            Value::String(s) => Ok(Self::String(s.into())),
            Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
                Err(D::Error::custom("Expect number or string"))
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Hash, Eq)]
#[serde(transparent)]
pub struct ProgressToken(pub NumberOrString);

// ── Errors ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct ErrorCode(pub i32);

impl ErrorCode {
    pub const UNSUPPORTED_PROTOCOL_VERSION: Self = Self(-32022);
    pub const MISSING_REQUIRED_CLIENT_CAPABILITY: Self = Self(-32021);
    pub const HEADER_MISMATCH: Self = Self(-32020);
    pub const RESOURCE_NOT_FOUND: Self = Self(-32002);
    pub const INVALID_REQUEST: Self = Self(-32600);
    pub const METHOD_NOT_FOUND: Self = Self(-32601);
    pub const INVALID_PARAMS: Self = Self(-32602);
    pub const INTERNAL_ERROR: Self = Self(-32603);
    pub const PARSE_ERROR: Self = Self(-32700);
}

/// A JSON-RPC error object.
#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ErrorData {
    pub code: ErrorCode,
    pub message: Cow<'static, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl std::fmt::Display for ErrorData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code.0, self.message)?;
        if let Some(data) = &self.data {
            write!(f, "({data})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ErrorData {}

impl ErrorData {
    pub fn new(
        code: ErrorCode,
        message: impl Into<Cow<'static, str>>,
        data: Option<Value>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
    pub fn resource_not_found(message: impl Into<Cow<'static, str>>, data: Option<Value>) -> Self {
        Self::new(ErrorCode::RESOURCE_NOT_FOUND, message, data)
    }
    pub fn header_mismatch(message: impl Into<Cow<'static, str>>, data: Option<Value>) -> Self {
        Self::new(ErrorCode::HEADER_MISMATCH, message, data)
    }
    pub fn unsupported_protocol_version(
        requested: ProtocolVersion,
        supported: &[ProtocolVersion],
    ) -> Self {
        Self::new(
            ErrorCode::UNSUPPORTED_PROTOCOL_VERSION,
            "Unsupported protocol version",
            Some(serde_json::json!({"requested": requested, "supported": supported})),
        )
    }
    pub fn missing_required_client_capability(required: ClientCapabilities) -> Self {
        Self::new(
            ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY,
            "Missing required client capability",
            Some(serde_json::json!({"requiredCapabilities": required})),
        )
    }
    pub fn parse_error(message: impl Into<Cow<'static, str>>, data: Option<Value>) -> Self {
        Self::new(ErrorCode::PARSE_ERROR, message, data)
    }
    pub fn invalid_request(message: impl Into<Cow<'static, str>>, data: Option<Value>) -> Self {
        Self::new(ErrorCode::INVALID_REQUEST, message, data)
    }
    /// `-32601` naming the method that has no handler.
    pub fn method_not_found(method: impl Into<Cow<'static, str>>) -> Self {
        Self::new(ErrorCode::METHOD_NOT_FOUND, method, None)
    }
    pub fn invalid_params(message: impl Into<Cow<'static, str>>, data: Option<Value>) -> Self {
        Self::new(ErrorCode::INVALID_PARAMS, message, data)
    }
    pub fn internal_error(message: impl Into<Cow<'static, str>>, data: Option<Value>) -> Self {
        Self::new(ErrorCode::INTERNAL_ERROR, message, data)
    }
}

// ── Result types and cache hints ─────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultType(Cow<'static, str>);

impl ResultType {
    pub const COMPLETE: Self = Self(Cow::Borrowed("complete"));
    pub const INPUT_REQUIRED: Self = Self(Cow::Borrowed("input_required"));
    pub const TASK: Self = Self(Cow::Borrowed("task"));
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_complete(&self) -> bool {
        self.0 == "complete"
    }
    pub fn is_input_required(&self) -> bool {
        self.0 == "input_required"
    }
    pub fn is_task(&self) -> bool {
        self.0 == "task"
    }
}

impl Default for ResultType {
    fn default() -> Self {
        Self::COMPLETE
    }
}

impl Serialize for ResultType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResultType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "complete" => Self::COMPLETE,
            "input_required" => Self::INPUT_REQUIRED,
            _ => Self(Cow::Owned(s)),
        })
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheScope {
    #[default]
    Public,
    Private,
}

fn deserialize_ttl_ms<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<u64>, D::Error> {
    Ok(Option::<i64>::deserialize(deserializer)?.map(|ttl| ttl.max(0) as u64))
}

fn deserialize_cache_scope<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CacheScope>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(value) => CacheScope::deserialize(value)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

fn option_f32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f32>, D::Error> {
    Ok(Option::<serde_json::Number>::deserialize(deserializer)?
        .and_then(|n| n.as_f64())
        .map(|f| f as f32))
}

fn option_f64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<f64>, D::Error> {
    Ok(Option::<serde_json::Number>::deserialize(deserializer)?.and_then(|n| n.as_f64()))
}

fn json_f64<'de, D: Deserializer<'de>>(deserializer: D) -> Result<f64, D::Error> {
    serde_json::Number::deserialize(deserializer)?
        .as_f64()
        .ok_or_else(|| serde::de::Error::custom("number out of range"))
}

// ── Metadata objects ─────────────────────────────────────────────────────

pub const META_PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
pub const META_CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
pub const META_CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
pub const META_LOG_LEVEL: &str = "io.modelcontextprotocol/logLevel";
pub const META_SUBSCRIPTION_ID: &str = "io.modelcontextprotocol/subscriptionId";
pub const META_SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(transparent)]
pub struct MetaObject(pub JsonObject);

impl MetaObject {
    pub fn new() -> Self {
        Self::default()
    }
    fn decode<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        self.0.get(key).and_then(|v| T::deserialize(v).ok())
    }
    fn set_serialized<T: Serialize>(&mut self, key: &str, value: T) {
        if let Ok(value) = serde_json::to_value(value) {
            self.0.insert(key.to_string(), value);
        }
    }
}

impl std::ops::Deref for MetaObject {
    type Target = JsonObject;
    fn deref(&self) -> &JsonObject {
        &self.0
    }
}

impl std::ops::DerefMut for MetaObject {
    fn deref_mut(&mut self) -> &mut JsonObject {
        &mut self.0
    }
}

/// `params._meta` of a request.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(transparent)]
pub struct RequestMetaObject(pub MetaObject);

impl RequestMetaObject {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn from_object(object: JsonObject) -> Self {
        Self(MetaObject(object))
    }
    pub fn get_progress_token(&self) -> Option<ProgressToken> {
        self.0.decode("progressToken")
    }
    pub fn set_progress_token(&mut self, token: ProgressToken) {
        self.0.set_serialized("progressToken", token);
    }
    pub fn protocol_version(&self) -> Option<ProtocolVersion> {
        self.0.decode(META_PROTOCOL_VERSION)
    }
    pub fn client_info(&self) -> Option<Implementation> {
        self.0.decode(META_CLIENT_INFO)
    }
    pub fn client_capabilities(&self) -> Option<ClientCapabilities> {
        self.0.decode(META_CLIENT_CAPABILITIES)
    }
    pub fn log_level(&self) -> Option<LoggingLevel> {
        self.0.decode(META_LOG_LEVEL)
    }
    /// The 2026-07-28 per-request keys this metadata lacks (or carries
    /// malformed); empty for earlier revisions.
    pub fn missing_required_keys(&self, version: &ProtocolVersion) -> Vec<&'static str> {
        if version.as_str() < ProtocolVersion::V_2026_07_28.as_str() {
            return Vec::new();
        }
        let mut missing = Vec::new();
        if self.protocol_version().is_none() {
            missing.push(META_PROTOCOL_VERSION);
        }
        if self.client_capabilities().is_none() {
            missing.push(META_CLIENT_CAPABILITIES);
        }
        missing
    }
}

impl std::ops::Deref for RequestMetaObject {
    type Target = MetaObject;
    fn deref(&self) -> &MetaObject {
        &self.0
    }
}

impl std::ops::DerefMut for RequestMetaObject {
    fn deref_mut(&mut self) -> &mut MetaObject {
        &mut self.0
    }
}

/// `params._meta` of a notification.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq)]
#[serde(transparent)]
pub struct NotificationMetaObject(pub MetaObject);

impl NotificationMetaObject {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn subscription_id(&self) -> Option<RequestId> {
        self.0.decode(META_SUBSCRIPTION_ID)
    }
    pub fn set_subscription_id(&mut self, id: RequestId) {
        self.0.set_serialized(META_SUBSCRIPTION_ID, id);
    }
}

impl std::ops::Deref for NotificationMetaObject {
    type Target = MetaObject;
    fn deref(&self) -> &MetaObject {
        &self.0
    }
}

// ── Implementation info and capabilities ─────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Hash, Copy)]
#[serde(rename_all = "lowercase")]
pub enum IconTheme {
    Light,
    Dark,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Icon {
    pub src: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<IconTheme>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub website_url: Option<String>,
}

impl Implementation {
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            title: None,
            version: version.into(),
            description: None,
            icons: None,
            website_url: None,
        }
    }
}

impl Default for Implementation {
    fn default() -> Self {
        Self::new("susi_gmcp", env!("CARGO_PKG_VERSION"))
    }
}

pub type ExperimentalCapabilities = BTreeMap<String, JsonObject>;
pub type ExtensionCapabilities = BTreeMap<String, JsonObject>;

/// The tasks extension a client declares to receive task handles.
pub const TASKS_EXTENSION_ID: &str = "io.modelcontextprotocol/tasks";

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PromptsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscribe: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolsCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct RootsCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_changed: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FormElicitationCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_validation: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct UrlElicitationCapability {}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ElicitationCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub form: Option<FormElicitationCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<UrlElicitationCapability>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct SamplingCapability {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<JsonObject>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct ClientCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<ExperimentalCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ExtensionCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roots: Option<RootsCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sampling: Option<SamplingCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elicitation: Option<ElicitationCapability>,
}

impl ClientCapabilities {
    pub fn builder() -> ClientCapabilitiesBuilder {
        ClientCapabilitiesBuilder::default()
    }
    pub fn supports_tasks(&self) -> bool {
        self.extensions
            .as_ref()
            .is_some_and(|e| e.contains_key(TASKS_EXTENSION_ID))
    }
}

#[derive(Debug, Default)]
pub struct ClientCapabilitiesBuilder(ClientCapabilities);

impl ClientCapabilitiesBuilder {
    pub fn enable_tasks(mut self) -> Self {
        self.0
            .extensions
            .get_or_insert_with(Default::default)
            .insert(TASKS_EXTENSION_ID.to_string(), JsonObject::new());
        self
    }
    pub fn build(self) -> ClientCapabilities {
        self.0
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental: Option<ExperimentalCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extensions: Option<ExtensionCapabilities>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logging: Option<JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completions: Option<JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompts: Option<PromptsCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolsCapability>,
}

impl ServerCapabilities {
    pub fn builder() -> ServerCapabilitiesBuilder {
        ServerCapabilitiesBuilder::default()
    }
    pub fn supports_tasks(&self) -> bool {
        self.extensions
            .as_ref()
            .is_some_and(|e| e.contains_key(TASKS_EXTENSION_ID))
    }
}

#[derive(Debug, Default)]
pub struct ServerCapabilitiesBuilder(ServerCapabilities);

impl ServerCapabilitiesBuilder {
    pub fn enable_tools(mut self) -> Self {
        self.0.tools = Some(ToolsCapability::default());
        self
    }
    pub fn enable_tool_list_changed(mut self) -> Self {
        if let Some(tools) = self.0.tools.as_mut() {
            tools.list_changed = Some(true);
        }
        self
    }
    pub fn enable_resources(mut self) -> Self {
        self.0.resources = Some(ResourcesCapability::default());
        self
    }
    pub fn enable_resources_list_changed(mut self) -> Self {
        if let Some(resources) = self.0.resources.as_mut() {
            resources.list_changed = Some(true);
        }
        self
    }
    pub fn enable_resources_subscribe(mut self) -> Self {
        if let Some(resources) = self.0.resources.as_mut() {
            resources.subscribe = Some(true);
        }
        self
    }
    pub fn enable_prompts(mut self) -> Self {
        self.0.prompts = Some(PromptsCapability::default());
        self
    }
    pub fn enable_prompts_list_changed(mut self) -> Self {
        if let Some(prompts) = self.0.prompts.as_mut() {
            prompts.list_changed = Some(true);
        }
        self
    }
    pub fn enable_completions(mut self) -> Self {
        self.0.completions = Some(JsonObject::new());
        self
    }
    pub fn enable_logging(mut self) -> Self {
        self.0.logging = Some(JsonObject::new());
        self
    }
    pub fn enable_tasks(mut self) -> Self {
        self.0
            .extensions
            .get_or_insert_with(Default::default)
            .insert(TASKS_EXTENSION_ID.to_string(), JsonObject::new());
        self
    }
    pub fn build(self) -> ServerCapabilities {
        self.0
    }
}

// ── Lifecycle: initialize and discover ───────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequestParams {
    pub protocol_version: ProtocolVersion,
    pub capabilities: ClientCapabilities,
    pub client_info: Implementation,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: ProtocolVersion,
    pub capabilities: ServerCapabilities,
    pub server_info: Implementation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

pub type ServerConfig = InitializeResult;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverResult {
    pub result_type: ResultType,
    pub supported_versions: Vec<ProtocolVersion>,
    pub capabilities: ServerCapabilities,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub ttl_ms: u64,
    pub cache_scope: CacheScope,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

fn set_server_info(meta: &mut MetaObject, server_info: Implementation) {
    meta.set_serialized(META_SERVER_INFO, server_info);
}

impl DiscoverResult {
    pub fn from_server_info(
        supported_versions: Vec<ProtocolVersion>,
        config: ServerConfig,
    ) -> Self {
        let mut meta = config.meta.unwrap_or_default();
        set_server_info(&mut meta, config.server_info);
        Self {
            result_type: ResultType::COMPLETE,
            supported_versions,
            capabilities: config.capabilities,
            instructions: config.instructions,
            ttl_ms: 0,
            cache_scope: CacheScope::Private,
            meta: Some(meta),
        }
    }
    pub fn with_ttl_ms(mut self, ttl_ms: u64) -> Self {
        self.ttl_ms = ttl_ms;
        self
    }
    pub fn with_cache_scope(mut self, cache_scope: CacheScope) -> Self {
        self.cache_scope = cache_scope;
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmptyObject {}

pub type EmptyResult = EmptyObject;

// ── Pagination ───────────────────────────────────────────────────────────

pub type Cursor = String;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedRequestParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

macro_rules! paginated_result {
    ($name:ident { $field:ident : $ty:ty }) => {
        #[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub result_type: Option<ResultType>,
            #[serde(rename = "_meta", default, skip_serializing_if = "Option::is_none")]
            pub meta: Option<MetaObject>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub next_cursor: Option<Cursor>,
            #[serde(
                default,
                deserialize_with = "deserialize_ttl_ms",
                skip_serializing_if = "Option::is_none"
            )]
            pub ttl_ms: Option<u64>,
            #[serde(
                default,
                deserialize_with = "deserialize_cache_scope",
                skip_serializing_if = "Option::is_none"
            )]
            pub cache_scope: Option<CacheScope>,
            pub $field: $ty,
        }
        impl Default for $name {
            fn default() -> Self {
                Self {
                    result_type: Some(ResultType::COMPLETE),
                    meta: None,
                    next_cursor: None,
                    ttl_ms: None,
                    cache_scope: None,
                    $field: Default::default(),
                }
            }
        }
        impl $name {
            pub fn with_ttl_ms(mut self, ttl_ms: u64) -> Self {
                self.ttl_ms = Some(ttl_ms);
                self
            }
            pub fn with_cache_scope(mut self, cache_scope: CacheScope) -> Self {
                self.cache_scope = Some(cache_scope);
                self
            }
        }
    };
}

paginated_result!(ListToolsResult { tools: Vec<Tool> });
paginated_result!(ListResourcesResult { resources: Vec<Resource> });
paginated_result!(ListResourceTemplatesResult { resource_templates: Vec<ResourceTemplate> });
paginated_result!(ListPromptsResult { prompts: Vec<Prompt> });

// ── Tools ────────────────────────────────────────────────────────────────

mod arc_object {
    use super::JsonObject;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;
    pub fn serialize<S: Serializer>(value: &Arc<JsonObject>, s: S) -> Result<S::Ok, S::Error> {
        value.as_ref().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<JsonObject>, D::Error> {
        JsonObject::deserialize(d).map(Arc::new)
    }
}

mod option_arc_object {
    use super::JsonObject;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::sync::Arc;
    pub fn serialize<S: Serializer>(
        value: &Option<Arc<JsonObject>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_deref().serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Arc<JsonObject>>, D::Error> {
        Option::<JsonObject>::deserialize(d).map(|o| o.map(Arc::new))
    }
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: Cow<'static, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<Cow<'static, str>>,
    #[serde(with = "arc_object")]
    pub input_schema: Arc<JsonObject>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "option_arc_object"
    )]
    pub output_schema: Option<Arc<JsonObject>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<ToolAnnotations>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

impl Tool {
    pub fn new_with_raw(
        name: impl Into<Cow<'static, str>>,
        description: Option<Cow<'static, str>>,
        input_schema: impl Into<Arc<JsonObject>>,
    ) -> Self {
        Self {
            name: name.into(),
            description,
            input_schema: input_schema.into(),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ToolAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_only_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destructive_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotent_hint: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub open_world_hint: Option<bool>,
}

#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CallToolRequestParams {
    pub name: Cow<'static, str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_responses: Option<InputResponses>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_state: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CallToolResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<ResultType>,
    #[serde(default)]
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

impl<'de> Deserialize<'de> for CallToolResult {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
            Value::deserialize(d).map(Some)
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Helper {
            #[serde(default)]
            result_type: Option<ResultType>,
            content: Option<Vec<ContentBlock>>,
            #[serde(default, deserialize_with = "present")]
            structured_content: Option<Value>,
            is_error: Option<bool>,
            #[serde(rename = "_meta")]
            meta: Option<MetaObject>,
        }
        let h = Helper::deserialize(deserializer)?;
        if h.result_type.as_ref().is_some_and(|t| !t.is_complete()) {
            return Err(serde::de::Error::custom(
                "CallToolResult requires resultType to be \"complete\" when present",
            ));
        }
        if h.content.is_none()
            && h.structured_content.is_none()
            && h.is_error.is_none()
            && h.meta.is_none()
        {
            return Err(serde::de::Error::custom(
                "expected at least one known CallToolResult field (content, structuredContent, isError, or _meta)",
            ));
        }
        Ok(Self {
            result_type: h.result_type,
            content: h.content.unwrap_or_default(),
            structured_content: h.structured_content,
            is_error: h.is_error,
            meta: h.meta,
        })
    }
}

impl Default for CallToolResult {
    fn default() -> Self {
        Self {
            result_type: Some(ResultType::COMPLETE),
            content: Vec::new(),
            structured_content: None,
            is_error: None,
            meta: None,
        }
    }
}

impl CallToolResult {
    pub fn success(content: Vec<ContentBlock>) -> Self {
        Self {
            content,
            is_error: Some(false),
            ..Self::default()
        }
    }
    pub fn error(content: Vec<ContentBlock>) -> Self {
        Self {
            content,
            is_error: Some(true),
            ..Self::default()
        }
    }
    pub fn structured(value: Value) -> Self {
        Self {
            content: vec![ContentBlock::text(value.to_string())],
            structured_content: Some(value),
            is_error: Some(false),
            ..Self::default()
        }
    }
    pub fn structured_error(value: Value) -> Self {
        Self {
            content: vec![ContentBlock::text(value.to_string())],
            structured_content: Some(value),
            is_error: Some(true),
            ..Self::default()
        }
    }
}

/// A `tools/call` answer: complete, a task handle, or a request for input.
#[derive(Debug, Clone)]
pub enum CallToolResponse {
    Complete(CallToolResult),
    InputRequired(InputRequiredResult),
    Task(CreateTaskResult),
}

impl From<CallToolResult> for CallToolResponse {
    fn from(result: CallToolResult) -> Self {
        Self::Complete(result)
    }
}

impl From<InputRequiredResult> for CallToolResponse {
    fn from(result: InputRequiredResult) -> Self {
        Self::InputRequired(result)
    }
}

impl From<CreateTaskResult> for CallToolResponse {
    fn from(result: CreateTaskResult) -> Self {
        Self::Task(result)
    }
}

// ── Content ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Annotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<Role>>,
    #[serde(
        default,
        deserialize_with = "option_f32",
        skip_serializing_if = "Option::is_none"
    )]
    pub priority: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "lastModified")]
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    pub text: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    pub data: String,
    pub mime_type: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

pub type AudioContent = ImageContent;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    pub resource: ResourceContents,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

/// One content block: text, image, audio, an embedded resource or a link.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text(TextContent),
    Image(ImageContent),
    Audio(AudioContent),
    Resource(EmbeddedResource),
    ResourceLink(Resource),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextContent {
            text: text.into(),
            meta: None,
            annotations: None,
        })
    }
    pub fn as_text(&self) -> Option<&TextContent> {
        if let Self::Text(text) = self {
            Some(text)
        } else {
            None
        }
    }
}

// ── Resources ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

impl Resource {
    pub fn new(uri: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            name: name.into(),
            title: None,
            description: None,
            mime_type: None,
            size: None,
            icons: None,
            meta: None,
            annotations: None,
        }
    }
    pub fn with_mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceTemplate {
    pub uri_template: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum ResourceContents {
    #[serde(rename_all = "camelCase")]
    TextResourceContents {
        uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        text: String,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<MetaObject>,
    },
    #[serde(rename_all = "camelCase")]
    BlobResourceContents {
        uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        blob: String,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<MetaObject>,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadResourceRequestParams {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_responses: Option<InputResponses>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_state: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReadResourceResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<ResultType>,
    #[serde(
        default,
        deserialize_with = "deserialize_ttl_ms",
        skip_serializing_if = "Option::is_none"
    )]
    pub ttl_ms: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_cache_scope",
        skip_serializing_if = "Option::is_none"
    )]
    pub cache_scope: Option<CacheScope>,
    pub contents: Vec<ResourceContents>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

#[derive(Debug, Clone)]
pub enum ReadResourceResponse {
    Complete(ReadResourceResult),
    InputRequired(InputRequiredResult),
}

impl From<ReadResourceResult> for ReadResourceResponse {
    fn from(result: ReadResourceResult) -> Self {
        Self::Complete(result)
    }
}

impl From<InputRequiredResult> for ReadResourceResponse {
    fn from(result: InputRequiredResult) -> Self {
        Self::InputRequired(result)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SubscribeRequestParams {
    pub uri: String,
}

pub type UnsubscribeRequestParams = SubscribeRequestParams;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceUpdatedNotificationParam {
    pub uri: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<NotificationMetaObject>,
}

impl ResourceUpdatedNotificationParam {
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            meta: None,
        }
    }
}

// ── Prompts and completion ───────────────────────────────────────────────

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<Icon>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptArgument {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromptMessage {
    pub role: Role,
    pub content: ContentBlock,
}

#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetPromptRequestParams {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<JsonObject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_responses: Option<InputResponses>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_state: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetPromptResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<ResultType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub messages: Vec<PromptMessage>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

#[derive(Debug, Clone)]
pub enum GetPromptResponse {
    Complete(GetPromptResult),
    InputRequired(InputRequiredResult),
}

impl From<GetPromptResult> for GetPromptResponse {
    fn from(result: GetPromptResult) -> Self {
        Self::Complete(result)
    }
}

impl From<InputRequiredResult> for GetPromptResponse {
    fn from(result: InputRequiredResult) -> Self {
        Self::InputRequired(result)
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(tag = "type")]
pub enum Reference {
    #[serde(rename = "ref/resource")]
    Resource(ResourceReference),
    #[serde(rename = "ref/prompt")]
    Prompt(PromptReference),
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ResourceReference {
    pub uri: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct PromptReference {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ArgumentInfo {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
pub struct CompletionContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct CompleteRequestParams {
    pub r#ref: Reference,
    pub argument: ArgumentInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<CompletionContext>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompletionInfo {
    pub values: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CompleteResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_type: Option<ResultType>,
    pub completion: CompletionInfo,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

// ── Logging, progress and cancellation ───────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Copy)]
#[serde(rename_all = "lowercase")]
pub enum LoggingLevel {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SetLevelRequestParams {
    pub level: LoggingLevel,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LoggingMessageNotificationParam {
    pub level: LoggingLevel,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logger: Option<String>,
    pub data: Value,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<NotificationMetaObject>,
}

impl LoggingMessageNotificationParam {
    pub fn new(level: LoggingLevel, data: Value) -> Self {
        Self {
            level,
            logger: None,
            data,
            meta: None,
        }
    }
    pub fn with_logger(mut self, logger: impl Into<String>) -> Self {
        self.logger = Some(logger.into());
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProgressNotificationParam {
    pub progress_token: ProgressToken,
    #[serde(deserialize_with = "json_f64")]
    pub progress: f64,
    #[serde(
        default,
        deserialize_with = "option_f64",
        skip_serializing_if = "Option::is_none"
    )]
    pub total: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<NotificationMetaObject>,
}

impl ProgressNotificationParam {
    pub fn new(progress_token: ProgressToken, progress: f64) -> Self {
        Self {
            progress_token,
            progress,
            total: None,
            message: None,
            meta: None,
        }
    }
    pub fn with_total(mut self, total: f64) -> Self {
        self.total = Some(total);
        self
    }
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = Some(message.into());
        self
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CancelledNotificationParam {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

// ── Subscriptions (2026-07-28) ───────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SubscriptionFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_list_changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompts_list_changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources_list_changed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_subscriptions: Option<Vec<String>>,
}

fn both(a: Option<bool>, b: Option<bool>) -> Option<bool> {
    (a == Some(true) && b == Some(true)).then_some(true)
}

impl SubscriptionFilter {
    /// What both filters ask for.
    pub fn intersection(&self, other: &Self) -> Self {
        let resource_subscriptions = self
            .resource_subscriptions
            .as_ref()
            .and_then(|requested| {
                other.resource_subscriptions.as_ref().map(|accepted| {
                    requested
                        .iter()
                        .filter(|uri| accepted.contains(uri))
                        .cloned()
                        .collect::<Vec<_>>()
                })
            })
            .filter(|uris| !uris.is_empty());
        Self {
            tools_list_changed: both(self.tools_list_changed, other.tools_list_changed),
            prompts_list_changed: both(self.prompts_list_changed, other.prompts_list_changed),
            resources_list_changed: both(self.resources_list_changed, other.resources_list_changed),
            resource_subscriptions,
        }
    }
    /// The part of this filter the advertised capabilities can deliver.
    pub fn supported_by(&self, capabilities: &ServerCapabilities) -> Self {
        let tools = capabilities.tools.as_ref().and_then(|c| c.list_changed);
        let prompts = capabilities.prompts.as_ref().and_then(|c| c.list_changed);
        let resources = capabilities.resources.as_ref();
        Self {
            tools_list_changed: both(self.tools_list_changed, tools),
            prompts_list_changed: both(self.prompts_list_changed, prompts),
            resources_list_changed: both(
                self.resources_list_changed,
                resources.and_then(|c| c.list_changed),
            ),
            resource_subscriptions: resources
                .is_some_and(|c| c.subscribe == Some(true))
                .then(|| self.resource_subscriptions.clone())
                .flatten(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct SubscriptionsListenRequestParams {
    pub notifications: SubscriptionFilter,
}

// ── Tasks ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Working,
    InputRequired,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub task_id: String,
    pub status: TaskStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    pub created_at: String,
    pub last_updated_at: String,
    pub ttl_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_interval_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskPayload {
    Working,
    InputRequired { input_requests: InputRequests },
    Completed { result: JsonObject },
    Failed { error: JsonObject },
    Cancelled,
}

impl TaskPayload {
    pub fn status(&self) -> TaskStatus {
        match self {
            Self::Working => TaskStatus::Working,
            Self::InputRequired { .. } => TaskStatus::InputRequired,
            Self::Completed { .. } => TaskStatus::Completed,
            Self::Failed { .. } => TaskStatus::Failed,
            Self::Cancelled => TaskStatus::Cancelled,
        }
    }
}

/// A task with its current payload (`tasks/get`).
#[derive(Debug, Clone, PartialEq)]
pub struct DetailedTask {
    pub task: Task,
    pub payload: TaskPayload,
}

impl DetailedTask {
    pub fn new(mut task: Task, payload: TaskPayload) -> Self {
        task.status = payload.status();
        Self { task, payload }
    }
    pub fn status(&self) -> TaskStatus {
        self.task.status
    }
}

impl Serialize for DetailedTask {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire<'a> {
            #[serde(flatten)]
            task: &'a Task,
            #[serde(skip_serializing_if = "Option::is_none")]
            input_requests: Option<&'a InputRequests>,
            #[serde(skip_serializing_if = "Option::is_none")]
            result: Option<&'a JsonObject>,
            #[serde(skip_serializing_if = "Option::is_none")]
            error: Option<&'a JsonObject>,
        }
        let (input_requests, result, error) = match &self.payload {
            TaskPayload::Working | TaskPayload::Cancelled => (None, None, None),
            TaskPayload::InputRequired { input_requests } => (Some(input_requests), None, None),
            TaskPayload::Completed { result } => (None, Some(result), None),
            TaskPayload::Failed { error } => (None, None, Some(error)),
        };
        Wire {
            task: &self.task,
            input_requests,
            result,
            error,
        }
        .serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTaskResult {
    pub result_type: ResultType,
    #[serde(flatten)]
    pub task: Task,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

impl CreateTaskResult {
    pub fn new(task: Task) -> Self {
        Self {
            result_type: ResultType::TASK,
            task,
            meta: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskResult {
    pub result_type: ResultType,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
    #[serde(flatten)]
    pub task: DetailedTask,
}

impl GetTaskResult {
    pub fn new(task: DetailedTask) -> Self {
        Self {
            result_type: ResultType::COMPLETE,
            meta: None,
            task,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskParams {
    pub task_id: String,
}

pub type CancelTaskParams = GetTaskParams;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UpdateTaskParams {
    pub task_id: String,
    pub input_responses: InputResponses,
}

// ── Multi-round-trip requests (2026-07-28) ───────────────────────────────

/// A server→client request carried inside a result: sampling, elicitation
/// or a roots listing, kept as the peer wrote it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct InputRequest(Value);

impl InputRequest {
    pub const METHODS: [&'static str; 3] =
        ["sampling/createMessage", "elicitation/create", "roots/list"];
    pub fn method(&self) -> &str {
        self.0.get("method").and_then(Value::as_str).unwrap_or("")
    }
}

impl<'de> Deserialize<'de> for InputRequest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        let method = value.get("method").and_then(Value::as_str);
        let params_ok = match value.get("params") {
            None => method == Some("roots/list"),
            Some(Value::Object(_)) => true,
            Some(Value::Null) => method == Some("roots/list"),
            Some(_) => false,
        };
        if !method.is_some_and(|m| Self::METHODS.contains(&m)) || !params_ok {
            return Err(serde::de::Error::custom(
                "input request must be sampling/createMessage, elicitation/create or roots/list",
            ));
        }
        Ok(Self(value))
    }
}

pub type InputRequests = BTreeMap<String, InputRequest>;
pub type InputResponses = BTreeMap<String, Value>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputRequiredResult {
    pub result_type: ResultType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_requests: Option<InputRequests>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_state: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<MetaObject>,
}

impl<'de> Deserialize<'de> for InputRequiredResult {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Helper {
            result_type: Option<ResultType>,
            input_requests: Option<InputRequests>,
            request_state: Option<String>,
            #[serde(rename = "_meta")]
            meta: Option<MetaObject>,
        }
        let h = Helper::deserialize(deserializer)?;
        if !h
            .result_type
            .as_ref()
            .is_some_and(ResultType::is_input_required)
        {
            return Err(serde::de::Error::custom(
                "InputRequiredResult requires resultType to be \"input_required\"",
            ));
        }
        if h.input_requests.is_none() && h.request_state.is_none() {
            return Err(serde::de::Error::custom(
                "InputRequiredResult requires at least one of inputRequests or requestState",
            ));
        }
        Ok(Self {
            result_type: ResultType::INPUT_REQUIRED,
            input_requests: h.input_requests,
            request_state: h.request_state,
            meta: h.meta,
        })
    }
}
