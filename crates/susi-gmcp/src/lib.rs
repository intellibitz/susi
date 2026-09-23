#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
mod susi_paths;

// Vendored `susi-config` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// rustfmt::skip: the file is vendored byte-identical while consumers span
// edition 2021/2024 whose style editions sort imports and indent format!
// args differently — formatting it per-crate would break the invariant.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_config;

// Vendored `susi-sandbox` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[rustfmt::skip]
pub mod susi_sandbox;

pub mod catalog;
pub mod mcp_wrapper;
pub mod protocol;
pub mod reflexes;
pub mod server;
mod stdio;
pub mod tool_registry;
pub mod tool_types;
pub mod tools;

use std::path::Path;

pub use susi_core::plane_bus::tools as plane_tools;

/// GMCP Host: The unified execution entry point for the Meta-Intelligence Substrate.
pub struct GmcpHost;

impl GmcpHost {
    pub fn dispatch(name: &str, arg: &str, workspace: &Path) -> String {
        let val = serde_json::from_str(arg).unwrap_or(serde_json::json!(arg));
        plane_tools::execute_tool(name, &val, workspace).unwrap_or_else(|e| format!("[Error] {e}"))
    }
}

#[cfg(test)]
mod protocol_tests;
