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

#[rustfmt::skip]
#[path = "../../susi-abi/src/lib.rs"]
pub mod susi_abi;

// Vendored `susi-error` contract + IPC reporter: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[path = "../../susi-core/src/susi_error.rs"]
pub mod susi_error;

// Vendored `susi-paths` IPC client: full surface kept identical
// across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[path = "../../susi-core/src/susi_paths.rs"]
mod susi_paths;

// Vendored `susi-config` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
// rustfmt::skip: the file is vendored byte-identical while consumers span
// edition 2021/2024 whose style editions sort imports and indent format!
// args differently — formatting it per-crate would break the invariant.
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../susi-core/src/susi_config.rs"]
pub mod susi_config;

// Vendored `susi-sandbox` surface + IPC client: full surface kept
// identical across crates; per-crate dead_code allowance is the audit trail.
#[allow(dead_code)]
#[rustfmt::skip]
#[path = "../../susi-sandbox/vendor_template/susi_sandbox/mod.rs"]
pub mod susi_sandbox;

// Vendored `susi_core` microkernel subset (canonical tree:
// `susi-core/vendor_template/susi_core/`): bus/registry/capture/mac/intent
// state rendezvous with the daemon's real susi_core via `<cache>/bus/<pid>/`
// + substrate files. Allows keep the tree byte-identical across consumers:
// dead_code audits the unexercised surface; rustfmt::skip + collapsible_if
// stop edition-2024 style drift against the edition-2021 canonical source.
#[allow(dead_code, clippy::collapsible_if)]
#[rustfmt::skip]
#[path = "../../susi-core/src/embedded.rs"]
pub mod susi_core;

pub mod catalog;
#[cfg(feature = "tools-rich")]
pub mod embed_provider;
#[cfg(not(feature = "tools-rich"))]
pub mod embed_provider {
    /// No fastembed in this build — the embed surface refuses rather
    /// than fabricating vectors.
    pub fn register_local_embed_provider() {}
}
pub mod mcp_wrapper;
pub mod protocol;
pub mod reflexes;
pub mod server;
mod stdio;
pub mod tool_registry;
pub mod tool_types;
pub mod tools;

use std::path::Path;

pub use crate::susi_core::plane_bus::tools as plane_tools;

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
