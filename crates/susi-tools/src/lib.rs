#![deny(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        unsafe_code,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

// One compiled native-service client; Wasmer remains isolated in the service.
pub use susi_native_client as susi_native;

pub use susi_core;

pub mod plane_handler;

pub mod client;
pub mod config;
pub mod connection;
pub mod hooks;
pub mod leading_mcp;
pub mod registry;
pub mod types;

pub use client::GmcpClient;
pub use config::{GlobalMcpEntry, McpConfig, McpServerConfig};
pub use hooks::{EngineHooks, HardwareSnapshot};
pub use leading_mcp::{LeadingMcpDefinition, LeadingMcpManager, LeadingMcpOverride};
pub use registry::ToolRegistry;
pub use types::{McpTool, MetaCategory, MetaTool, MetaToolHandler, SusiTool};
