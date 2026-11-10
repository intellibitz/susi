//! The Agent2Agent (A2A) 1.0 protocol as this server speaks it, in-tree:
//! the JSON-RPC binding with SSE streaming, the task lifecycle and push
//! notifications. It replaced the ra2a SDK (T-CLAUDENOA2ASDK-1), whose
//! client side alone pulled reqwest and its TLS, encoding and date crates
//! into the core build; `tests/golden/a2a_wire.json` holds this module to
//! the bytes ra2a put on the wire.

mod codec;
mod error;
mod handler;
mod http;
mod types;

pub use error::{A2AError, Result};
pub use handler::{AgentExecutor, EventQueue, Handler, RequestContext};
pub use http::{router, WELL_KNOWN_AGENT_CARD_PATH};
pub use types::{
    AgentCapabilities, AgentCard, AgentInterface, AgentSkill, AuthenticationInfo,
    CancelTaskRequest, DeleteTaskPushNotificationConfigRequest, GetExtendedAgentCardRequest,
    GetTaskPushNotificationConfigRequest, GetTaskRequest, HttpAuthSecurityScheme,
    ListTaskPushNotificationConfigsRequest, ListTaskPushNotificationConfigsResponse,
    ListTasksRequest, ListTasksResponse, Message, Metadata, Part, PartContent, Role,
    SecurityRequirement, SecurityScheme, SendMessageConfiguration, SendMessageRequest,
    SubscribeToTaskRequest, Task, TaskPushNotificationConfig, TaskState, TaskStatus,
};

/// The A2A protocol revision the card advertises.
pub const PROTOCOL_VERSION: &str = "1.0";

/// The JSON-RPC binding's name on an agent interface.
pub const JSONRPC_BINDING: &str = "JSONRPC";
