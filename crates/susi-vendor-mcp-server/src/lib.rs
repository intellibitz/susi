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
//! (GMCP) depend on this crate and never declare `rmcp` themselves. Outbound
//! MCP client adapters live in `susi-commercial-apps` (`susi-ecosystem-hub`).

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

    #[test]
    fn rmcp_parse_invalid_request_and_method_not_found_constructors_preserve_codes() {
        let parse_err = crate::ErrorData::parse_error("malformed json", None);
        assert_eq!(parse_err.code, crate::model::ErrorCode::PARSE_ERROR);
        assert_eq!(parse_err.message.as_ref(), "malformed json");

        let inv_req = crate::ErrorData::invalid_request("missing jsonrpc", None);
        assert_eq!(inv_req.code, crate::model::ErrorCode::INVALID_REQUEST);

        let not_found = crate::ErrorData::method_not_found::<crate::model::CallToolRequestMethod>();
        assert_eq!(not_found.code, crate::model::ErrorCode::METHOD_NOT_FOUND);
    }

    #[test]
    fn reexported_schemars_generates_object_schema_for_derived_input() {
        #[derive(crate::schemars::JsonSchema)]
        #[schemars(crate = "crate::schemars")]
        #[allow(dead_code)]
        struct ProbeToolInput {
            name: String,
            limit: u32,
        }

        let schema = crate::schemars::schema_for!(ProbeToolInput);
        let value = schema.as_value();
        assert_eq!(value["type"], "object");
        assert!(value["properties"]["name"].is_object());
        assert!(value["properties"]["limit"].is_object());
    }

    #[test]
    fn rmcp_error_code_constants_are_distinct_and_cloneable() {
        let codes = [
            crate::model::ErrorCode::PARSE_ERROR,
            crate::model::ErrorCode::INVALID_REQUEST,
            crate::model::ErrorCode::METHOD_NOT_FOUND,
            crate::model::ErrorCode::INVALID_PARAMS,
            crate::model::ErrorCode::INTERNAL_ERROR,
        ];
        for (i, a) in codes.iter().enumerate() {
            assert_eq!(&a.clone(), a);
            for (j, b) in codes.iter().enumerate() {
                assert_eq!(i == j, a == b);
            }
        }
    }
}
