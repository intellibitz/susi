//! ACP (Agent Client Protocol) implementation for susi-gemi
//!
//! This module provides a complete ACP-compliant agent implementation that
//! integrates with susi-gemi's inference engine, enabling standardized
//! communication with ACP clients (IDEs, CLIs, etc.).

pub mod agent;
pub mod capabilities;
pub mod server;
pub mod session;
pub mod streaming;

pub use agent::GemiAgent;
pub use capabilities::GemiCapabilities;
pub use server::{run_acp_server_http, run_acp_server_stdio, GemiAcpServer};
pub use session::SessionManager;
pub use streaming::StreamingHandler;
