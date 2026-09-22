//! Leading MCP server control plane (list/doctor/enable) plus stdio serve.
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
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    println!(
        "{}",
        susi_agents::external::redact(&serde_json::to_string_pretty(value)?)
    );
    Ok(())
}

/// Returns `true` when the caller should run the native stdio MCP server.
pub fn execute(action: Option<McpCommands>, workspace: &Path) -> Result<bool> {
    match action.unwrap_or(McpCommands::Serve) {
        McpCommands::Serve => Ok(true),
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
                "instructions": "Install Node (npx) and/or uv (uvx). Set any env_keys (e.g. BRAVE_API_KEY, GITHUB_PERSONAL_ACCESS_TOKEN, COMPOSIO_API_KEY, POSTGRES_URL). Autonomy MVA: filesystem, bash, browser, brave-search. Run doctor, then enable to write ~/.susi/mcp_config.json. Override argv/runner with configure. No packages or credentials are provisioned implicitly."
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
