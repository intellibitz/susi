//! GMCP's own Model Context Protocol server: JSON-RPC 2.0 on the standard
//! library, serde and the tokio/hyper transport GMCP already runs.
//!
//! It serves exactly the MCP surface GMCP exposes — revisions 2024-11-05
//! through 2026-07-28; the `initialize` handshake and the 2026-07-28
//! per-request lifecycle (`server/discover`); tools (paginated listing,
//! calls, progress, cancellation, tasks, multi-round-trip input); resources
//! and templates; prompts; completion; logging; subscriptions — over
//! newline-delimited stdio and Streamable HTTP. No MCP SDK is linked.
//!
//! - [`model`]: the wire types.
//! - [`message`]: JSON-RPC envelopes and typed client requests.
//! - [`service`]: sessions, dispatch to a [`ServerHandler`], the peer.
//! - [`transport`]: the transport contract and a raw byte-stream transport.
//! - [`http`]: Streamable HTTP (sessions, SSE, stateless requests).
//! - [`task_manager`]: pollable tasks.
//! - [`schema`]: `inputSchema`s from struct declarations.

pub mod http;
pub mod message;
pub mod model;
pub mod schema;
pub mod service;
pub mod task_manager;
pub mod transport;

pub use model::ErrorData;
pub use service::{RoleServer, ServerHandler, ServiceExt};
