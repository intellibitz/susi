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

/// GMCP's own MCP server (wire model, sessions, stdio and Streamable HTTP).
pub mod mcp;
/// The path the typed MCP API has always had: consumers name
/// `susi_gmcp::rmcp::model::*` for the typed form the canonical content
/// model adapts into (`content::CanonicalContentBlock::to_rmcp`). It is
/// GMCP's own implementation in [`mcp`]; no MCP SDK is linked.
pub use self::mcp as rmcp;

pub use susi_abi;

pub use susi_error;

pub use susi_config;

pub use susi_sandbox_client as susi_sandbox;

pub use susi_core;

pub mod catalog;
pub mod content;
pub mod direct_call;
#[cfg(feature = "tools-rich")]
pub mod embed_provider;
pub mod extensions;
#[cfg(not(feature = "tools-rich"))]
pub mod embed_provider {
    /// No fastembed in this build — the embed surface refuses rather
    /// than fabricating vectors.
    pub fn register_local_embed_provider() {}

    /// The `install embeddings` source, accepted so the CLI can refuse it.
    pub const INSTALL_SOURCE: &str = "embeddings";

    /// No fastembed in this build: the explicit install refuses.
    pub fn install_on_request() -> crate::susi_error::EaiResult<String> {
        Err(crate::susi_error::EaiError::process(
            "this build has no semantic-search engine (tools-rich disabled)",
        ))
    }
}
pub mod jsonrpc_gate;
pub mod mcp_call_budgets;
pub mod mcp_probe;
pub mod mcp_profile;
pub mod mcp_registry_trust;
pub mod mcp_wrapper;
pub mod protocol;
pub mod requirements_ledger;
pub mod schema_check;
pub mod server;
pub mod stdio;
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
mod eco_mcp_features;
#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
#[path = "tests/vc_201_086_mastery.rs"]
mod vc_201_086_mastery_tests;

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn gmcp_host_dispatch_handles_json_object_argument_without_panicking() {
        let out = GmcpHost::dispatch("unwired_test_tool", r#"{"key":"val"}"#, Path::new("."));
        assert!(!out.is_empty());
    }

    #[test]
    fn gmcp_host_dispatch_falls_back_to_json_string_when_argument_is_plain_text() {
        let out = GmcpHost::dispatch("unwired_test_tool", "plain-text-arg", Path::new("."));
        assert!(!out.is_empty());
    }

    #[test]
    fn gmcp_host_dispatch_handles_empty_argument_cleanly() {
        let out = GmcpHost::dispatch("", "", Path::new("."));
        assert!(!out.is_empty());
    }

    #[test]
    fn susi_error_reexport_constructs_process_error_from_gmcp_root() {
        let err = susi_error::EaiError::process("gmcp root test");
        assert!(err.to_string().contains("gmcp root test"));
    }

    #[test]
    fn gmcp_crate_reexports_embed_install_source_and_protocol_versions() {
        assert_eq!(embed_provider::INSTALL_SOURCE, "embeddings");
        assert!(!protocol::MODERN_REQUEST_VERSIONS.is_empty());
    }
}
