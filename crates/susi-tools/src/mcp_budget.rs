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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use susi_core::mac_policy::PrivacyMode;

    fn server(command: &str, capabilities: Option<Value>) -> McpServerConfig {
        let mut extra = std::collections::HashMap::new();
        if let Some(c) = capabilities {
            extra.insert("capabilities".to_string(), c);
        }
        McpServerConfig {
            command: command.to_string(),
            args: Vec::new(),
            env: None,
            extra,
        }
    }

    /// In-memory policy (no state dir): `LocalOnly` seeds filesystem grants
    /// but neither `process.exec` nor `network.egress`.
    fn local_only() -> MacPolicy {
        MacPolicy::new([7u8; 32], PrivacyMode::LocalOnly, false)
    }

    #[test]
    fn subprocess_server_requires_process_exec() {
        let err = authorize_transport(&server("npx", None), &local_only()).unwrap_err();
        assert!(err.contains(actions::PROCESS_EXEC), "{err}");
        assert!(err.contains("`npx`"), "{err}");

        let balanced = MacPolicy::new([7u8; 32], PrivacyMode::Balanced, false);
        assert_eq!(authorize_transport(&server("npx", None), &balanced), Ok(()));

        // An explicit grant scoped to the command opens it under LocalOnly.
        let policy = local_only();
        policy
            .grant("susi", actions::PROCESS_EXEC, "npx", None)
            .unwrap();
        assert_eq!(authorize_transport(&server("npx", None), &policy), Ok(()));
    }

    #[test]
    fn remote_http_requires_network_egress_but_loopback_does_not() {
        let policy = local_only();
        let err =
            authorize_transport(&server("https://mcp.example.com/mcp", None), &policy).unwrap_err();
        assert!(err.contains(actions::NETWORK_EGRESS), "{err}");

        for loopback in [
            "http://127.0.0.1:9090/mcp",
            "http://localhost:9093",
            "http://[::1]:9090/mcp",
        ] {
            assert_eq!(
                authorize_transport(&server(loopback, None), &policy),
                Ok(()),
                "{loopback} must stay local"
            );
        }

        let open = MacPolicy::new([7u8; 32], PrivacyMode::Open, false);
        assert_eq!(
            authorize_transport(&server("https://mcp.example.com/mcp", None), &open),
            Ok(())
        );
    }

    #[test]
    fn declared_capabilities_are_validated_and_gated() {
        let policy = local_only();
        let local = "http://127.0.0.1:9090/mcp";

        // Filesystem read+write are seeded even under LocalOnly.
        assert_eq!(
            authorize_transport(
                &server(local, Some(json!(["tools", "filesystem"]))),
                &policy
            ),
            Ok(())
        );

        // "network" re-imposes egress even on a loopback transport.
        let err =
            authorize_transport(&server(local, Some(json!(["network"]))), &policy).unwrap_err();
        assert!(err.contains(actions::NETWORK_EGRESS), "{err}");

        let err = authorize_transport(&server(local, Some(json!(["tools", "telepathy"]))), &policy)
            .unwrap_err();
        assert_eq!(err, "MCP capability `telepathy` is not supported");

        let err = authorize_transport(&server(local, Some(json!([42]))), &policy).unwrap_err();
        assert_eq!(err, "MCP capability must be a string");

        let err = authorize_transport(&server(local, Some(json!("tools"))), &policy).unwrap_err();
        assert_eq!(err, "MCP capabilities must be a list");

        // Empty list declares nothing extra.
        assert_eq!(
            authorize_transport(&server(local, Some(json!([]))), &policy),
            Ok(())
        );
    }

    #[test]
    fn transport_denial_precedes_capability_validation() {
        // A subprocess denied `process.exec` is refused before its (invalid)
        // capability list is even inspected.
        let err = authorize_transport(&server("npx", Some(json!("not-a-list"))), &local_only())
            .unwrap_err();
        assert!(err.contains(actions::PROCESS_EXEC), "{err}");
    }

    #[test]
    fn balanced_policy_permits_loopback_and_subprocess_with_filesystem_capability() {
        let balanced = MacPolicy::new([9u8; 32], PrivacyMode::Balanced, false);
        assert_eq!(
            authorize_transport(
                &server("npx", Some(json!(["tools", "filesystem"]))),
                &balanced
            ),
            Ok(())
        );
    }
}
