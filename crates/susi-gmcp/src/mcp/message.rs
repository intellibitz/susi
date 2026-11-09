//! JSON-RPC 2.0 envelopes and the typed MCP client requests and
//! notifications GMCP dispatches.
//!
//! Parsing is deliberately exact about what a request must look like: a
//! known method whose `params` do not fit its typed shape is treated as an
//! unknown (custom) method, which the dispatcher answers `-32601`; a
//! message no shape fits at all is rejected whole ("unrecognized JSON-RPC
//! message"), which the transports answer `-32600`.
#![allow(deprecated)]

use super::model::*;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::borrow::Cow;

const UNRECOGNIZED: &str = "unrecognized JSON-RPC message";

/// One typed client request.
#[derive(Debug, Clone)]
pub enum ClientRequest {
    Ping,
    Initialize(InitializeRequestParams),
    Discover,
    Complete(CompleteRequestParams),
    SetLevel(SetLevelRequestParams),
    GetPrompt(GetPromptRequestParams),
    ListPrompts(Option<PaginatedRequestParams>),
    ListResources(Option<PaginatedRequestParams>),
    ListResourceTemplates(Option<PaginatedRequestParams>),
    ReadResource(ReadResourceRequestParams),
    SubscriptionsListen(SubscriptionsListenRequestParams),
    Subscribe(SubscribeRequestParams),
    Unsubscribe(UnsubscribeRequestParams),
    CallTool(CallToolRequestParams),
    ListTools(Option<PaginatedRequestParams>),
    GetTask(GetTaskParams),
    UpdateTask(UpdateTaskParams),
    CancelTask(CancelTaskParams),
    Custom {
        method: String,
        params: Option<Value>,
    },
}

/// A request with its id, method name and `params._meta`.
#[derive(Debug, Clone)]
pub struct JsonRpcRequest {
    pub id: RequestId,
    pub method: String,
    pub request: ClientRequest,
    pub meta: RequestMetaObject,
}

/// One typed client notification.
#[derive(Debug, Clone)]
pub enum ClientNotificationKind {
    Cancelled(CancelledNotificationParam),
    Progress(ProgressNotificationParam),
    Initialized,
    RootsListChanged,
    Custom {
        method: String,
        params: Option<Value>,
    },
}

#[derive(Debug, Clone)]
pub struct ClientNotification {
    pub method: String,
    pub kind: ClientNotificationKind,
    pub meta: NotificationMetaObject,
}

/// Any message a client sends. A request is the large variant; messages
/// move a few times per request, so boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ClientJsonRpcMessage {
    Request(JsonRpcRequest),
    Notification(ClientNotification),
    Response {
        id: RequestId,
        result: Value,
    },
    Error {
        id: Option<RequestId>,
        error: ErrorData,
    },
}

impl ClientJsonRpcMessage {
    /// Parse one JSON-RPC message; `None` when no message shape fits.
    pub fn from_value(value: Value) -> Option<Self> {
        let Value::Object(mut map) = value else {
            return None;
        };
        let (method, id, result, error) = (
            map.contains_key("method"),
            map.contains_key("id"),
            map.contains_key("result"),
            map.contains_key("error"),
        );
        if map.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return None;
        }
        match (method, id, result, error) {
            (true, true, false, false) => {
                let id = RequestId::deserialize(map.remove("id")?).ok()?;
                let method = map.remove("method")?.as_str()?.to_string();
                let (request, meta) = parse_request(&method, map.remove("params"))?;
                Some(Self::Request(JsonRpcRequest {
                    id,
                    method,
                    request,
                    meta,
                }))
            }
            (true, false, false, false) => {
                let method = map.remove("method")?.as_str()?.to_string();
                let (kind, meta) = parse_notification(&method, map.remove("params"))?;
                Some(Self::Notification(ClientNotification {
                    method,
                    kind,
                    meta,
                }))
            }
            (false, _, true, false) => {
                let id = RequestId::deserialize(map.remove("id")?).ok()?;
                Some(Self::Response {
                    id,
                    result: map.remove("result")?,
                })
            }
            (false, _, false, true) => {
                let id = match map.remove("id") {
                    None | Some(Value::Null) => None,
                    Some(v) => Some(RequestId::deserialize(v).ok()?),
                };
                let error = ErrorData::deserialize(map.remove("error")?).ok()?;
                Some(Self::Error { id, error })
            }
            _ => None,
        }
    }

    pub fn request(request: ClientRequest, method: &str, id: RequestId) -> Self {
        Self::Request(JsonRpcRequest {
            id,
            method: method.to_string(),
            request,
            meta: RequestMetaObject::default(),
        })
    }
}

impl<'de> Deserialize<'de> for ClientJsonRpcMessage {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).ok_or_else(|| serde::de::Error::custom(UNRECOGNIZED))
    }
}

/// Split `params` into `_meta` and the rest. `Err` when `params` or its
/// `_meta` is not an object (no message shape accepts that).
fn split_params(params: Option<Value>) -> Result<(Option<JsonObject>, Option<JsonObject>), ()> {
    match params {
        None | Some(Value::Null) => Ok((None, None)),
        Some(Value::Object(mut map)) => {
            let meta = match map.remove("_meta") {
                None | Some(Value::Null) => None,
                Some(Value::Object(meta)) => Some(meta),
                Some(_) => return Err(()),
            };
            Ok((meta, Some(map)))
        }
        Some(_) => Err(()),
    }
}

fn typed<P: DeserializeOwned>(rest: &Option<JsonObject>) -> Option<P> {
    P::deserialize(Value::Object(rest.clone()?)).ok()
}

fn optional<P: DeserializeOwned>(rest: &Option<JsonObject>) -> Option<Option<P>> {
    match rest {
        None => Some(None),
        Some(map) => P::deserialize(Value::Object(map.clone())).ok().map(Some),
    }
}

fn parse_request(
    method: &str,
    params: Option<Value>,
) -> Option<(ClientRequest, RequestMetaObject)> {
    let (meta, rest) = split_params(params).ok()?;
    let meta = RequestMetaObject::from_object(meta.unwrap_or_default());
    let typed_request = match method {
        "ping" => Some(ClientRequest::Ping),
        "initialize" => typed(&rest).map(ClientRequest::Initialize),
        // `server/discover` requires a params object (its fields beyond
        // `_meta` are ignored).
        "server/discover" => rest.as_ref().map(|_| ClientRequest::Discover),
        "completion/complete" => typed(&rest).map(ClientRequest::Complete),
        "logging/setLevel" => typed(&rest).map(ClientRequest::SetLevel),
        "prompts/get" => typed(&rest).map(ClientRequest::GetPrompt),
        "prompts/list" => optional(&rest).map(ClientRequest::ListPrompts),
        "resources/list" => optional(&rest).map(ClientRequest::ListResources),
        "resources/templates/list" => optional(&rest).map(ClientRequest::ListResourceTemplates),
        "resources/read" => typed(&rest).map(ClientRequest::ReadResource),
        "subscriptions/listen" => typed(&rest).map(ClientRequest::SubscriptionsListen),
        "resources/subscribe" => typed(&rest).map(ClientRequest::Subscribe),
        "resources/unsubscribe" => typed(&rest).map(ClientRequest::Unsubscribe),
        "tools/call" => typed(&rest).map(ClientRequest::CallTool),
        "tools/list" => optional(&rest).map(ClientRequest::ListTools),
        "tasks/get" => typed(&rest).map(ClientRequest::GetTask),
        "tasks/update" => typed(&rest).map(ClientRequest::UpdateTask),
        "tasks/cancel" => typed(&rest).map(ClientRequest::CancelTask),
        _ => None,
    };
    let request = typed_request.unwrap_or_else(|| ClientRequest::Custom {
        method: method.to_string(),
        params: rest.map(Value::Object),
    });
    Some((request, meta))
}

fn parse_notification(
    method: &str,
    params: Option<Value>,
) -> Option<(ClientNotificationKind, NotificationMetaObject)> {
    let (meta, rest) = split_params(params).ok()?;
    let meta = NotificationMetaObject(MetaObject(meta.unwrap_or_default()));
    let body = || Value::Object(rest.clone().unwrap_or_default());
    let typed_kind = match method {
        "notifications/cancelled" => CancelledNotificationParam::deserialize(body())
            .ok()
            .map(ClientNotificationKind::Cancelled),
        "notifications/progress" => ProgressNotificationParam::deserialize(body())
            .ok()
            .map(ClientNotificationKind::Progress),
        "notifications/initialized" => Some(ClientNotificationKind::Initialized),
        "notifications/roots/list_changed" => Some(ClientNotificationKind::RootsListChanged),
        _ => None,
    };
    let kind = typed_kind.unwrap_or_else(|| ClientNotificationKind::Custom {
        method: method.to_string(),
        params: rest.map(Value::Object),
    });
    Some((kind, meta))
}

// ── Server → client ──────────────────────────────────────────────────────

/// A server notification: a method and its optional params object.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerNotification {
    pub method: Cow<'static, str>,
    pub params: Option<JsonObject>,
}

impl ServerNotification {
    pub fn new(method: &'static str, params: impl Serialize) -> Self {
        Self {
            method: Cow::Borrowed(method),
            params: Some(object(serde_json::to_value(params).unwrap_or(Value::Null))),
        }
    }
    pub fn no_params(method: &'static str) -> Self {
        Self {
            method: Cow::Borrowed(method),
            params: None,
        }
    }
    /// Attach `_meta[key] = value`, creating `params` when absent.
    pub fn set_meta(&mut self, key: &str, value: Value) {
        let params = self.params.get_or_insert_with(JsonObject::new);
        let meta = params
            .entry("_meta".to_string())
            .or_insert_with(|| Value::Object(JsonObject::new()));
        if let Value::Object(meta) = meta {
            meta.insert(key.to_string(), value);
        }
    }
    pub fn progress_token(&self) -> Option<ProgressToken> {
        if self.method != "notifications/progress" {
            return None;
        }
        ProgressToken::deserialize(self.params.as_ref()?.get("progressToken")?.clone()).ok()
    }
    pub fn cancelled_request_id(&self) -> Option<RequestId> {
        if self.method != "notifications/cancelled" {
            return None;
        }
        RequestId::deserialize(self.params.as_ref()?.get("requestId")?.clone()).ok()
    }
}

/// The typed result of one request, before it is shaped for the peer's era.
#[derive(Debug, Clone)]
pub enum ServerResult {
    Discover(DiscoverResult),
    Initialize(InitializeResult),
    Complete(CompleteResult),
    GetPrompt(GetPromptResult),
    ListPrompts(ListPromptsResult),
    ListResources(ListResourcesResult),
    ListResourceTemplates(ListResourceTemplatesResult),
    ReadResource(ReadResourceResult),
    SubscriptionsListen(Value),
    ListTools(ListToolsResult),
    CreateTask(CreateTaskResult),
    GetTask(GetTaskResult),
    CallTool(CallToolResult),
    InputRequired(InputRequiredResult),
    TaskAck,
    Empty,
    Custom(Value),
}

impl ServerResult {
    /// Older revisions do not define `resultType: "complete"`.
    pub fn strip_result_type_for_legacy_peer(&mut self) {
        let result_type = match self {
            Self::Complete(r) => &mut r.result_type,
            Self::GetPrompt(r) => &mut r.result_type,
            Self::ListPrompts(r) => &mut r.result_type,
            Self::ListResources(r) => &mut r.result_type,
            Self::ListResourceTemplates(r) => &mut r.result_type,
            Self::ReadResource(r) => &mut r.result_type,
            Self::ListTools(r) => &mut r.result_type,
            Self::CallTool(r) => &mut r.result_type,
            Self::Discover(_)
            | Self::Initialize(_)
            | Self::SubscriptionsListen(_)
            | Self::CreateTask(_)
            | Self::GetTask(_)
            | Self::InputRequired(_)
            | Self::TaskAck
            | Self::Empty
            | Self::Custom(_) => return,
        };
        result_type.take_if(|t| t.is_complete());
    }

    /// 2026-07-28 requires caching hints on cacheable complete results.
    pub fn fill_missing_cache_hints(&mut self) {
        let (result_type, ttl_ms, cache_scope) = match self {
            Self::ListTools(r) => (&r.result_type, &mut r.ttl_ms, &mut r.cache_scope),
            Self::ListPrompts(r) => (&r.result_type, &mut r.ttl_ms, &mut r.cache_scope),
            Self::ListResources(r) => (&r.result_type, &mut r.ttl_ms, &mut r.cache_scope),
            Self::ListResourceTemplates(r) => (&r.result_type, &mut r.ttl_ms, &mut r.cache_scope),
            Self::ReadResource(r) => (&r.result_type, &mut r.ttl_ms, &mut r.cache_scope),
            Self::Discover(_)
            | Self::Initialize(_)
            | Self::Complete(_)
            | Self::GetPrompt(_)
            | Self::SubscriptionsListen(_)
            | Self::CreateTask(_)
            | Self::GetTask(_)
            | Self::CallTool(_)
            | Self::InputRequired(_)
            | Self::TaskAck
            | Self::Empty
            | Self::Custom(_) => return,
        };
        if result_type.as_ref().is_some_and(|t| !t.is_complete()) {
            return;
        }
        ttl_ms.get_or_insert(0);
        cache_scope.get_or_insert(CacheScope::Private);
    }

    pub fn is_input_required(&self) -> bool {
        matches!(self, Self::InputRequired(_))
    }
}

impl Serialize for ServerResult {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Discover(r) => r.serialize(serializer),
            Self::Initialize(r) => r.serialize(serializer),
            Self::Complete(r) => r.serialize(serializer),
            Self::GetPrompt(r) => r.serialize(serializer),
            Self::ListPrompts(r) => r.serialize(serializer),
            Self::ListResources(r) => r.serialize(serializer),
            Self::ListResourceTemplates(r) => r.serialize(serializer),
            Self::ReadResource(r) => r.serialize(serializer),
            Self::SubscriptionsListen(r) | Self::Custom(r) => r.serialize(serializer),
            Self::ListTools(r) => r.serialize(serializer),
            Self::CreateTask(r) => r.serialize(serializer),
            Self::GetTask(r) => r.serialize(serializer),
            Self::CallTool(r) => r.serialize(serializer),
            Self::InputRequired(r) => r.serialize(serializer),
            Self::TaskAck => serde_json::json!({"resultType": "complete"}).serialize(serializer),
            Self::Empty => EmptyObject {}.serialize(serializer),
        }
    }
}

impl From<CallToolResponse> for ServerResult {
    fn from(response: CallToolResponse) -> Self {
        match response {
            CallToolResponse::Complete(r) => Self::CallTool(r),
            CallToolResponse::InputRequired(r) => Self::InputRequired(r),
            CallToolResponse::Task(r) => Self::CreateTask(r),
        }
    }
}

impl From<GetPromptResponse> for ServerResult {
    fn from(response: GetPromptResponse) -> Self {
        match response {
            GetPromptResponse::Complete(r) => Self::GetPrompt(r),
            GetPromptResponse::InputRequired(r) => Self::InputRequired(r),
        }
    }
}

impl From<ReadResourceResponse> for ServerResult {
    fn from(response: ReadResourceResponse) -> Self {
        match response {
            ReadResourceResponse::Complete(r) => Self::ReadResource(r),
            ReadResourceResponse::InputRequired(r) => Self::InputRequired(r),
        }
    }
}

/// Any message the server sends (the result is the large variant; see
/// [`ClientJsonRpcMessage`]).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ServerJsonRpcMessage {
    Response {
        id: RequestId,
        result: ServerResult,
    },
    Error {
        id: Option<RequestId>,
        error: ErrorData,
    },
    Notification(ServerNotification),
}

impl ServerJsonRpcMessage {
    pub fn response(result: ServerResult, id: RequestId) -> Self {
        Self::Response { id, result }
    }
    pub fn error(error: ErrorData, id: Option<RequestId>) -> Self {
        Self::Error { id, error }
    }
    /// The id a response or error answers.
    pub fn answered_id(&self) -> Option<&RequestId> {
        match self {
            Self::Response { id, .. } => Some(id),
            Self::Error { id, .. } => id.as_ref(),
            Self::Notification(_) => None,
        }
    }
}

impl Serialize for ServerJsonRpcMessage {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("jsonrpc", "2.0")?;
        match self {
            Self::Response { id, result } => {
                map.serialize_entry("id", id)?;
                map.serialize_entry("result", result)?;
            }
            Self::Error { id, error } => {
                if let Some(id) = id {
                    map.serialize_entry("id", id)?;
                }
                map.serialize_entry("error", error)?;
            }
            Self::Notification(n) => {
                map.serialize_entry("method", &n.method)?;
                if let Some(params) = &n.params {
                    map.serialize_entry("params", params)?;
                }
            }
        }
        map.end()
    }
}
