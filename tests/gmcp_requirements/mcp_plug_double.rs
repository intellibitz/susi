/// Stand-in for the susi-commercial-apps plug host on the plane bus. Core's
/// outbound MCP client moved there (core T-CORESIZEVAULT-14): core authorizes
/// a call and sends it on `ecosystem_hub.mcp.call` / `list_tools`. These
/// conformance cases reach their in-test HTTP MCP servers through this
/// double, which speaks MCP with core's own `McpHost`; the plug host's real
/// transport is tested in susi-commercial-apps on Windows and Linux.
mod mcp_plug_double {
    use serde_json::{json, Value};
    use std::sync::{Arc, Once};
    use std::time::Duration;
    use susi_core::plane_bus::{PlaneBus, PlaneHandler};
    use susi_wire_client::host::{HostPolicy, HttpEndpoint, McpHost};

    struct Plug;

    impl PlaneHandler for Plug {
        fn handle(&self, topic: &str, payload: Value) -> Result<Value, String> {
            let config = &payload["config"];
            let addr = config["command"]
                .as_str()
                .and_then(|url| url.strip_prefix("http://"))
                .and_then(|rest| rest.strip_suffix("/mcp"))
                .ok_or("the double serves http://<addr>/mcp servers only")?;
            let timeout = Duration::from_millis(payload["lease_ms"].as_u64().unwrap_or(30_000));
            let endpoint = HttpEndpoint { addr, bearer: config["auth_token"].as_str(), timeout };
            let mut host = McpHost::new();
            host.connect_http_2026_07_28("peer", endpoint, HostPolicy::strict("susi-plug-double", "1.0.0"))
                .map_err(|e| e.to_string())?;
            if topic == "ecosystem_hub.mcp.list_tools" {
                return Ok(json!({ "tools": host.list_tools("peer").map_err(|e| e.to_string())? }));
            }
            let name = payload["name"].as_str().unwrap_or_default();
            let result = host
                .call_tool("peer", name, &payload["arguments"], None)
                .map_err(|e| e.to_string())?;
            let text = result.to_string();
            Ok(if result["isError"] == json!(true) {
                json!({ "error": text })
            } else {
                json!({ "result": text })
            })
        }
    }

    /// Serve both MCP topics in this test process (once).
    pub fn serve() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            PlaneBus::global().register("ecosystem_hub.mcp.call", Arc::new(Plug));
            PlaneBus::global().register("ecosystem_hub.mcp.list_tools", Arc::new(Plug));
        });
    }
}
