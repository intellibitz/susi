//! Engines-side frontier model helpers (prefer routing, paid/local probe).
#[cfg(test)]
use crate::susi_core::provider::Provider;
use susi_error::EaiResult as Result;
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
    crate::model_probe::run(prompt, "tokio runtime for frontier probe", || {
        provider(&FrontierManager::new()?, id)
    })
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn probe_rejects_empty_prompt_immediately() {
        let err = probe("any-frontier-id", "").unwrap_err();
        assert!(err.to_string().contains("probe prompt must not be empty"));
    }

    #[test]
    fn probe_rejects_whitespace_only_prompt_immediately() {
        let err = probe("any-frontier-id", "  \t\n ").unwrap_err();
        assert!(err.to_string().contains("probe prompt must not be empty"));
    }

    #[test]
    fn provider_returns_err_for_unknown_frontier_model_id() {
        let manager = FrontierManager::new().unwrap();
        let res = provider(&manager, "nonexistent-frontier-model-xyz-999");
        assert!(res.is_err());
    }

    #[test]
    fn prefer_returns_err_for_unknown_frontier_model_when_not_clearing() {
        let res = prefer("nonexistent-frontier-model-xyz-999", false);
        assert!(res.is_err());
    }

    #[test]
    fn provider_builds_http_provider_for_catalog_frontier_model() {
        let manager = FrontierManager::new().unwrap();
        let catalog = FrontierManager::catalog().unwrap();
        assert!(!catalog.is_empty());
        let first = &catalog[0];
        match provider(&manager, &first.id) {
            Ok(built) => assert_eq!(built.name(), format!("frontier-{}", first.id)),
            Err(err) => {
                let msg = err.to_string();
                assert!(
                    msg.contains("API_KEY") || msg.contains("susi keys set"),
                    "unexpected error: {msg}"
                );
            }
        }
    }
}
