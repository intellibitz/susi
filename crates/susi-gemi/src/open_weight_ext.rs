//! Engines-side open-weight helpers (prefer local routing, live probe).
use crate::susi_core::provider::Provider;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};
use susi_gemi_models::open_weight::OpenWeightManager;

use crate::engines::http_provider::{HttpProvider, InferenceProtocol};
use crate::engines::routing::InferenceRouter;

/// Build an [`HttpProvider`] for a curated open-weight model (Ollama/vLLM).
pub fn provider(manager: &OpenWeightManager, id: &str) -> Result<HttpProvider> {
    let (def, api_base, protocol) = manager.resolve_endpoint(id)?;
    Ok(HttpProvider {
        name: format!("open-weight-{}", def.id),
        api_base,
        model: def.ollama_tag.clone(),
        protocol: InferenceProtocol::from_config(&protocol),
        api_key: String::new(),
    })
}

/// Prefer an open-weight model and pin Ollama (or the model's engine) for routing.
pub fn prefer(model: &str, clear: bool) -> Result<String> {
    let manager = OpenWeightManager::new()?;
    if clear {
        let cleared = manager.clear_preferred()?;
        let _ = InferenceRouter::clear_preferred_cloud();
        return Ok(cleared);
    }
    let pin = manager.prefer(model)?;
    let def = manager.effective(model)?;
    let cloud =
        InferenceRouter::set_preferred_cloud(&def.engine).map_err(|e| susi_error::eai_err!(e))?;
    Ok(format!("{pin} | {cloud}"))
}

/// Local probe against the effective Ollama/vLLM endpoint (no cloud key required).
pub fn probe(id: &str, prompt: &str) -> Result<String> {
    if prompt.trim().is_empty() {
        bail!("probe prompt must not be empty");
    }
    let manager = OpenWeightManager::new()?;
    let provider = provider(&manager, id)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime for open-weight probe")?;
    let text = runtime
        .block_on(provider.generate(prompt))
        .map_err(|e| susi_error::eai_err!(e.to_string()))?;
    Ok(crate::susi_config::redact_credentials(&text))
}
