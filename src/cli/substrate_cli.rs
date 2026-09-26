//! Tier S substrate status: Pluggable, Sandbox, Governance, Host-contract, Reflexes.
use crate::cli_json::print_json;
use anyhow::Result;
use clap::Subcommand;
use std::path::Path;
use std::process::Command;
use susi_daemon::SusiDaemon;

#[derive(Debug, Subcommand)]
pub enum SubstrateCommands {
    /// Show Pluggable / Sandbox / Governance / Host-contract / Reflexes status (default)
    Status,
}

fn load_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn docker_available() -> bool {
    Command::new("docker")
        .arg("info")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .is_some_and(|s| s.success())
}

fn count_wasm_reflexes() -> usize {
    let dir = susi_paths::SusiDirs::data_dir().join("reflexes");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wasm"))
        })
        .count()
}

fn collect(workspace: &Path) -> serde_json::Value {
    let registry = susi_core::registry::CapabilityRegistry::global();
    let mut providers = registry.list_providers();
    providers.sort();
    let mut tools = registry.list_tools();
    tools.sort();
    let mut agents = registry.list_agents();
    agents.sort();

    let packs = susi_sandbox::extensions::list_packs();
    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();

    serde_json::json!({
        "kind": "substrate_status",
        "pluggable": {
            "providers": providers.len(),
            "tools": tools.len(),
            "agents": agents.len(),
            "provider_names": providers,
            "extension_packs": packs,
            "active_pack": susi_sandbox::extensions::active_pack().id,
        },
        "sandbox": {
            "wasmer_entry": "WasmHost::execute_untrusted_wasm",
            "wasmer_isolates": "plugins and reflexes (not host-native agents)",
            "docker_sandbox_exec": docker_available(),
            "docker_tool": "sandbox_exec",
        },
        "governance_first": {
            "mandate": 37,
            "sequencing": "SafetyAgent and SecurityAgent awaited before parallel fleet",
            "last": load_json(&workspace.join(".susi").join("last_governance.json")),
        },
        "host_contract": {
            "ports": {
                "gmcp": cfg.gmcp_port(),
                "gemi": cfg.gemi_port(),
                "udp": cfg.udp_discovery_port(),
                "gmcp_alias": cfg.gmcp_http_port(),
                "a2a_http": cfg.a2a_http_port(),
                "offset": cfg.port_offset(),
            },
            "ready": SusiDaemon::host_contract_ready(),
            "tcp_ready": SusiDaemon::host_contract_tcp_ready(),
            "udp_ready": SusiDaemon::host_contract_udp_ready(),
            "endpoints": SusiDaemon::host_contract_endpoints_report(),
        },
        "reflexes": {
            "wasm_count": count_wasm_reflexes(),
            "reflex_dir": susi_paths::SusiDirs::data_dir().join("reflexes"),
            "training_threshold": cfg.reflex_training_threshold(),
        },
        "concurrency": {
            "swarm": "rayon work-stealing",
            "daemon_http": "tokio",
            "shared_state": "parking_lot / DashMap",
        },
    })
}

pub fn execute(action: Option<SubstrateCommands>, workspace: &Path) -> Result<()> {
    match action.unwrap_or(SubstrateCommands::Status) {
        SubstrateCommands::Status => {
            let _ = susi_sandbox::extensions::ensure_extensions_substrate();
            susi_gemi::http_provider::apply_cloud_env_file();
            let substrate = susi_paths::SusiDirs::substrate_home();
            let _ = std::fs::create_dir_all(&substrate);
            let _ = susi_daemon::discovery_pipeline::prime_catalogs(&substrate);
            print_json(&collect(workspace))?;
        }
    }
    Ok(())
}
