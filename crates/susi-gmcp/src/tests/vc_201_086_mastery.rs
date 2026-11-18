//! Mastery checks for VC-201-086: probe admitted MCP servers for
//! initialization, discovery, cancellation, timeouts, malformed responses,
//! and credential isolation; compatibility status records tested behavior
//! rather than catalog membership.
//!
//! Every test name starts `vc_201_086_mastery_` so the vector's evidence is
//! enumerable with `cargo test -p susi-gmcp vc_201_086`.

use crate::{
    mcp_probe::{
        McpProbeError, McpProbeNotification, McpProbeOptions, McpProbeRequest, McpProbeResponse,
        McpProbeTransport,
    },
    mcp_profile::McpSpecProfile,
};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Default)]
struct FakeMcpServer {
    requests: Vec<McpProbeRequest>,
    notifications: Vec<McpProbeNotification>,
    malformed_initialize: bool,
}

impl McpProbeTransport for FakeMcpServer {
    fn request(&mut self, request: &McpProbeRequest) -> Result<McpProbeResponse, McpProbeError> {
        self.requests.push(request.clone());
        match request.method.as_str() {
            "initialize" if self.malformed_initialize => Ok(McpProbeResponse::from_value(
                Value::String("malformed".to_string()),
            )),
            "initialize" => Ok(McpProbeResponse::from_value(json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "fake-mcp", "version": "1.0"}
            }))),
            "tools/list" => Ok(McpProbeResponse::from_value(json!({
                "tools": [{"name": "echo"}, {"name": "status"}]
            }))),
            _ => Err(McpProbeError::Transport {
                method: request.method.clone(),
                detail: "unexpected request".to_string(),
            }),
        }
    }

    fn notify(&mut self, notification: &McpProbeNotification) -> Result<(), McpProbeError> {
        self.notifications.push(notification.clone());
        Ok(())
    }

    fn probe_timeout(
        &mut self,
        _timeout: Duration,
        _protocol_version: &str,
    ) -> Result<Duration, McpProbeError> {
        Ok(Duration::from_millis(1))
    }

    fn probe_malformed_response(
        &mut self,
        _timeout: Duration,
        _protocol_version: &str,
    ) -> Result<(), McpProbeError> {
        Ok(())
    }

    fn probe_credential_isolation(&self) -> Result<(), McpProbeError> {
        if self
            .requests
            .iter()
            .any(|request| !request.sensitive_headers().is_empty())
        {
            Err(McpProbeError::CredentialLeak {
                detail: "sensitive request header observed".to_string(),
            })
        } else {
            Ok(())
        }
    }
}

#[test]
fn vc_201_086_mastery_compatibility_records_probed_behavior() {
    let mut profile = McpSpecProfile::default_profile();
    assert_eq!(profile.registry_metadata["probe_status"], "unprobed");
    let mut server = FakeMcpServer::default();
    let report = profile.probe(&mut server, McpProbeOptions::default());

    assert!(report.passed(), "{report:?}");
    assert_eq!(profile.registry_metadata["probe_status"], "passed");
    assert_eq!(profile.registry_metadata["supports_list_changed"], true);
    assert_eq!(
        profile.registry_metadata["discovered_tools"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(profile.negotiated.as_deref(), Some("2025-11-25"));
}

#[test]
fn vc_201_086_mastery_catalog_versions_include_current_wire() {
    let mut profile = McpSpecProfile::default_profile();
    assert_eq!(
        profile.negotiate(&["2025-11-25"]),
        Some("2025-11-25".to_string())
    );
    assert_eq!(
        profile.negotiate(&["2025-06-18"]),
        Some("2025-06-18".to_string())
    );
}

#[test]
fn vc_201_086_mastery_probe_exercises_lifecycle_safety_checks() {
    let mut profile = McpSpecProfile::default_profile();
    let mut server = FakeMcpServer::default();
    let report = profile.probe(&mut server, McpProbeOptions::default());

    for check in [
        "initialize",
        "initialized",
        "discovery",
        "cancellation",
        "timeout",
        "malformed_response",
        "credential_isolation",
    ] {
        assert!(report.checks[check].passed, "{check}: {report:?}");
    }
    assert_eq!(server.notifications[1].method, "notifications/cancelled");
}

#[test]
fn vc_201_086_mastery_malformed_initialization_is_rejected() {
    let mut profile = McpSpecProfile::default_profile();
    let mut server = FakeMcpServer {
        malformed_initialize: true,
        ..FakeMcpServer::default()
    };
    let report = profile.probe(&mut server, McpProbeOptions::default());

    assert!(!report.passed());
    assert!(report.checks["initialize"].detail.contains("malformed"));
    assert_eq!(profile.registry_metadata["probe_status"], "failed");
    assert!(profile.negotiated.is_none());
}

#[test]
fn vc_201_086_mastery_newest_common_wins_holds() {
    let mut profile = McpSpecProfile::default_profile();
    assert_eq!(
        profile.negotiate(&["2024-11-05", "2025-03-26"]),
        Some("2025-03-26".to_string())
    );
}

#[test]
fn vc_201_086_mastery_probe_requests_never_include_host_credentials() {
    let mut server = FakeMcpServer::default();
    let mut profile = McpSpecProfile::default_profile();
    let report = profile.probe(&mut server, McpProbeOptions::default());
    assert!(report.checks["credential_isolation"].passed);
    assert!(server
        .requests
        .iter()
        .all(|request| !request.headers.contains_key("authorization")));
}
