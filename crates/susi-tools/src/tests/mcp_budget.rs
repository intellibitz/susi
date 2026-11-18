use super::*;
use std::sync::Arc;
use susi_core::plane_bus::PlaneHandler;

// The transport itself (stdio and HTTP sessions, cooldown, header checks,
// cwd template, hidden console) is served and tested by the plug host in
// susi-commercial-apps (susi-ecosystem-hub migrated::susi_vendor_mcp); these
// pin what core sends it and how core reads its answers.

fn stdio_config() -> McpServerConfig {
    McpServerConfig {
        command: "srv".to_string(),
        args: vec![],
        env: None,
        extra: Default::default(),
    }
}

/// A stand-in plug host: echoes a call named `ok`, fails `fails`, answers
/// nothing usable otherwise, and lists two named tools and one unnamed.
struct Plug;

impl PlaneHandler for Plug {
    fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
        assert_eq!(payload["budget_policy"], "core_transport_v1");
        if topic == TOPIC_MCP_LIST_TOOLS {
            return Ok(json!({"tools": [
                {"name": "a", "description": "A"},
                {"name": "b"},
                {"description": "unnamed"}
            ]}));
        }
        Ok(match payload["name"].as_str() {
            Some("ok") => json!({"result": payload.to_string()}),
            Some("fails") => json!({"error": "tool failed"}),
            _ => json!({}),
        })
    }
}

#[test]
fn mcp_calls_reach_the_plug_host_with_their_budgets_and_answers_are_read() {
    let bus = PlaneBus::global();
    bus.register(TOPIC_MCP_CALL, Arc::new(Plug));
    bus.register(TOPIC_MCP_LIST_TOOLS, Arc::new(Plug));
    let (lease, handshake) = (Duration::from_secs(7), Duration::from_secs(2));
    let call = |name: &str, arguments: Value| {
        call_blocking_result(
            stdio_config(),
            name.to_string(),
            arguments,
            lease,
            handshake,
        )
    };
    let sent: Value =
        serde_json::from_str(&call("ok", json!({"q": 1})).expect("ok")).expect("json");
    assert_eq!(sent["config"]["command"], json!("srv"));
    assert_eq!(sent["arguments"], json!({"q": 1}));
    assert_eq!(
        (
            sent["lease_ms"].clone(),
            sent["handshake_ms"].clone(),
            sent["bus_timeout_ms"].clone()
        ),
        (json!(7000), json!(2000), json!(14000))
    );
    assert_eq!(call("fails", json!({})).expect_err("error"), "tool failed");
    assert!(call("silent", json!({}))
        .expect_err("no result")
        .contains("without a `result`"));
    assert_eq!(
        call("ok", json!([1])).expect_err("array"),
        "Tool arguments must be an object"
    );
    assert_eq!(
        list_tools_blocking(stdio_config(), lease, handshake).expect("tools"),
        vec![
            ("a".to_string(), "A".to_string()),
            ("b".to_string(), "MCP tool 'b'".to_string())
        ]
    );
    bus.unregister(TOPIC_MCP_CALL);
    bus.unregister(TOPIC_MCP_LIST_TOOLS);
}

#[test]
fn with_no_plug_host_the_call_names_the_command_that_serves_it() {
    let err = via_plug(
        "ecosystem_hub.mcp.test_unserved",
        json!({}),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect_err("unserved");
    assert!(err.contains("susi-plug-host --supervise"), "{err}");
}
