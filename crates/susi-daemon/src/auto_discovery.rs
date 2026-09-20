use susi_core::registry::CapabilityRegistry;

/// Universal Autonomous Substrate Bootstrapper
/// Implements Pillar 8 (Autonomous Provisioning) by triggering zero-config
/// capability discovery across the local system.
pub async fn bootstrap_zero_config_substrate() {
    let registry = CapabilityRegistry::global();

    if std::env::var("SUSI_VERBOSE").is_ok() {
        eprintln!("[BOOTSTRAP] Initiating Zero-Config Substrate Discovery...");
    }

    // 1. Probe for Local Model Inference Engines (Ollama, vLLM, llama.cpp, etc.)
    susi_gemi::http_provider::auto_discover_local_engines(registry).await;

    // 2. Discover and Provision MCP Tools
    susi_gmcp::mcp_wrapper::auto_discover_mcp(registry);

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
