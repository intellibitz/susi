//! SUSI policy for outbound MCP calls: lease and handshake budgets come from
//! the global `SusiConfig` (execution lease, cloud-scout timeout), transports
//! are authorized here, and the call itself is served over `PlaneBus` by the
//! plug host of `susi-commercial-apps` on `"ecosystem_hub.mcp.call"` /
//! `"ecosystem_hub.mcp.list_tools"`, which starts stdio servers with this
//! socket's own `susi_config::mcp` builder. With no plug host wired the call
//! names the command that serves it.

use crate::McpServerConfig;
use serde_json::{json, Value};
use std::time::Duration;
pub use susi_config::mcp::{
    authorize_mcp_transport, authorized_stdio_command_literal, spawn_authorized_stdio_peer,
};
use susi_config::{mac_policy::MacPolicy, mcp::authorize_mcp_transport as authorize_transport};
use susi_core::plane_bus::PlaneBus;

pub const TOPIC_MCP_CALL: &str = "ecosystem_hub.mcp.call";
pub const TOPIC_MCP_LIST_TOOLS: &str = "ecosystem_hub.mcp.list_tools";
/// Slack past the lease and handshake before a silent plug host is given up.
const BUS_SLACK: Duration = Duration::from_secs(5);

fn config() -> crate::susi_sandbox::manager::SusiConfig {
    crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default()
}

/// The plug host's answer on `topic`, or why there is none.
fn via_plug(
    topic: &str,
    mut payload: Value,
    lease: Duration,
    handshake: Duration,
) -> Result<Value, String> {
    let bus = PlaneBus::global();
    if !bus.is_wired(topic) {
        return Err(format!(
            "MCP servers are reached through the Susi Commercial Apps plug host: run \
             `susi-plug-host --supervise` (nothing serves {topic})"
        ));
    }
    payload["lease_ms"] = json!(lease.as_millis() as u64);
    payload["budget_policy"] = json!("core_transport_v1");
    payload["handshake_ms"] = json!(handshake.as_millis() as u64);
    payload["bus_timeout_ms"] = json!((lease + handshake + BUS_SLACK).as_millis() as u64);
    let resp = bus.request(topic, payload)?;
    match resp["error"].as_str() {
        Some(err) => Err(err.to_owned()),
        None => Ok(resp),
    }
}

pub fn call_blocking_result(
    config: McpServerConfig,
    name: String,
    arguments: Value,
    lease: Duration,
    handshake: Duration,
) -> Result<String, String> {
    if !arguments.is_object() {
        return Err("Tool arguments must be an object".to_string());
    }
    let payload = json!({ "config": config, "name": name, "arguments": arguments });
    let resp = via_plug(TOPIC_MCP_CALL, payload, lease, handshake)?;
    resp["result"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "MCP plug host answered without a `result`".to_string())
}

/// Probe a configured MCP server for its live tool catalog (name +
/// description), bounded by `lease` and `handshake`.
pub fn list_tools_blocking(
    config: McpServerConfig,
    lease: Duration,
    handshake: Duration,
) -> Result<Vec<(String, String)>, String> {
    let resp = via_plug(
        TOPIC_MCP_LIST_TOOLS,
        json!({ "config": config }),
        lease,
        handshake,
    )?;
    let tools = resp["tools"]
        .as_array()
        .ok_or_else(|| "MCP plug host answered without a `tools` array".to_string())?;
    Ok(tools
        .iter()
        .filter_map(|item| {
            let name = item["name"].as_str()?;
            let desc = item["description"]
                .as_str()
                .map_or_else(|| format!("MCP tool '{name}'"), str::to_owned);
            Some((name.to_owned(), desc))
        })
        .collect())
}

/// Tool call: full execution lease, full cloud-scout handshake budget.
pub(crate) fn call(
    config_: McpServerConfig,
    name: String,
    arguments: Value,
) -> Result<String, String> {
    authorize_transport(&config_, MacPolicy::global())?;
    let cfg = config();
    call_blocking_result(
        config_,
        name,
        arguments,
        Duration::from_secs(cfg.execution_lease_secs()),
        Duration::from_secs(cfg.cloud_scout_timeout_secs()),
    )
}

/// Catalog probe: lease capped at 30s, handshake at 10s.
pub(crate) fn list_tools(config_: McpServerConfig) -> Result<Vec<(String, String)>, String> {
    authorize_transport(&config_, MacPolicy::global())?;
    let cfg = config();
    list_tools_blocking(
        config_,
        Duration::from_secs(cfg.execution_lease_secs().min(30)),
        Duration::from_secs(cfg.cloud_scout_timeout_secs().min(10)),
    )
}

#[cfg(test)]
#[path = "tests/mcp_budget.rs"]
mod tests;
