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
    let _permit = crate::key_arbitration::try_acquire(provider.name())
        .map_err(|d| susi_error::eai_err!(format!("{}: {}", provider.name(), d.describe())))?;
    let text = runtime
        .block_on(provider.generate(prompt))
        .map_err(|e| susi_error::eai_err!(e.to_string()))?;
    Ok(crate::susi_config::redact_credentials(&text))
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;

    #[test]
    fn probe_rejects_empty_and_whitespace_only_prompts_before_network() {
        let empty_err = probe("qwen", "").expect_err("empty prompt must fail");
        assert!(empty_err
            .to_string()
            .contains("probe prompt must not be empty"));

        let ws_err = probe("qwen", "  \t\n ").expect_err("whitespace prompt must fail");
        assert!(ws_err
            .to_string()
            .contains("probe prompt must not be empty"));
    }

    #[test]
    fn provider_rejects_unknown_open_weight_model_identifier() {
        let manager = OpenWeightManager::new().expect("open-weight manager");
        let res = provider(&manager, "nonexistent-open-weight-model-xyz");
        assert!(res.is_err());
    }

    #[test]
    fn provider_builds_http_provider_for_catalog_open_weight_model() {
        let manager = OpenWeightManager::new().expect("open-weight manager");
        let catalog = OpenWeightManager::catalog().expect("open-weight catalog");
        assert!(!catalog.is_empty());
        let first = &catalog[0];
        let built = provider(&manager, &first.id).expect("provider for catalog model");
        assert_eq!(built.name, format!("open-weight-{}", first.id));
        assert_eq!(built.model, first.ollama_tag);
        assert!(built.api_key.is_empty());
    }

    #[test]
    fn open_weight_catalog_entries_have_non_empty_ids_and_ollama_tags() {
        let catalog = OpenWeightManager::catalog().expect("open-weight catalog");
        assert!(!catalog.is_empty());
        for entry in catalog {
            assert!(!entry.id.trim().is_empty());
            assert!(!entry.ollama_tag.trim().is_empty());
            assert!(!entry.engine.trim().is_empty());
        }
    }

    #[test]
    fn prefer_returns_err_for_unknown_open_weight_model_when_not_clearing() {
        let res = prefer("nonexistent-open-weight-model-xyz-999", false);
        assert!(res.is_err());
    }
}
