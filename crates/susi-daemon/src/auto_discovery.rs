use std::time::Duration;

use susi_core::registry::CapabilityRegistry;

/// Universal Autonomous Substrate Bootstrapper
/// Implements Pillar 8 (Autonomous Provisioning) by triggering zero-config
/// capability discovery across the local system.
pub async fn bootstrap_zero_config_substrate() {
    let registry = CapabilityRegistry::global();

    if std::env::var("SUSI_VERBOSE").is_ok() {
        eprintln!("[BOOTSTRAP] Initiating Zero-Config Substrate Discovery...");
    }

    // Drop HTTP providers that went unhealthy since the last pass so a killed
    // Ollama/vLLM does not keep winning routing. Candle is permanent fallback.
    prune_unhealthy_providers(registry).await;

    // 1. Probe for Local Model Inference Engines (Ollama, vLLM, llama.cpp, etc.)
    susi_gemi::http_provider::auto_discover_local_engines(registry).await;

    // 1b. Register configured cloud vendors (OpenAI / Anthropic / Gemini) when
    // their API keys are present — same CapabilityRegistry path as local discovery.
    susi_gemi::http_provider::register_configured_cloud_endpoints(registry);

    // 2. Discover and Provision MCP Tools
    susi_gmcp::mcp_wrapper::auto_discover_mcp(registry);

    // 2b. MCP servers that expose chat/LLM tools → inference Providers so they
    // join cloud prefer / slow-local escalation (not tools-only).
    susi_gemi::mcp_provider::register_mcp_inference_providers(registry);

    // 3. Fallback: Ensure Candle (Local Edge) is always provisioned
    if registry.get_provider("Candle (Local)").is_none() {
        registry.register_provider(susi_gemi::candle_provider::CandleProvider);
    }

    if std::env::var("SUSI_VERBOSE").is_ok() {
        eprintln!("[BOOTSTRAP] Capability Registry Loaded:");
        eprintln!(" - Providers: {:?}", registry.list_providers());
        eprintln!(" - Tools: {:?}", registry.list_tools());
    }
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

    #[tokio::test]
    async fn prune_removes_unhealthy_non_candle_providers() {
        use susi_core::provider::{BoxFuture, Provider};
        use susi_error::EaiResult;

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
