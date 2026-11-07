//! Engines-side coding-model helpers that need `HttpProvider`.
//!
//! Catalog / preflight / configure live in [`susi_gemi_models::coding_models`].
//! Paid probe and provider construction stay here so models never depends on engines.

use crate::susi_core::provider::Provider;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};
use susi_gemi_models::coding_models::CodingModelManager;

use crate::engines::http_provider::{HttpProvider, InferenceProtocol};

/// Specification for a coding-model paid probe request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodingProbeSpec {
    pub id: String,
    pub prompt: String,
}

impl CodingProbeSpec {
    /// Verify structural invariants on a coding-model probe specification.
    pub fn verify_probe_spec_invariants(&self) -> std::result::Result<(), String> {
        if self.id.trim().is_empty() {
            return Err("CodingProbeSpec id must not be empty".to_string());
        }
        if self.prompt.trim().is_empty() {
            return Err("CodingProbeSpec prompt must not be empty".to_string());
        }
        Ok(())
    }
}

/// Verify structural invariants on an [`HttpProvider`] constructed for a curated coding model.
pub fn verify_coding_provider_invariants(
    provider: &HttpProvider,
) -> std::result::Result<(), String> {
    if !provider.name.starts_with("coding-") || provider.name["coding-".len()..].trim().is_empty() {
        return Err(format!(
            "coding HttpProvider name ('{}') must start with non-empty 'coding-' prefix",
            provider.name
        ));
    }
    let base = provider.api_base.trim();
    if base.is_empty() || (!base.starts_with("http://") && !base.starts_with("https://")) {
        return Err(format!(
            "coding HttpProvider api_base ('{}') must be a non-empty http(s) URL",
            provider.api_base
        ));
    }
    if provider.model.trim().is_empty() {
        return Err("coding HttpProvider model must not be empty".to_string());
    }
    if provider.api_key.trim().is_empty() {
        return Err("coding HttpProvider api_key must not be empty".to_string());
    }
    Ok(())
}

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

    #[test]
    fn verify_gemi_wave14_invariants_coding_models_ext() {
        let valid_spec = CodingProbeSpec {
            id: "deepseek-chat".to_string(),
            prompt: "reply ok".to_string(),
        };
        assert!(valid_spec.verify_probe_spec_invariants().is_ok());

        let blank_id = CodingProbeSpec {
            id: "  ".to_string(),
            prompt: "reply ok".to_string(),
        };
        assert!(blank_id.verify_probe_spec_invariants().is_err());

        let blank_prompt = CodingProbeSpec {
            id: "deepseek-chat".to_string(),
            prompt: String::new(),
        };
        assert!(blank_prompt.verify_probe_spec_invariants().is_err());

        let valid_provider = HttpProvider {
            name: "coding-deepseek".to_string(),
            api_base: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-coder".to_string(),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: "sk-test-key".to_string(),
        };
        assert!(verify_coding_provider_invariants(&valid_provider).is_ok());

        let bad_prefix = HttpProvider {
            name: "deepseek".to_string(),
            api_base: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-coder".to_string(),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: "sk-test-key".to_string(),
        };
        assert!(verify_coding_provider_invariants(&bad_prefix).is_err());

        let bad_base = HttpProvider {
            name: "coding-deepseek".to_string(),
            api_base: "ftp://api.deepseek.com".to_string(),
            model: "deepseek-coder".to_string(),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: "sk-test-key".to_string(),
        };
        assert!(verify_coding_provider_invariants(&bad_base).is_err());

        let blank_key = HttpProvider {
            name: "coding-deepseek".to_string(),
            api_base: "https://api.deepseek.com/v1".to_string(),
            model: "deepseek-coder".to_string(),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: "   ".to_string(),
        };
        assert!(verify_coding_provider_invariants(&blank_key).is_err());
    }
}
