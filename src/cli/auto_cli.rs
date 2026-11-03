//! Zero-config auto substrate status (packs, MCP, models, peers).
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Serialize)]
struct AutoStatus {
    packs: serde_json::Value,
    leading_mcp_enabled: Vec<String>,
    leading_mcp_ready_not_enabled: Vec<String>,
    preferred_coding_model: Option<String>,
    agents_ready: usize,
    /// `None` (JSON null) when no framework readiness probe has run.
    frameworks_ready: Option<usize>,
    /// `probed`, or `not probed` until a framework readiness scan has
    /// recorded `agent-engines/ready.json`: core never probes framework
    /// adapters on its own (`susi auto prime` and `susi substrate status`
    /// record one on request; `susi frameworks doctor` reports live).
    frameworks_probe: &'static str,
    last_auto_prime: Option<serde_json::Value>,
    last_blackboard: Option<serde_json::Value>,
}

#[derive(Debug, Subcommand)]
pub enum AutoCommands {
    /// Show zero-config auto substrate readiness (default). A snapshot of
    /// what the last prime wrote: the full report is refreshed only by
    /// `auto prime` and commands that prime every catalog (missions,
    /// in-process `mcp call`); the daemon and other commands prime only
    /// what they read, and the daemon never probes agent frameworks.
    Status,
    /// Re-run auto-prime now (models/installed peers; never enables MCP)
    Prime,
}

fn load_json_file(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn collect_status(workspace: &Path) -> Result<AutoStatus> {
    let packs = serde_json::to_value(susi_sandbox::extensions::list_packs())?;

    let mut enabled = Vec::new();
    let mut ready_not = Vec::new();
    if let Ok(mcp) = susi_tools::LeadingMcpManager::new(workspace) {
        if let Ok(rows) = mcp.status() {
            for row in rows {
                let id = row
                    .get("mcp")
                    .and_then(|m| m.get("id"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let is_enabled = row
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let ready = row
                    .get("prerequisites_present")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if is_enabled {
                    enabled.push(id);
                } else if ready {
                    ready_not.push(id);
                }
            }
        }
    }

    let preferred = susi_gemi::models::coding_models::CodingModelManager::new()
        .ok()
        .and_then(|m| m.preferred());

    let agents_ready = load_json_file(
        &susi_paths::SusiDirs::config_dir()
            .join("execution-agents")
            .join("ready.json"),
    )
    .and_then(|v| v.as_array().map(|a| a.len()))
    .unwrap_or(0);
    let frameworks_ready = load_json_file(
        &susi_paths::SusiDirs::config_dir()
            .join("agent-engines")
            .join("ready.json"),
    )
    .and_then(|v| v.as_array().map(|a| a.len()));
    let frameworks_probe = if frameworks_ready.is_some() {
        "probed"
    } else {
        "not probed"
    };

    let last_auto_prime =
        load_json_file(&susi_paths::SusiDirs::config_dir().join("last_auto_prime.json"));
    let last_blackboard = load_json_file(&workspace.join(".susi").join("last_blackboard.json"));

    Ok(AutoStatus {
        packs,
        leading_mcp_enabled: enabled,
        leading_mcp_ready_not_enabled: ready_not,
        preferred_coding_model: preferred,
        agents_ready,
        frameworks_ready,
        frameworks_probe,
        last_auto_prime,
        last_blackboard: last_blackboard.map(|b| {
            serde_json::json!({
                "agent_count": b.get("agent_count").cloned().unwrap_or(serde_json::json!(0)),
                "path": ".susi/last_blackboard.json",
            })
        }),
    })
}

pub fn execute(action: Option<AutoCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(AutoCommands::Status) {
        AutoCommands::Status => {
            let status = collect_status(workspace)?;
            print_json(&status)?;
            if status.last_auto_prime.is_none() {
                eprintln!(
                    "hint: no full prime recorded yet; `susi auto prime` refreshes this snapshot"
                );
            }
        }
        AutoCommands::Prime => {
            let substrate = susi_paths::SusiDirs::substrate_home();
            let _ = std::fs::create_dir_all(&substrate);
            let report = susi_daemon::discovery_pipeline::prime_catalogs(&substrate);
            print_json(&report)?;
        }
    }
    Ok(())
}
