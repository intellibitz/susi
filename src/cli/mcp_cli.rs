//! Leading MCP server control plane (list/doctor/enable) plus stdio serve.
use crate::cli_json::print_json;
use anyhow::{bail, Result};
use clap::Subcommand;
use std::path::Path;
use susi_tools::{LeadingMcpManager, LeadingMcpOverride};

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
    /// Invoke a tool on a peer MCP endpoint (`host:port`) over the
    /// session-aware channel — the same path cluster replication uses.
    Call {
        /// Peer address, e.g. 127.0.0.1:9093
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
        /// Peer address, e.g. 127.0.0.1:9093
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
            let bearer = token.or_else(|| default_bearer(&addr));
            let result =
                susi_core::mcp_client::call_tool(&addr, &tool, &arguments, bearer.as_deref())
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(false)
        }
        McpCommands::Tools { addr, token } => {
            let bearer = token.or_else(|| default_bearer(&addr));
            let result = susi_core::mcp_client::session_call(
                &addr,
                "tools/list",
                &serde_json::json!({}),
                bearer.as_deref(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            // Compact table when the standard {tools:[{name,description}]}
            // shape comes back; raw JSON otherwise.
            if let Some(tools) = result.get("tools").and_then(|t| t.as_array()) {
                println!("{:<28} DESCRIPTION", "TOOL");
                for t in tools {
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
            } else {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Ok(false)
        }
        McpCommands::List | McpCommands::Status => {
            let manager = LeadingMcpManager::new(workspace)?;
            print_json(&manager.status()?)?;
            Ok(false)
        }
        McpCommands::Doctor { server } => {
            let manager = LeadingMcpManager::new(workspace)?;
            let servers = match server {
                Some(id) => vec![LeadingMcpManager::definition(&id)?],
                None => LeadingMcpManager::catalog()?,
            };
            let mut missing = false;
            for srv in servers {
                let result = manager.preflight(&srv.id);
                missing |= result.is_err();
                print_json(&serde_json::json!({
                    "server": srv.id,
                    "prerequisites_present": result.is_ok(),
                    "enabled": manager.is_enabled(&srv.id).unwrap_or(false),
                    "detail": match result { Ok(s) => s, Err(e) => e.to_string() }
                }))?;
            }
            if missing {
                bail!("one or more leading MCP servers need setup");
            }
            Ok(false)
        }
        McpCommands::Setup { server } => {
            let manager = LeadingMcpManager::new(workspace)?;
            let def = manager.effective(&server)?;
            print_json(&serde_json::json!({
                "server": def,
                "enabled": manager.is_enabled(&def.id)?,
                "instructions": "Install Node (npx), Docker (for GitHub MCP), and/or uv (uvx). Top 5: filesystem, github, context7, browser (Playwright; also chrome-devtools), sentry (SENTRY_ACCESS_TOKEN). Set env_keys as needed (GITHUB_PERSONAL_ACCESS_TOKEN, SENTRY_ACCESS_TOKEN, BRAVE_API_KEY, …). Run doctor, then enable to write ~/.susi/mcp_config.json. Override argv/runner with configure. No packages or credentials are provisioned implicitly."
            }))?;
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
            let manager = LeadingMcpManager::new(workspace)?;
            let over: LeadingMcpOverride = serde_json::from_slice(&std::fs::read(file)?)?;
            print_json(&manager.configure(&server, &over)?)?;
            Ok(false)
        }
        McpCommands::Reset { server } => {
            let manager = LeadingMcpManager::new(workspace)?;
            print_json(&manager.reset(&server)?)?;
            Ok(false)
        }
    }
}
