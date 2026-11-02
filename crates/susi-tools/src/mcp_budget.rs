//! SUSI policy over the MCP vendor client: lease and handshake budgets come
//! from the global `SusiConfig` (execution lease, cloud-scout timeout).

use crate::McpServerConfig;
use serde_json::Value;
use std::time::Duration;
use susi_core::mac_policy::{actions, MacPolicy};

fn config() -> crate::susi_sandbox::manager::SusiConfig {
    crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default()
}

fn authorize_transport(server: &McpServerConfig, policy: &MacPolicy) -> Result<(), String> {
    let endpoint = server.http_scope()?;
    let resource = endpoint
        .as_ref()
        .map_or(server.command.as_str(), |(url, _)| url.as_str());
    let require = |action: &str| {
        if policy.is_permitted("susi", action, resource) {
            Ok(())
        } else {
            Err(format!(
                "MCP access denied: `{resource}` requires `{action}`"
            ))
        }
    };
    match endpoint.as_ref() {
        None => require(actions::PROCESS_EXEC)?,
        Some((_, false)) => require(actions::NETWORK_EGRESS)?,
        Some((_, true)) => {}
    }
    if let Some(declared) = server.extra.get("capabilities") {
        for capability in declared
            .as_array()
            .ok_or("MCP capabilities must be a list")?
        {
            match capability.as_str() {
                Some("tools") => {}
                Some("filesystem") => {
                    require(actions::FILESYSTEM_READ)?;
                    require(actions::FILESYSTEM_WRITE)?;
                }
                Some("network") => require(actions::NETWORK_EGRESS)?,
                Some(name) => return Err(format!("MCP capability `{name}` is not supported")),
                None => return Err("MCP capability must be a string".into()),
            }
        }
    }
    Ok(())
}

/// Tool call: full execution lease, full cloud-scout handshake budget.
pub(crate) fn call(
    config_: McpServerConfig,
    name: String,
    arguments: Value,
) -> Result<String, String> {
    authorize_transport(&config_, MacPolicy::global())?;
    let cfg = config();
    susi_vendor_mcp::call_blocking_result(
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
    susi_vendor_mcp::list_tools_blocking(
        config_,
        Duration::from_secs(cfg.execution_lease_secs().min(30)),
        Duration::from_secs(cfg.cloud_scout_timeout_secs().min(10)),
    )
}
