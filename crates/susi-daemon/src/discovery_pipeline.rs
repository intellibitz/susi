//! Zero-config substrate discovery pipeline: cloud keys, extension packs,
//! local inference engines, MCP servers, catalogs, and periodic rediscovery.
//! Swarm Cell spawning lives in [`crate::auto_discovery`].

use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

// The vendored `susi_core` inside susi-gemi shares the process capability
// catalog with the real `susi_core` (bus rendezvous), so its global() is the
// same registry — and it is the type susi_gemi's discovery entrypoints take.
use susi_gemi::susi_core::registry::CapabilityRegistry;

/// Universal Autonomous Substrate Bootstrapper
/// Implements Pillar 8 (Autonomous Provisioning) by triggering zero-config
/// capability discovery across the local system — packs, MCP, models, peers,
/// and local engines are primed when host prerequisites are already present.
pub async fn bootstrap_zero_config_substrate() {
    let registry = CapabilityRegistry::global();
    let substrate = crate::susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);

    if std::env::var("SUSI_VERBOSE").is_ok() {
        eprintln!("[BOOTSTRAP] Initiating Zero-Config Substrate Discovery...");
    }

    // Keys first so MCP/model/peer preflights see ~/.susi/cloud.env.
    susi_gemi::http_provider::apply_cloud_env_file();

    // 0. Extension packs: seed ~/.susi/extensions/default, auto-discover packs.
    match crate::susi_sandbox::extensions::ensure_extensions_substrate() {
        Ok(pack) => {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!(
                    "[BOOTSTRAP] Extension pack `{}` ready at {}",
                    pack.id,
                    pack.root.display()
                );
            }
        }
        Err(e) => {
            eprintln!("[BOOTSTRAP] Extension pack seed skipped: {}", e);
        }
    }

    // 0b. Auto-enable leading MCP / prefer coding models / admit ready peers.
    prime_catalogs(&substrate);

    // 0c. Awaken local inference daemons already installed on PATH.
    try_awaken_local_engines();

    // Drop HTTP providers that went unhealthy since the last pass so a killed
    // Ollama/vLLM does not keep winning routing. Candle is permanent fallback.
    prune_unhealthy_providers(registry).await;

    // 1. Probe for Local Model Inference Engines (Ollama, vLLM, llama.cpp, etc.)
    //    plus any configured OpenAI-compat inference_endpoints bases (open admission).
    susi_gemi::http_provider::auto_discover_local_engines(registry).await;

    // 1b. Register configured cloud + local-with-model endpoints when keys/models present.
    susi_gemi::http_provider::register_configured_cloud_endpoints(registry);

    // 2. Discover and Provision MCP Tools (any entry in ~/.susi/mcp_config.json).
    susi_gmcp::mcp_wrapper::auto_discover_mcp();

    // 2b. MCP servers that expose chat/LLM tools → inference Providers so they
    // join cloud prefer / slow-local escalation (not tools-only).
    susi_gemi::mcp_provider::register_mcp_inference_providers(registry);

    // 3. Fallback: Ensure Candle (Local Edge) is always provisioned
    if registry.get_provider("Candle (Local)").is_none() {
        registry.register_provider(susi_gemi::candle_provider::CandleProvider);
    }

    // 3b. Local embedder: susi-gmcp's vendored copy self-registers through
    // the shared IPC rendezvous, so `gemi.infer.embed` and /v1/embeddings
    // work with zero external configuration.
    susi_gmcp::embed_provider::register_local_embed_provider();

    // 4. Background weight priming (hardware-optimal ladder) — idempotent.
    susi_gemi::models::ModelManager::spawn_background_hardware_model_provisioner(&substrate);

    if std::env::var("SUSI_VERBOSE").is_ok() {
        eprintln!("[BOOTSTRAP] Capability Registry Loaded:");
        eprintln!(" - Providers: {:?}", registry.list_providers());
        eprintln!(" - Tools: {:?}", registry.list_tools());
    }
}

/// Auto-admit host-ready ecosystem components (MCP, coding models, peers).
/// Persists a glass-box report under `~/.susi/last_auto_prime.json`.
pub fn prime_catalogs(substrate: &Path) -> serde_json::Value {
    let mut mcp_enabled: Vec<String> = Vec::new();
    let mut preferred_model: Option<String> = None;
    let mut agents_ready = 0usize;
    let mut frameworks_ready = 0usize;

    match susi_tools::LeadingMcpManager::new(substrate) {
        Ok(mcp) => match mcp.auto_enable_ready() {
            Ok(ids) => {
                if !ids.is_empty() && std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] Auto-enabled MCP: {:?}", ids);
                }
                mcp_enabled = ids;
            }
            Err(e) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] MCP auto-enable skipped: {e}");
                }
            }
        },
        Err(e) => {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[BOOTSTRAP] Leading MCP manager unavailable: {e}");
            }
        }
    }

    match susi_gemi::coding_models::CodingModelManager::new() {
        Ok(models) => match models.auto_prefer_best_ready() {
            Ok(Some(id)) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] Preferred coding model: {id}");
                }
                preferred_model = Some(id);
            }
            Ok(None) => {}
            Err(e) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] Coding-model auto-prefer skipped: {e}");
                }
            }
        },
        Err(e) => {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[BOOTSTRAP] CodingModelManager unavailable: {e}");
            }
        }
    }

    for kind in [
        susi_agents::external::CatalogKind::Execution,
        susi_agents::external::CatalogKind::Framework,
    ] {
        match susi_agents::external::AgentManager::for_kind(substrate, kind) {
            Ok(mgr) => match mgr.auto_prime_ready() {
                Ok(ready) => {
                    if !ready.is_empty() && std::env::var("SUSI_VERBOSE").is_ok() {
                        eprintln!("[BOOTSTRAP] Auto-ready {}: {}", kind.label(), ready.len());
                    }
                    match kind {
                        susi_agents::external::CatalogKind::Execution => {
                            agents_ready = ready.len();
                        }
                        susi_agents::external::CatalogKind::Framework => {
                            frameworks_ready = ready.len();
                        }
                    }
                }
                Err(e) => {
                    if std::env::var("SUSI_VERBOSE").is_ok() {
                        eprintln!("[BOOTSTRAP] {} auto-prime skipped: {e}", kind.label());
                    }
                }
            },
            Err(e) => {
                if std::env::var("SUSI_VERBOSE").is_ok() {
                    eprintln!("[BOOTSTRAP] {} manager unavailable: {e}", kind.label());
                }
            }
        }
    }

    let pack = crate::susi_sandbox::extensions::active_pack();
    let report = serde_json::json!({
        "kind": "auto_prime",
        "active_pack": pack.id,
        "pack_root": pack.root,
        "mcp_enabled": mcp_enabled,
        "preferred_coding_model": preferred_model,
        "agents_ready": agents_ready,
        "frameworks_ready": frameworks_ready,
    });
    let path = crate::susi_paths::SusiDirs::config_dir().join("last_auto_prime.json");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()),
    );
    report
}

/// If a well-known local engine binary is on PATH but its API port is closed,
/// start the daemon once (best-effort; never blocks bootstrap on failure).
fn try_awaken_local_engines() {
    static AWOKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if AWOKEN.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return;
    }
    if cfg!(test) {
        return;
    }
    // Ollama: `ollama serve` when binary present and :11434 is closed.
    if port_closed("127.0.0.1:11434") && binary_on_path("ollama") {
        let _ = Command::new("ollama")
            .arg("serve")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        if std::env::var("SUSI_VERBOSE").is_ok() {
            eprintln!("[BOOTSTRAP] Awakened local engine: ollama serve");
        }
        // Brief settle so the subsequent /models probe can succeed this pass.
        std::thread::sleep(Duration::from_millis(400));
    }
}

fn port_closed(addr: &str) -> bool {
    let Ok(sock) = addr.parse() else {
        return true;
    };
    TcpStream::connect_timeout(&sock, Duration::from_millis(150)).is_err()
}

fn binary_on_path(name: &str) -> bool {
    let Ok(path) = std::env::var("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&path) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if candidate
                    .metadata()
                    .is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
                {
                    return true;
                }
            }
            #[cfg(not(unix))]
            {
                return true;
            }
        }
    }
    false
}

async fn prune_unhealthy_providers(registry: &CapabilityRegistry) {
    // Probe concurrently with a deadline so an unresponsive capability cannot
    // prevent automatic discovery and recovery of the rest of the substrate.
    let mut probes = tokio::task::JoinSet::new();
    for name in registry.list_providers() {
        if name == "Candle (Local)" {
            continue;
        }
        let Some(provider) = registry.get_provider(&name) else {
            continue;
        };
        probes.spawn(async move {
            let healthy = tokio::time::timeout(Duration::from_secs(5), provider.is_healthy())
                .await
                .is_ok_and(|result| result.unwrap_or(false));
            (name, healthy)
        });
    }
    while let Some(result) = probes.join_next().await {
        if let Ok((name, false)) = result {
            registry.unregister_provider(&name);
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!("[BOOTSTRAP] Pruned unhealthy provider: {}", name);
            }
        }
    }
}

/// Spawn a background loop that re-runs zero-config discovery so engines/tools
/// that appear after daemon start are hot-plugged without restart.
/// `interval_secs == 0` disables the loop (initial bootstrap still runs).
pub fn spawn_periodic_rediscovery(interval_secs: u64) {
    if interval_secs == 0 {
        return;
    }
    std::thread::Builder::new()
        .name("susi-capability-rediscovery".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("[SusiDaemon] Capability rediscovery loop aborted: {}", e);
                    return;
                }
            };
            let interval = Duration::from_secs(interval_secs);
            loop {
                std::thread::sleep(interval);
                runtime.block_on(bootstrap_zero_config_substrate());
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rediscovery_disabled_when_interval_zero() {
        // Must not spawn / hang — just verify the early-return path.
        spawn_periodic_rediscovery(0);
    }

    // start_paused: tokio auto-advances the clock while idle, so the hung
    // provider's 5s probe deadline resolves instantly instead of in real
    // time — the hang-recovery assertion is unchanged.
    #[tokio::test(start_paused = true)]
    async fn prune_removes_unhealthy_non_candle_providers() {
        use susi_gemi::susi_core::provider::{BoxFuture, Provider};
        use susi_gemi::susi_core::susi_error::EaiResult;

        struct Unhealthy {
            hang: bool,
        }
        impl Provider for Unhealthy {
            fn name(&self) -> &str {
                if self.hang {
                    "ollama-hung"
                } else {
                    "ollama-dead"
                }
            }
            fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
                Box::pin(async move {
                    if self.hang {
                        std::future::pending::<()>().await;
                    }
                    Ok(false)
                })
            }
            fn generate(&self, _: &str) -> BoxFuture<'_, EaiResult<String>> {
                Box::pin(async { Ok(String::new()) })
            }
            fn embed(&self, _: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
                Box::pin(async { Ok(vec![]) })
            }
            fn as_any(&self) -> &dyn std::any::Any {
                self
            }
        }

        let registry = CapabilityRegistry::new();
        registry.register_provider(Unhealthy { hang: false });
        registry.register_provider(Unhealthy { hang: true });
        registry.register_provider(susi_gemi::candle_provider::CandleProvider);
        tokio::time::timeout(Duration::from_secs(7), prune_unhealthy_providers(&registry))
            .await
            .expect("discovery must recover from a hanging provider");
        assert!(registry.get_provider("ollama-dead").is_none());
        assert!(registry.get_provider("ollama-hung").is_none());
        assert!(registry.get_provider("Candle (Local)").is_some());
    }
}
