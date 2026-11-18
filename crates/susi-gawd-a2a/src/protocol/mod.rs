//! Agent2Agent 1.0 wire contracts, JSON codecs, and protocol errors.
//! The commercial runtime owns routing, session execution and task storage.

pub mod codec;
pub mod error;
pub mod types;

pub use error::{A2AError, Result};
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

/// Public A2A discovery path.
pub const WELL_KNOWN_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
