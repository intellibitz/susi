//! `susi os ecosystem`: the local AI ecosystem the CLI reports — hardware,
//! runtimes, models, agent and framework readiness, MCP servers and running
//! AI processes — and what the MCP `os_ecosystem` tool returns over the
//! `daemon.host` plane topic (zero-UI plan B4 group 4).

use serde_json::Value;
use std::path::Path;

fn command_path(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn command_version(path: &Path) -> Option<String> {
    std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            let text = if output.stdout.is_empty() {
                &output.stderr
            } else {
                &output.stdout
            };
            String::from_utf8_lossy(text)
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .to_string()
        })
        .filter(|version| !version.is_empty())
}

fn runtime_inventory() -> Vec<serde_json::Value> {
    [
        ("cuda", "nvidia-smi"),
        ("cuda-toolkit", "nvcc"),
        ("ollama", "ollama"),
        ("llama.cpp", "llama-server"),
        ("vllm", "vllm"),
        ("python", "python3"),
        ("uv", "uv"),
        ("docker", "docker"),
        ("podman", "podman"),
        ("node", "node"),
        ("npx", "npx"),
        ("rust", "cargo"),
    ]
    .into_iter()
    .map(|(id, command)| {
        let path = command_path(command);
        serde_json::json!({
            "id": id,
            "command": command,
            "installed": path.is_some(),
            "path": path,
            "version": path.as_deref().and_then(command_version),
        })
    })
    .collect()
}

/// `susi os ecosystem --json` for `workspace`; `loaded_models` reads the
/// daemon's inference cache (see [`crate::runtime_view::view`]).
pub fn report(
    workspace: &Path,
    loaded_models: impl FnOnce() -> Result<Value, String>,
) -> Result<Value, String> {
    let hardware = susi_gemi::models::hardware::HardwareProfiler::get_profile();
    let runtimes = runtime_inventory();
    let models = susi_core::plane_bus::gemi::ModelManager::list_models(workspace);
    let model_count = models.as_array().map_or(0, Vec::len);
    let agent_catalog = susi_gawd::external::catalog(susi_gawd::external::CatalogKind::Execution)
        .map_err(|e| e.to_string())?;
    let framework_catalog =
        susi_gawd::external::catalog(susi_gawd::external::CatalogKind::Framework)
            .map_err(|e| e.to_string())?;
    let agent_manager =
        susi_gawd::external::AgentManager::new(workspace).map_err(|e| e.to_string())?;
    let framework_manager =
        susi_gawd::external::AgentManager::frameworks(workspace).map_err(|e| e.to_string())?;
    let agents: Vec<_> = agent_catalog
        .into_iter()
        .map(|definition| {
            let readiness = agent_manager
                .adapter(&definition.id)
                .and_then(|adapter| adapter.preflight());
            serde_json::json!({
                "definition": definition,
                "ready": readiness.is_ok(),
                "detail": match readiness { Ok(detail) => detail, Err(error) => error.to_string() },
            })
        })
        .collect();
    let frameworks: Vec<_> = framework_catalog
        .into_iter()
        .map(|definition| {
            let readiness = framework_manager
                .adapter(&definition.id)
                .and_then(|adapter| adapter.preflight());
            serde_json::json!({
                "definition": definition,
                "ready": readiness.is_ok(),
                "detail": match readiness { Ok(detail) => detail, Err(error) => error.to_string() },
            })
        })
        .collect();
    let mcp = susi_tools::LeadingMcpManager::new(workspace)
        .and_then(|m| m.status())
        .map_err(|e| e.to_string())?;
    let runtime = crate::runtime_view::view(loaded_models);
    let processes = runtime["processes"].as_array().cloned().unwrap_or_default();
    let installed_runtimes = runtimes
        .iter()
        .filter(|runtime| runtime["installed"].as_bool() == Some(true))
        .count();
    let unhealthy_installed = runtimes
        .iter()
        .filter(|runtime| {
            runtime["installed"].as_bool() == Some(true) && runtime["version"].is_null()
        })
        .count();
    Ok(serde_json::json!({
        "hardware": hardware,
        "runtimes": runtimes,
        "models": models,
        "agents": agents,
        "frameworks": frameworks,
        "mcp_servers": mcp,
        "running_ai_processes": processes,
        "summary": {
            "installed_runtimes": installed_runtimes,
            "unhealthy_installed_runtimes": unhealthy_installed,
            "models": model_count,
            "agents": agents.len(),
            "frameworks": frameworks.len(),
            "mcp_servers": mcp.len(),
            "running_ai_processes": processes.len(),
        },
        "control": {
            "runtime": "susi os runtime [--json]",
            "services": "susi os manage service <status|start|stop|restart|logs>",
            "models": "susi os manage model <list|doctor|install|uninstall|load|unload|configure|reset|prefer|local>",
            "agents": "susi os manage agent <list|doctor|configure|reset|run|tasks|cancel>",
            "frameworks": "susi os manage framework <list|doctor|configure|reset|run|tasks|cancel>",
            "mcp": "susi os manage mcp <list|doctor|install|uninstall|configure|reset|status>",
            "privacy": "susi privacy <status|mode|grant|revoke>",
        }
    }))
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn command_path_returns_none_for_nonexistent_binary_name() {
        assert!(command_path("susi-definitely-missing-binary-xyz-999").is_none());
    }

    #[test]
    fn command_version_returns_none_for_missing_executable_path() {
        let missing = Path::new("susi-missing-dir-xyz/susi-missing-exe");
        assert!(command_version(missing).is_none());
    }

    #[test]
    fn runtime_inventory_contains_all_twelve_canonical_runtime_entries() {
        let inv = runtime_inventory();
        assert_eq!(inv.len(), 12);
        let ids: Vec<&str> = inv.iter().filter_map(|v| v["id"].as_str()).collect();
        assert!(ids.contains(&"cuda"));
        assert!(ids.contains(&"docker"));
        assert!(ids.contains(&"rust"));
    }

    #[test]
    fn runtime_inventory_entries_carry_id_command_and_installed_fields() {
        for item in runtime_inventory() {
            assert!(item["id"].is_string());
            assert!(item["command"].is_string());
            assert!(item["installed"].is_boolean());
        }
    }

    #[test]
    fn runtime_inventory_ids_are_unique_and_non_empty() {
        let inv = runtime_inventory();
        let mut seen = std::collections::HashSet::new();
        for item in &inv {
            let id = item["id"].as_str().expect("id string");
            assert!(!id.trim().is_empty());
            assert!(seen.insert(id.to_string()), "duplicate runtime id: {id}");
        }
        assert_eq!(seen.len(), inv.len());
    }
}
