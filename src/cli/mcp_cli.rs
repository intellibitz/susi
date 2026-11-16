//! Leading MCP server control plane (list/doctor/enable) plus stdio serve.
use crate::anyhow::{bail, Result};
use crate::cli_json::print_json;
use std::path::Path;
pub use susi_cli_defs::mcp::McpCommands;
use susi_daemon::adapter_ops::{self, AdapterKind};
use susi_tools::LeadingMcpManager;

/// Default credential for a peer MCP call when `--token` isn't given.
/// The host api_token only authenticates on THIS host — a remote member
/// validates it against its own token and refuses — so non-loopback
/// targets get the cluster-derived peer bearer (identical on every
/// member) and loopback gets the host token.
fn default_bearer(addr: &str) -> Option<String> {
    let loopback = addr.split(':').next().is_some_and(|h| {
        h == "localhost"
            || h.parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
    });
    if !loopback {
        if let Some(pb) = susi_config::cluster_key::peer_bearer() {
            return Some(pb);
        }
    }
    std::fs::read_to_string(susi_paths::host_token_path())
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

pub(crate) fn is_network_endpoint(addr: &str) -> bool {
    if addr.starts_with("http://") || addr.starts_with("https://") {
        return true;
    }
    if addr.parse::<std::net::SocketAddr>().is_ok() {
        return true;
    }
    if let Some((host, port)) = addr.rsplit_once(':') {
        if port.parse::<u16>().is_ok()
            && (host == "localhost"
                || host.parse::<std::net::IpAddr>().is_ok()
                || host.contains('.'))
        {
            return true;
        }
    }
    false
}

/// Returns `true` when the caller should run the native stdio MCP server.
pub fn execute(action: Option<McpCommands>, workspace: &Path) -> Result<bool> {
    match action.unwrap_or(McpCommands::Serve) {
        McpCommands::Serve => Ok(true),
        McpCommands::Call {
            addr,
            tool,
            args,
            token,
        } => {
            let arguments: serde_json::Value = serde_json::from_str(&args)
                .map_err(|e| crate::anyhow::anyhow!("args must be a JSON object: {e}"))?;
            let result = if is_network_endpoint(&addr) {
                let bearer = token.or_else(|| default_bearer(&addr));
                susi_core::mcp_client::call_tool(&addr, &tool, &arguments, bearer.as_deref())
                    .map_err(|e| crate::anyhow::anyhow!("{e}"))?
            } else {
                // This control-plane command returns before normal CLI composition.
                // A registered stdio server needs the tools plane to dispatch.
                susi_daemon::composition::wire_plane_handlers();
                susi_gmcp::direct_call::call_tool_direct(&addr, &tool, &arguments, workspace)
                    .map_err(|e| crate::anyhow::anyhow!("{e}"))?
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(false)
        }
        McpCommands::Tools { addr, token } => {
            let bearer = token.or_else(|| default_bearer(&addr));
            let mut params = serde_json::json!({});
            let mut tools = Vec::new();
            // GMCP pages tools/list at 100. Follow nextCursor so names past
            // the first page (for example time:get_current_time) are visible.
            const MAX_TOOLS_LIST_PAGES: usize = 50;
            let mut complete = false;
            for _ in 0..MAX_TOOLS_LIST_PAGES {
                let result = susi_core::mcp_client::session_call(
                    &addr,
                    "tools/list",
                    &params,
                    bearer.as_deref(),
                )
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                let Some(page) = result.get("tools").and_then(|t| t.as_array()) else {
                    if tools.is_empty() {
                        println!("{}", serde_json::to_string_pretty(&result)?);
                    }
                    complete = true;
                    break;
                };
                tools.extend(page.iter().cloned());
                let cursor = result
                    .get("nextCursor")
                    .or_else(|| result.get("next_cursor"))
                    .and_then(|v| v.as_str())
                    .filter(|cursor| !cursor.is_empty());
                match cursor {
                    Some(cursor) => params = serde_json::json!({ "cursor": cursor }),
                    None => {
                        complete = true;
                        break;
                    }
                }
            }
            if !complete {
                bail!("tools/list still paging after {MAX_TOOLS_LIST_PAGES} pages");
            }
            if !tools.is_empty() {
                println!("{:<28} DESCRIPTION", "TOOL");
                for t in &tools {
                    let name = t.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let desc = t
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .chars()
                        .take(80)
                        .collect::<String>();
                    println!("{name:<28} {desc}");
                }
            }
            Ok(false)
        }
        McpCommands::Ledger { drift, refetched } => {
            if drift {
                // Same document as the MCP `ledger_verify` tool with `drift`.
                let report = susi_gmcp::requirements_ledger::drift_report_json(
                    workspace,
                    refetched.as_deref(),
                )
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                print_json(&report)?;
                let findings = report["findings"].as_array().map_or(0, Vec::len);
                if findings > 0 {
                    bail!(
                        "upstream drift: {findings} finding(s) — the pinned corpus is stale and must be re-pinned"
                    );
                }
                return Ok(false);
            }
            let report = susi_gmcp::requirements_ledger::verify_report_json(workspace)
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            print_json(&report)?;
            Ok(false)
        }
        McpCommands::List | McpCommands::Status => {
            let manager = LeadingMcpManager::new(workspace)?;
            print_json(&manager.status()?)?;
            Ok(false)
        }
        // `susi_daemon::adapter_ops` builds the doctor/setup/configure/reset
        // documents; the MCP `adapter_*` tools return the same ones.
        McpCommands::Doctor { server } => {
            let rows = adapter_ops::doctor(AdapterKind::Mcp, server.as_deref(), workspace)
                .map_err(crate::anyhow::Error::msg)?;
            for row in &rows {
                print_json(row)?;
            }
            if !adapter_ops::all_ready(&rows) {
                bail!("{}", adapter_ops::needs_setup_message(AdapterKind::Mcp));
            }
            Ok(false)
        }
        McpCommands::Setup { server } => {
            print_json(
                &adapter_ops::setup(AdapterKind::Mcp, &server, workspace)
                    .map_err(crate::anyhow::Error::msg)?,
            )?;
            Ok(false)
        }
        McpCommands::Enable { server } => {
            let manager = LeadingMcpManager::new(workspace)?;
            let cfg = manager.enable(&server)?;
            print_json(&serde_json::json!({ "server": server, "enabled": true, "config": cfg}))?;
            Ok(false)
        }
        McpCommands::Disable { server } => {
            let manager = LeadingMcpManager::new(workspace)?;
            let removed = manager.disable(&server)?;
            print_json(&serde_json::json!({ "server": server, "removed": removed}))?;
            Ok(false)
        }
        McpCommands::Configure { server, file } => {
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(
                &adapter_ops::configure(AdapterKind::Mcp, &server, doc, workspace)
                    .map_err(crate::anyhow::Error::msg)?,
            )?;
            Ok(false)
        }
        McpCommands::Reset { server } => {
            print_json(
                &adapter_ops::reset(AdapterKind::Mcp, &server, workspace)
                    .map_err(crate::anyhow::Error::msg)?,
            )?;
            Ok(false)
        }
        McpCommands::Discover {
            addr,
            legacy,
            token,
        } => {
            let mut s = open_host_cli_session(&addr, token, legacy)?;
            let doc = s.discover().map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            print_json(&doc)?;
            let _ = s.close();
            Ok(false)
        }
        McpCommands::Resources {
            addr,
            templates,
            token,
        } => {
            let mut s = open_host_cli_session(&addr, token, false)?;
            let resources = s
                .list_all_resources()
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            let out = if templates {
                let tmpl = s
                    .list_all_resource_templates()
                    .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                serde_json::json!({ "resources": resources, "resourceTemplates": tmpl })
            } else {
                serde_json::json!({ "resources": resources })
            };
            print_json(&out)?;
            let _ = s.close();
            Ok(false)
        }
        McpCommands::Read {
            addr,
            uri,
            app,
            token,
        } => {
            let mut s = open_host_cli_session(&addr, token, false)?;
            if app {
                let info = s
                    .inspect_app_resource(&uri)
                    .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                print_json(
                    &serde_json::json!({ "uri": info.uri, "mimeType": info.mime_type, "payloadBytes": info.payload_bytes, "csp": info.csp, "permissions": info.permissions, "domain": info.domain, "meta": info.meta,}),
                )?;
            } else {
                let doc = s
                    .read_resource(&uri)
                    .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                print_json(&doc)?;
            }
            let _ = s.close();
            Ok(false)
        }
        McpCommands::Prompts { addr, token } => {
            let mut s = open_host_cli_session(&addr, token, false)?;
            let prompts = s
                .list_all_prompts()
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            print_json(&serde_json::json!({ "prompts": prompts }))?;
            let _ = s.close();
            Ok(false)
        }
        McpCommands::Prompt {
            addr,
            name,
            args,
            token,
        } => {
            let parsed: serde_json::Value = serde_json::from_str(&args)
                .map_err(|e| crate::anyhow::anyhow!("args must be valid JSON: {e}"))?;
            let mut s = open_host_cli_session(&addr, token, false)?;
            let doc = s
                .get_prompt(&name, Some(&parsed))
                .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            print_json(&doc)?;
            let _ = s.close();
            Ok(false)
        }
        McpCommands::Complete {
            addr,
            kind,
            target,
            arg,
            value,
            token,
        } => {
            let mut s = open_host_cli_session(&addr, token, false)?;
            let doc = match kind.as_str() {
                "prompt" | "ref/prompt" => s.complete_prompt(&target, &arg, &value, None),
                "resource" | "ref/resource" => s.complete_resource(&target, &arg, &value, None),
                other => {
                    bail!("unsupported completion kind `{other}` (expected `prompt` or `resource`)")
                }
            }
            .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
            print_json(&doc)?;
            let _ = s.close();
            Ok(false)
        }
        McpCommands::Task {
            addr,
            op,
            task_id,
            responses,
            token,
        } => {
            let mut s = open_host_cli_session(&addr, token, false)?;
            match op.as_str() {
                "get" => {
                    let snap = s
                        .get_task(&task_id)
                        .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                    print_json(
                        &serde_json::json!({ "taskId": snap.handle.task_id, "status": snap.handle.status.as_str(), "statusMessage": snap.handle.status_message, "result": snap.result, "error": snap.error, "inputRequests": snap.input_requests.iter().map(|e| serde_json::json!({ "key": e.key, "method": e.method, "params": e.params,})).collect::<Vec<_>>(),}),
                    )?;
                }
                "update" => {
                    let parsed: serde_json::Value = serde_json::from_str(&responses)
                        .map_err(|e| crate::anyhow::anyhow!("responses must be valid JSON: {e}"))?;
                    s.update_task(&task_id, &parsed)
                        .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                    print_json(&serde_json::json!({ "taskId": task_id, "updated": true }))?;
                }
                "cancel" => {
                    s.cancel_task(&task_id)
                        .map_err(|e| crate::anyhow::anyhow!("{e}"))?;
                    print_json(&serde_json::json!({ "taskId": task_id, "cancelled": true }))?;
                }
                other => {
                    bail!("unsupported task op `{other}` (expected `get`, `update`, or `cancel`)")
                }
            }
            let _ = s.close();
            Ok(false)
        }
    }
}

fn open_host_cli_session(
    addr: &str,
    token: Option<String>,
    force_legacy: bool,
) -> Result<susi_wire_client::mcp::Session> {
    use std::time::Duration;
    use susi_wire_client::mcp::Session;
    let clean = addr
        .strip_prefix("http://")
        .or_else(|| addr.strip_prefix("https://"))
        .unwrap_or(addr)
        .split('/')
        .next()
        .unwrap_or(addr);
    let bearer = token.or_else(|| default_bearer(clean));
    let timeout = Duration::from_secs(20);
    if force_legacy {
        return Session::open(clean, bearer.as_deref(), timeout)
            .map_err(|e| crate::anyhow::anyhow!("{e}"));
    }
    match Session::open_2026_07_28(clean, bearer.as_deref(), timeout) {
        Ok(s) => Ok(s),
        Err(_) => Session::open(clean, bearer.as_deref(), timeout)
            .map_err(|e| crate::anyhow::anyhow!("{e}")),
    }
}
