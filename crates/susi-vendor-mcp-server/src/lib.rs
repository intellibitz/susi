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

//! # susi-vendor-mcp-server
//!
//! The one crate that links the MCP *server* SDK (`rmcp`). Feature planes
//! (GMCP) depend on this crate and never declare `rmcp` themselves. The
//! client SDK and its reqwest transport stay in `susi-vendor-mcp`.

/// JSON Schema derivation for typed tool inputs (`#[derive(JsonSchema)]` with
/// `#[schemars(crate = "susi_vendor_mcp_server::schemars")]`), the same
/// generator rmcp's `#[tool]` uses for `inputSchema`.
pub use rmcp::schemars;
pub use rmcp::tool;
pub use rmcp::{
    handler, model, service, task_manager, transport, ErrorData, ServerHandler, ServiceExt,
};

#[cfg(test)]
mod tests {
    /// The re-exported rmcp surface must stay reachable — a feature/flag
    /// regression in this facade fails loudly here instead of at the GMCP
    /// call site.
    #[test]
    fn rmcp_server_surface_is_reachable() {
        let err = crate::model::ErrorData::internal_error("probe", None);
        assert_eq!(err.code, crate::model::ErrorCode::INTERNAL_ERROR);
    }

    #[test]
    fn rmcp_invalid_params_preserves_code_and_message() {
        let err = crate::model::ErrorData::invalid_params("bad param", None);
        assert_eq!(err.code, crate::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(err.message.as_ref(), "bad param");
    }
}
