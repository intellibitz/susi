//! Leading MCP server control plane (list/doctor/enable) plus stdio serve.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use susi_daemon::adapter_ops::{self, AdapterKind};
use susi_tools::LeadingMcpManager;

#[derive(Debug, Subcommand)]
pub enum McpCommands {
    /// Run SUSI's native MCP stdio server (default when bare `susi mcp`)
    Serve,
    /// Show the top MCP tool servers and enablement/readiness
    List,
    /// Check launchers and required env vars (does not start servers)
    Doctor { server: Option<String> },
    /// Show setup instructions and the effective adapter
    Setup { server: String },
    /// Write the leading MCP into ~/.susi/mcp_config.json
    Enable { server: String },
    /// Remove a leading MCP from ~/.susi/mcp_config.json
    Disable { server: String },
    /// Override runner/args/env_keys/package via JSON file
    Configure {
        server: String,
        file: std::path::PathBuf,
    },
    /// Remove a host override and restore the bundled leading MCP entry
    Reset { server: String },
    /// Show enablement status for the top MCP servers
    Status,
    /// Verify the pinned GMCP normative requirements ledger
    /// (.agents/gmcp): vendored corpus hashes, verbatim clauses, and
    /// complete row classification — the shipped production path for
    /// VC-203-001. `--drift` instead refetches every pinned upstream and
    /// fails closed on a changed or unreachable source.
    Ledger {
        /// Check upstream standards drift instead of the local corpus
        #[arg(long)]
        drift: bool,
        /// With `--drift`: compare this pre-fetched corpus directory
        /// (keyed by manifest document name) instead of the network —
        /// the deterministic path tests and air-gapped checks use.
        #[arg(long, requires = "drift")]
        refetched: Option<std::path::PathBuf>,
    },
    /// Invoke a tool on a peer MCP endpoint (`host:port`) over the
    /// session-aware channel — the same path cluster replication uses.
    Call {
        /// Peer address, e.g. 127.0.0.1:9093 (canonical GMCP-HTTP + port_offset)
        addr: String,
        /// Tool name as exposed by the peer's tools/list
        tool: String,
        /// JSON arguments object, e.g. '{"coordinator":"node-a"}' (default {})
        #[arg(default_value = "{}")]
        args: String,
        /// Bearer token (default: cluster peer bearer for remote
        /// members, ~/.susi/api_token for loopback)
        #[arg(long)]
        token: Option<String>,
    },
    /// List the tools a peer MCP endpoint exposes (`tools/list` over the
    /// session-aware channel) — discover the surface before `mcp call`.
    Tools {
        /// Peer address, e.g. 127.0.0.1:9093 (canonical GMCP-HTTP + port_offset)
        addr: String,
        /// Bearer token (default: cluster peer bearer for remote
        /// members, ~/.susi/api_token for loopback)
        #[arg(long)]
        token: Option<String>,
    },
}

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
    std::fs::read_to_string(susi_paths::SusiDirs::config_dir().join("api_token"))
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
                .map_err(|e| anyhow::anyhow!("args must be a JSON object: {e}"))?;
            let result = if is_network_endpoint(&addr) {
                let bearer = token.or_else(|| default_bearer(&addr));
                susi_core::mcp_client::call_tool(&addr, &tool, &arguments, bearer.as_deref())
                    .map_err(|e| anyhow::anyhow!("{e}"))?
            } else {
                // This control-plane command returns before normal CLI composition.
                // A registered stdio server needs the tools plane to dispatch.
                susi_daemon::composition::wire_plane_handlers();
                susi_gmcp::direct_call::call_tool_direct(&addr, &tool, &arguments, workspace)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
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
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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
                .map_err(|e| anyhow::anyhow!("{e}"))?;
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
                .map_err(anyhow::Error::msg)?;
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
                    .map_err(anyhow::Error::msg)?,
            )?;
            Ok(false)
        }
        McpCommands::Enable { server } => {
            let manager = LeadingMcpManager::new(workspace)?;
            let cfg = manager.enable(&server)?;
            print_json(&serde_json::json!({
                "server": server,
                "enabled": true,
                "config": cfg
            }))?;
            Ok(false)
        }
        McpCommands::Disable { server } => {
            let manager = LeadingMcpManager::new(workspace)?;
            let removed = manager.disable(&server)?;
            print_json(&serde_json::json!({
                "server": server,
                "removed": removed
            }))?;
            Ok(false)
        }
        McpCommands::Configure { server, file } => {
            let doc: serde_json::Value = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(
                &adapter_ops::configure(AdapterKind::Mcp, &server, doc, workspace)
                    .map_err(anyhow::Error::msg)?,
            )?;
            Ok(false)
        }
        McpCommands::Reset { server } => {
            print_json(
                &adapter_ops::reset(AdapterKind::Mcp, &server, workspace)
                    .map_err(anyhow::Error::msg)?,
            )?;
            Ok(false)
        }
    }
}
