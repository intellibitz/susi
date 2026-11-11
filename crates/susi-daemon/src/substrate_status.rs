//! Tier S substrate status: Pluggable, Sandbox, Governance, Host-contract,
//! Reflexes. The document `susi substrate status` prints and the MCP
//! `substrate_status` tool returns over the `daemon.substrate.status` plane
//! topic (zero-UI plan B4 group 5): one implementation for both.

use crate::discovery_pipeline::Catalogs;
use std::path::Path;

fn load_json(path: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

fn docker_available() -> bool {
    susi_gawd::external::docker_present()
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

fn collect(workspace: &Path, docker_sandbox_exec: bool) -> serde_json::Value {
    let registry = susi_core::registry::CapabilityRegistry::global();
    let mut providers = registry.list_providers();
    providers.sort();
    let mut tools = registry.list_tools();
    tools.sort();
    let mut agents = registry.list_agents();
    agents.sort();

    // One probe pass: `host_contract_ready` would repeat the TCP probes, and
    // each refused loopback port costs its full timeout on Windows.
    let tcp_ready = crate::SusiDaemon::host_contract_tcp_ready();
    let udp_ready = crate::SusiDaemon::host_contract_udp_ready();
    let packs = crate::susi_sandbox::extensions::list_packs();
    let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();

    serde_json::json!({
        "kind": "substrate_status",
        "pluggable": {
            "providers": providers.len(),
            "tools": tools.len(),
            "agents": agents.len(),
            "provider_names": providers,
            "extension_packs": packs,
            "active_pack": crate::susi_sandbox::extensions::active_pack().id,
        },
        "sandbox": {
            "wasmtime_entry": "WasmHost::execute_untrusted_wasm",
            "wasmtime_isolates": "plugins and reflexes (not host-native agents)",
            "docker_sandbox_exec": docker_sandbox_exec,
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
            // The live daemon's published block (ports.json): its offset,
            // and the configured one it fell back from when they differ.
            "published": susi_paths::ports::live_daemon_ports().map(|live| serde_json::json!({
                "offset": live.offset,
                "fallback": live.is_fallback(),
                "fallback_from": live.fallback_from,
                "pid": live.pid,
            })),
            "ready": tcp_ready && udp_ready,
            "tcp_ready": tcp_ready,
            "udp_ready": udp_ready,
            "endpoints": crate::SusiDaemon::host_contract_endpoints_report(),
        },
        "reflexes": {
            "wasm_count": count_wasm_reflexes(),
            "reflex_dir": susi_paths::SusiDirs::data_dir().join("reflexes"),
            "training_threshold": cfg.reflex_training_threshold(),
            "distillation": susi_gawd::reflex_trainer::distillation_summary(workspace),
            "suppressed": susi_gemi::pulse::SusiPulse::suppressed_reflexes(workspace),
            "tier0": susi_gemi::engines::alpha::SusiAlphaModel::inventory(
                &susi_paths::SusiDirs::config_dir()
            ),
            "tier0_roi": susi_gemi::pulse::SusiPulse::roi_summary(workspace),
        },
        "concurrency": {
            "swarm": "std scoped threads",
            "daemon_http": "tokio",
            "shared_state": "std::sync locks (non-poisoning wrappers)",
        },
    })
}

/// What `substrate status` primes: its `pluggable.agents` count is the agent
/// capabilities the execution-agent and framework scans register.
/// Coding-model auto-prefer feeds nothing it prints.
pub const PRIMED: Catalogs = Catalogs::AGENTS;

/// `susi substrate status`: prime the catalogs it counts, probe Docker in
/// parallel, and collect the status document for `workspace`.
pub fn report(workspace: &Path) -> serde_json::Value {
    let substrate = susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);
    // `docker info` takes ~0.7 s even when Docker is down: run it
    // while the catalogs prime instead of after them.
    // A scoped probe, joined before this returns: no worker outlives the
    // request (daemon workers spawn only through the supervisor).
    let docker_sandbox_exec = std::thread::scope(|scope| {
        let docker = scope.spawn(docker_available);
        let _ = crate::boot_trace::phase("substrate: prime catalogs", || {
            crate::discovery_pipeline::prime_selected_catalogs(&substrate, PRIMED)
        });
        docker.join().unwrap_or(false)
    });
    collect(workspace, docker_sandbox_exec)
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn load_json_returns_none_for_missing_or_invalid_file() {
        let dir =
            std::env::temp_dir().join(format!("susi-sub-status-inline-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load_json(&dir.join("missing.json")).is_none());

        let invalid = dir.join("invalid.json");
        std::fs::write(&invalid, "{not json").unwrap();
        assert!(load_json(&invalid).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_json_parses_valid_json_document() {
        let dir =
            std::env::temp_dir().join(format!("susi-sub-status-valid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let valid = dir.join("gov.json");
        std::fs::write(&valid, r#"{"verdict":"pass","mandate":37}"#).unwrap();
        let val = load_json(&valid).unwrap();
        assert_eq!(val["verdict"], "pass");
        assert_eq!(val["mandate"], 37);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn primed_constant_selects_agents_catalog() {
        assert_eq!(PRIMED, Catalogs::AGENTS);
    }

    #[test]
    fn count_wasm_reflexes_returns_usize_without_panicking() {
        let _count = count_wasm_reflexes();
    }

    #[test]
    fn collect_produces_structured_substrate_status_with_concurrency_model() {
        let dir =
            std::env::temp_dir().join(format!("susi-sub-status-collect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let doc = collect(&dir, false);
        assert_eq!(doc["sandbox"]["docker_sandbox_exec"], false);
        assert_eq!(doc["concurrency"]["swarm"], "std scoped threads");
        assert_eq!(doc["concurrency"]["daemon_http"], "tokio");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
