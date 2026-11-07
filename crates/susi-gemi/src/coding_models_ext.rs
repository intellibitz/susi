//! Engines-side coding-model helpers that need `HttpProvider`.
//!
//! Catalog / preflight / configure live in [`susi_gemi_models::coding_models`].
//! Paid probe and provider construction stay here so models never depends on engines.

use crate::susi_core::provider::Provider;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};
use susi_gemi_models::coding_models::CodingModelManager;

use crate::engines::http_provider::{HttpProvider, InferenceProtocol};

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
    let _permit = crate::key_arbitration::try_acquire(provider.name())
        .map_err(|d| susi_error::eai_err!(format!("{}: {}", provider.name(), d.describe())))?;
    let text = runtime
        .block_on(provider.generate(prompt))
        .map_err(|e| susi_error::eai_err!(e.to_string()))?;
    Ok(crate::susi_config::redact_credentials(&text))
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn probe_rejects_empty_prompt_immediately() {
        let err = probe("any-model-id", "").unwrap_err();
        assert!(err.to_string().contains("probe prompt must not be empty"));
    }

    #[test]
    fn probe_rejects_whitespace_only_prompt_immediately() {
        let err = probe("any-model-id", "   \n\t  ").unwrap_err();
        assert!(err.to_string().contains("probe prompt must not be empty"));
    }

    #[test]
    fn provider_returns_err_for_unknown_coding_model_id() {
        let manager = CodingModelManager::new().unwrap();
        let res = provider(&manager, "nonexistent-coding-model-xyz-999");
        assert!(res.is_err());
    }

    #[test]
    fn inference_protocol_mapping_matches_coding_provider_expectations() {
        assert_eq!(
            InferenceProtocol::from_config("openai"),
            InferenceProtocol::OpenAiChat
        );
        assert_ne!(
            InferenceProtocol::from_config("anthropic"),
            InferenceProtocol::from_config("openai")
        );
    }

    #[test]
    fn provider_builds_http_provider_for_catalog_coding_model() {
        let manager = CodingModelManager::new().unwrap();
        let catalog = CodingModelManager::catalog().unwrap();
        assert!(!catalog.is_empty());
        let first = &catalog[0];
        match provider(&manager, &first.id) {
            Ok(built) => assert_eq!(built.name(), format!("coding-{}", first.id)),
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
