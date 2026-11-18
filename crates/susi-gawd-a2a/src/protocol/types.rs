//! A2A 1.0 wire types the server reads and writes: the JSON member names,
//! casing and enum spellings of the A2A JSON-RPC binding (ra2a 0.10.1 is the
//! reference this module reproduces; `tests/golden/a2a_wire.json` pins the
//! bytes). Parts and responses carry their kind as the member name
//! (`{"text": ..}`, `{"task": ..}`), never a `kind` field.
//!
//! Request structs keep every field the protocol defines, read or not, so
//! the server rejects exactly the payloads ra2a rejected with the same
//! serde messages (`Invalid params: ...`).

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::fmt;

use super::codec;

/// Extension metadata (`google.protobuf.Struct`).
pub type Metadata = HashMap<String, serde_json::Value>;

/// The sender of a message: `ROLE_USER`, `ROLE_AGENT`, `ROLE_UNSPECIFIED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    #[default]
    Unspecified,
    User,
    Agent,
}

const ROLES: &[&str] = &["ROLE_USER", "ROLE_AGENT", "ROLE_UNSPECIFIED"];

impl Serialize for Role {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Unspecified => "ROLE_UNSPECIFIED",
            Self::User => "ROLE_USER",
            Self::Agent => "ROLE_AGENT",
        })
    }
}

impl<'de> Deserialize<'de> for Role {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "ROLE_USER" => Ok(Self::User),
            "ROLE_AGENT" => Ok(Self::Agent),
            "ROLE_UNSPECIFIED" | "" => Ok(Self::Unspecified),
            other => Err(de::Error::unknown_variant(other, ROLES)),
        }
    }
}

/// A part's payload, discriminated on the wire by its member name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartContent {
    Text(String),
    /// Bytes, standard base64 with canonical padding on the wire.
    Raw(Vec<u8>),
    Url(String),
    Data(serde_json::Value),
}

/// One content part of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub content: PartContent,
    pub metadata: Option<Metadata>,
    pub filename: Option<String>,
    pub media_type: Option<String>,
}

impl Part {
    /// A text part.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: PartContent::Text(text.into()),
            metadata: None,
            filename: None,
            media_type: None,
        }
    }
}

impl Serialize for Part {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match &self.content {
            PartContent::Text(s) => map.serialize_entry("text", s)?,
            PartContent::Raw(bytes) => map.serialize_entry("raw", &codec::base64_encode(bytes))?,
            PartContent::Url(u) => map.serialize_entry("url", u)?,
            PartContent::Data(v) => map.serialize_entry("data", v)?,
        }
        if let Some(filename) = &self.filename {
            map.serialize_entry("filename", filename)?;
        }
        if let Some(media_type) = &self.media_type {
            map.serialize_entry("mediaType", media_type)?;
        }
        if let Some(metadata) = &self.metadata {
            map.serialize_entry("metadata", metadata)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Part {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(PartVisitor)
    }
}

struct PartVisitor;

impl<'de> Visitor<'de> for PartVisitor {
    type Value = Part;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a Part object with one of: text, raw, url, data")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Part, M::Error> {
        let mut content = None;
        let mut metadata = None;
        let mut filename = None;
        let mut media_type = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "text" => content = Some(PartContent::Text(map.next_value()?)),
                "raw" => {
                    let encoded: String = map.next_value()?;
                    let bytes = codec::base64_decode(&encoded).map_err(de::Error::custom)?;
                    content = Some(PartContent::Raw(bytes));
                }
                "url" => content = Some(PartContent::Url(map.next_value()?)),
                "data" => content = Some(PartContent::Data(map.next_value()?)),
                "filename" => filename = Some(map.next_value()?),
                "mediaType" => media_type = Some(map.next_value()?),
                "metadata" => metadata = Some(map.next_value::<Metadata>()?),
                _ => {
                    map.next_value::<serde_json::Value>()?;
                }
            }
        }
        let content = content
            .ok_or_else(|| de::Error::custom("Part must contain one of: text, raw, url, data"))?;
        Ok(Part {
            content,
            metadata,
            filename,
            media_type,
        })
    }
}

/// One message between a client and the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    #[serde(rename = "messageId")]
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub role: Role,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_task_ids: Vec<String>,
}

impl Message {
    /// A message with a fresh (version 7) message id.
    pub fn new(role: Role, parts: Vec<Part>) -> Self {
        Self {
            id: codec::uuid_v7(),
            context_id: None,
            task_id: None,
            role,
            parts,
            metadata: None,
            extensions: Vec::new(),
            reference_task_ids: Vec::new(),
        }
    }

    pub fn user(parts: Vec<Part>) -> Self {
        Self::new(Role::User, parts)
    }

    pub fn agent(parts: Vec<Part>) -> Self {
        Self::new(Role::Agent, parts)
    }
}

/// A task's lifecycle state, `TASK_STATE_*` on the wire. `Debug` names
/// the variant in the terminal-state error text, as the wire always did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskState {
    #[default]
    Unspecified,
    Submitted,
    Working,
    Completed,
    Failed,
    Canceled,
    InputRequired,
    Rejected,
    AuthRequired,
}

const TASK_STATES: &[&str] = &[
    "TASK_STATE_UNSPECIFIED",
    "TASK_STATE_SUBMITTED",
    "TASK_STATE_WORKING",
    "TASK_STATE_COMPLETED",
    "TASK_STATE_FAILED",
    "TASK_STATE_CANCELED",
    "TASK_STATE_INPUT_REQUIRED",
    "TASK_STATE_REJECTED",
    "TASK_STATE_AUTH_REQUIRED",
];

impl TaskState {
    const ALL: [Self; 9] = [
        Self::Unspecified,
        Self::Submitted,
        Self::Working,
        Self::Completed,
        Self::Failed,
        Self::Canceled,
        Self::InputRequired,
        Self::Rejected,
        Self::AuthRequired,
    ];

    /// No further change is permitted.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Canceled | Self::Rejected
        )
    }

    /// Paused until the client acts.
    pub fn is_interrupted(self) -> bool {
        matches!(self, Self::InputRequired | Self::AuthRequired)
    }

    fn wire_name(self) -> &'static str {
        let index = Self::ALL.iter().position(|s| *s == self).unwrap_or(0);
        TASK_STATES[index]
    }
}

impl Serialize for TaskState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.wire_name())
    }
}

impl<'de> Deserialize<'de> for TaskState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        if s.is_empty() {
            return Ok(Self::Unspecified);
        }
        TASK_STATES
            .iter()
            .position(|name| *name == s)
            .map(|i| Self::ALL[i])
            .ok_or_else(|| de::Error::unknown_variant(&s, TASK_STATES))
    }
}

/// A task's status at one point in time.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TaskStatus {
    pub state: TaskState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<Message>,
    /// RFC 3339, UTC, `+00:00`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

impl TaskStatus {
    /// `state`, stamped now.
    pub fn new(state: TaskState) -> Self {
        Self {
            state,
            message: None,
            timestamp: Some(codec::rfc3339_now()),
        }
    }

    /// `state` with a status message, stamped now.
    pub fn with_message(state: TaskState, message: Message) -> Self {
        Self {
            message: Some(message),
            ..Self::new(state)
        }
    }

    /// `failed` with the error as the agent's message.
    pub fn failed(error: impl Into<String>) -> Self {
        Self::with_message(TaskState::Failed, Message::agent(vec![Part::text(error)]))
    }
}

/// A unit of work and its status. This server never records artifacts or
/// history, so the task carries neither (both are omitted when empty).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    pub id: String,
    pub context_id: String,
    pub status: TaskStatus,
}

impl Task {
    /// A `submitted` task.
    pub fn new(id: impl Into<String>, context_id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            context_id: context_id.into(),
            status: TaskStatus::new(TaskState::Submitted),
        }
    }
}

/// Where and how to deliver a task's push notifications.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthenticationInfo {
    pub scheme: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<String>,
}

/// A push-notification config bound to a task (request and reply shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TaskPushNotificationConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authentication: Option<AuthenticationInfo>,
}

/// `message/send` and `message/stream` options.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageConfiguration {
    #[serde(default)]
    pub accepted_output_modes: Vec<String>,
    #[serde(default)]
    pub task_push_notification_config: Option<TaskPushNotificationConfig>,
    #[serde(default)]
    pub history_length: Option<i32>,
    /// Reply on the first task event instead of waiting for a terminal one.
    #[serde(default)]
    pub return_immediately: bool,
}

/// `message/send` / `message/stream` params.
#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    pub message: Message,
    #[serde(default)]
    pub configuration: Option<SendMessageConfiguration>,
    #[serde(default)]
    pub metadata: Option<Metadata>,
}

/// `tasks/get` params.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    pub id: String,
    #[serde(default)]
    pub history_length: Option<i32>,
}

/// `tasks/list` params.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub context_id: Option<String>,
    #[serde(default)]
    pub status: Option<TaskState>,
    #[serde(default)]
    pub page_size: Option<i32>,
    #[serde(default)]
    pub page_token: Option<String>,
    #[serde(default)]
    pub history_length: Option<i32>,
    #[serde(default)]
    pub status_timestamp_after: Option<String>,
    #[serde(default)]
    pub include_artifacts: Option<bool>,
}

/// `tasks/list` reply.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ListTasksResponse {
    pub tasks: Vec<Task>,
    pub next_page_token: String,
    pub page_size: i32,
    pub total_size: i32,
}

/// `tasks/cancel` params.
#[derive(Debug, Clone, Deserialize)]
pub struct CancelTaskRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    pub id: String,
    #[serde(default)]
    pub metadata: Option<Metadata>,
}

/// `tasks/resubscribe` params.
#[derive(Debug, Clone, Deserialize)]
pub struct SubscribeToTaskRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    pub id: String,
}

/// `tasks/pushNotificationConfig/get` params.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTaskPushNotificationConfigRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    pub task_id: String,
    pub id: String,
}

/// `tasks/pushNotificationConfig/delete` params (its own type: serde names
/// the struct in a params error).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteTaskPushNotificationConfigRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    pub task_id: String,
    pub id: String,
}

/// `tasks/pushNotificationConfig/list` params.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTaskPushNotificationConfigsRequest {
    #[serde(default)]
    pub tenant: Option<String>,
    pub task_id: String,
    #[serde(default)]
    pub page_size: Option<i32>,
    #[serde(default)]
    pub page_token: Option<String>,
}

/// `tasks/pushNotificationConfig/list` reply.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListTaskPushNotificationConfigsResponse {
    pub configs: Vec<TaskPushNotificationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page_token: Option<String>,
}

/// `agent/getAuthenticatedExtendedCard` params.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GetExtendedAgentCardRequest {
    #[serde(default)]
    pub tenant: Option<String>,
}

/// One endpoint of the agent and the binding it speaks.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentInterface {
    pub url: String,
    /// `JSONRPC`, `GRPC` or `HTTP+JSON`.
    pub protocol_binding: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    pub protocol_version: String,
}

/// Optional protocol features the agent supports.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[cfg_attr(test, derive(Deserialize))]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub streaming: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_notifications: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extended_agent_card: Option<bool>,
}

/// One advertised skill.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub input_modes: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output_modes: Vec<String>,
}

/// HTTP authentication (`{"http": {"scheme": "Bearer"}}` on the card).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HttpAuthSecurityScheme {
    pub scheme: String,
}

/// A security scheme, keyed on the wire by its kind. The server advertises
/// HTTP bearer authentication only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityScheme {
    Http(HttpAuthSecurityScheme),
}

impl Serialize for SecurityScheme {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(1))?;
        match self {
            Self::Http(v) => map.serialize_entry("http", v)?,
        }
        map.end()
    }
}

/// Schemes (by name) a request must satisfy, with their scopes.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SecurityRequirement {
    pub schemes: HashMap<String, Vec<String>>,
}

/// The agent card served at `/.well-known/agent-card.json`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentCard {
    pub name: String,
    pub description: String,
    pub supported_interfaces: Vec<AgentInterface>,
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
    pub capabilities: AgentCapabilities,
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub security_schemes: HashMap<String, SecurityScheme>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub security_requirements: Vec<SecurityRequirement>,
    pub default_input_modes: Vec<String>,
    pub default_output_modes: Vec<String>,
    pub skills: Vec<AgentSkill>,
}
