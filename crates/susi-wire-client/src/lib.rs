//! Std-only client for the local daemon's loopback socket: HTTP/1.1 with a
//! streaming body reader, Server-Sent Events, and MCP Streamable HTTP
//! sessions. It depends on std and JSON only, so the thin `susi` client can
//! talk to the daemon without an async runtime or an HTTP stack.
//!
//! It is for the loopback daemon socket only: it presents the host bearer
//! and never signs requests or seals bodies. Peer calls that need either go
//! through `susi_core::mcp_client`.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod http;
pub mod mcp;
pub mod sse;
