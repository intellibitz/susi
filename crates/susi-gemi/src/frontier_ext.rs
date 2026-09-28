//! Engines-side frontier model helpers (prefer routing, paid/local probe).
use crate::susi_core::provider::Provider;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};
use susi_gemi_models::frontier::FrontierManager;

use crate::engines::http_provider::{HttpProvider, InferenceProtocol};
use crate::engines::routing::InferenceRouter;

/// Build an [`HttpProvider`] for a curated frontier model.
pub fn provider(manager: &FrontierManager, id: &str) -> Result<HttpProvider> {
    let ep = manager.resolve_endpoint(id)?;
    Ok(HttpProvider {
        name: format!("frontier-{}", ep.def.id),
        api_base: ep.api_base,
        model: ep.def.model.clone(),
        protocol: InferenceProtocol::from_config(&ep.protocol_type),
        api_key: ep.api_key,
    })
}

/// Prefer a frontier model and pin its engine for cloud/local routing.
pub fn prefer(model: &str, clear: bool) -> Result<String> {
    let manager = FrontierManager::new()?;
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

/// Probe — short completion via cloud or local endpoint.
pub fn probe(id: &str, prompt: &str) -> Result<String> {
    if prompt.trim().is_empty() {
        bail!("probe prompt must not be empty");
    }
    let manager = FrontierManager::new()?;
    let provider = provider(&manager, id)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime for frontier probe")?;
    let text = runtime
        .block_on(provider.generate(prompt))
        .map_err(|e| susi_error::eai_err!(e.to_string()))?;
    Ok(crate::susi_config::redact_credentials(&text))
}
