use super::*;

struct MockTransport {
    repeat_cursor: bool,
}

impl McpProbeTransport for MockTransport {
    fn request(&mut self, request: &McpProbeRequest) -> Result<McpProbeResponse, McpProbeError> {
        match request.method.as_str() {
            "initialize" => Ok(McpProbeResponse::from_value(json!({
                "protocolVersion": CURRENT_PROTOCOL_VERSION,
                "serverInfo": { "name": "mock-mcp", "version": "1.0.0" },
                "capabilities": { "tools": { "listChanged": true } }
            }))),
            "tools/list" => {
                if self.repeat_cursor {
                    Ok(McpProbeResponse::from_value(json!({
                        "tools": [{ "name": "t1" }],
                        "nextCursor": "same-cursor"
                    })))
                } else {
                    Ok(McpProbeResponse::from_value(json!({
                        "tools": [{ "name": "alpha" }, { "name": "beta" }]
                    })))
                }
            }
            "ping" => Ok(McpProbeResponse::from_value(json!({}))),
            other => Err(McpProbeError::Protocol {
                method: other.into(),
                detail: "unexpected".into(),
            }),
        }
    }

    fn notify(&mut self, _notification: &McpProbeNotification) -> Result<(), McpProbeError> {
        Ok(())
    }

    fn probe_malformed_response(
        &mut self,
        _timeout: Duration,
        _protocol_version: &str,
    ) -> Result<(), McpProbeError> {
        Ok(())
    }

    fn probe_credential_isolation(&self) -> Result<(), McpProbeError> {
        Ok(())
    }
}

#[test]
fn request_headers_detect_sensitive_credentials_and_preserve_protocol_version() {
    let mut req = McpProbeRequest::new(1, "initialize", json!({}), Duration::from_secs(1))
        .with_protocol_version(CURRENT_PROTOCOL_VERSION);
    assert!(req.sensitive_headers().is_empty());
    assert_eq!(
        req.headers.get("mcp-protocol-version").map(String::as_str),
        Some(CURRENT_PROTOCOL_VERSION)
    );
    req.headers
        .insert("Authorization".into(), "Bearer secret".into());
    assert_eq!(req.sensitive_headers(), vec!["Authorization".to_string()]);
}

#[test]
fn probe_rejects_zero_timeout_or_limits_during_configuration_check() {
    let opts = McpProbeOptions {
        timeout: Duration::ZERO,
        ..Default::default()
    };
    let report = McpProbe::new(opts).run(&mut MockTransport {
        repeat_cursor: false,
    });
    assert!(!report.passed());
    assert!(!report.checks["configuration"].passed);
}

#[test]
fn probe_run_succeeds_across_all_lifecycle_checks_with_valid_transport() {
    let probe = McpProbe::new(McpProbeOptions::default());
    let report = probe.run(&mut MockTransport {
        repeat_cursor: false,
    });
    assert!(report.passed());
    assert_eq!(
        report.protocol_version.as_deref(),
        Some(CURRENT_PROTOCOL_VERSION)
    );
    assert_eq!(report.supports_list_changed, Some(true));
    assert_eq!(report.tools, vec!["alpha".to_string(), "beta".to_string()]);
}

#[test]
fn probe_run_fails_closed_when_server_repeats_pagination_cursor() {
    let probe = McpProbe::new(McpProbeOptions::default());
    let report = probe.run(&mut MockTransport {
        repeat_cursor: true,
    });
    assert!(!report.passed());
    assert!(!report.checks["discovery"].passed);
    assert!(report.checks["discovery"]
        .detail
        .contains("repeated a pagination cursor"));
}

#[test]
fn validate_initialize_and_tools_page_reject_unsupported_versions_and_duplicate_tools() {
    let bad_ver = json!({
        "protocolVersion": "1999-01-01",
        "serverInfo": { "name": "srv", "version": "1.0" },
        "capabilities": {}
    });
    assert!(validate_initialize(&bad_ver).is_err());

    let dup_tools = json!({
        "tools": [
            { "name": "same_tool" },
            { "name": "same_tool" }
        ]
    });
    assert!(validate_tools_page(&dup_tools).is_err());
}
