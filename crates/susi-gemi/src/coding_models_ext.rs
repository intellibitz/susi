//! Engines-side coding-model helpers that need `HttpProvider`.
//!
//! Catalog / preflight / configure live in [`susi_gemi_models::coding_models`].
//! Paid probe and provider construction stay here so models never depends on engines.

use anyhow::{bail, Context, Result};
use susi_core::provider::Provider;
use susi_gemi_models::coding_models::CodingModelManager;

use crate::http_provider::{HttpProvider, InferenceProtocol};

/// Build an [`HttpProvider`] for a curated coding/agent model.
pub fn provider(manager: &CodingModelManager, id: &str) -> Result<HttpProvider> {
    let ep = manager.resolve_endpoint(id)?;
    Ok(HttpProvider {
        name: format!("coding-{}", ep.def.id),
        api_base: ep.api_base,
        model: ep.def.model.clone(),
        protocol: InferenceProtocol::from_config(&ep.protocol_type),
        api_key: ep.api_key,
    })
}

/// Paid probe — short completion to verify auth/model id. Output is evidence, not Truth.
pub fn probe(id: &str, prompt: &str) -> Result<String> {
    if prompt.trim().is_empty() {
        bail!("probe prompt must not be empty");
    }
    let manager = CodingModelManager::new()?;
    let provider = provider(&manager, id)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime for model probe")?;
    let text = runtime
        .block_on(provider.generate(prompt))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(redact_secrets(&text))
}

fn redact_secrets(text: &str) -> String {
    let mut result = text.to_owned();
    for (key, value) in std::env::vars() {
        if value.len() >= 8
            && (key.ends_with("_API_KEY") || key.ends_with("_TOKEN") || key.ends_with("_SECRET"))
        {
            result = result.replace(&value, "[REDACTED]");
        }
    }
    susi_core::redact::redact_patterns(
        &["sk-".into(), "ghp_".into(), "github_pat_".into()],
        &result,
    )
}
