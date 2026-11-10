//! A2A errors and their JSON-RPC codes. The display text is the `message`
//! clients read, so each variant keeps the wording the wire always carried.

use std::fmt;

/// A request the A2A handler refused, or an executor failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A2AError {
    /// -32602.
    InvalidParams(String),
    /// -32001.
    TaskNotFound(String),
    /// -32002.
    TaskNotCancelable(String),
    /// -32003.
    PushNotificationNotSupported,
    /// -32007.
    ExtendedCardNotConfigured,
    /// -32603: everything else (event delivery, a queue that closed).
    Other(String),
}

impl fmt::Display for A2AError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidParams(m) => write!(f, "invalid params: {m}"),
            Self::TaskNotFound(id) => write!(f, "task not found: {id}"),
            Self::TaskNotCancelable(id) => write!(f, "task cannot be canceled: {id}"),
            Self::PushNotificationNotSupported => f.write_str("push notification not supported"),
            Self::ExtendedCardNotConfigured => f.write_str("extended card not configured"),
            Self::Other(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for A2AError {}

impl A2AError {
    /// The JSON-RPC error code (A2A 1.0 section 9.5 for the -320xx range).
    pub fn code(&self) -> i32 {
        match self {
            Self::InvalidParams(_) => INVALID_PARAMS,
            Self::TaskNotFound(_) => -32001,
            Self::TaskNotCancelable(_) => -32002,
            Self::PushNotificationNotSupported => -32003,
            Self::ExtendedCardNotConfigured => -32007,
            Self::Other(_) => INTERNAL_ERROR,
        }
    }
}

/// Result of an A2A operation.
pub type Result<T> = std::result::Result<T, A2AError>;

pub const PARSE_ERROR: i32 = -32700;
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;
