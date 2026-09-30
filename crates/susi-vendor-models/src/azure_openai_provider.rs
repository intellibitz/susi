//! Azure OpenAI provider (T-CLAUDE-30): per-deployment URLs, `api-version`
//! query params, `api-key` auth header, deployment discovery and health.

use susi_error::{EaiError, EaiResult};

/// A configured Azure OpenAI resource.
#[derive(Debug, Clone, PartialEq)]
pub struct AzureConfig {
    /// Resource name (`https://{resource}.openai.azure.com`). Accepts a bare
    /// name or a full endpoint URL; stored normalised to a bare name.
    pub resource: String,
    /// API version string, e.g. `2024-10-21`.
    pub api_version: String,
    /// `api-key` credential.
    pub api_key: String,
}

impl AzureConfig {
    pub fn new(resource: &str, api_version: &str, api_key: &str) -> EaiResult<Self> {
        let name = resource
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(EaiError::config(format!(
                "invalid azure resource name `{resource}`"
            )));
        }
        if api_version.is_empty() {
            return Err(EaiError::config("api-version is required"));
        }
        Ok(Self {
            resource: name.into(),
            api_version: api_version.into(),
            api_key: api_key.into(),
        })
    }

    pub fn base(&self) -> String {
        format!("https://{}.openai.azure.com", self.resource)
    }

    /// Chat completions URL for a deployment.
    pub fn chat_url(&self, deployment: &str) -> String {
        format!(
            "{}/openai/deployments/{}/chat/completions?api-version={}",
            self.base(),
            deployment,
            self.api_version
        )
    }

    /// Embeddings URL for a deployment.
    pub fn embeddings_url(&self, deployment: &str) -> String {
        format!(
            "{}/openai/deployments/{}/embeddings?api-version={}",
            self.base(),
            deployment,
            self.api_version
        )
    }

    /// Deployment listing — doubles as the health probe (a 401 means the key
    /// is wrong; 404 means the resource name is wrong).
    pub fn deployments_url(&self) -> String {
        format!(
            "{}/openai/deployments?api-version={}",
            self.base(),
            self.api_version
        )
    }

    /// Model-listing URL (catalog of deployable models).
    pub fn models_url(&self) -> String {
        format!(
            "{}/openai/models?api-version={}",
            self.base(),
            self.api_version
        )
    }

    /// Auth header (`api-key`, NOT `Authorization: Bearer` — that's Entra ID).
    pub fn auth_headers(&self) -> Vec<(&'static str, String)> {
        vec![("api-key", self.api_key.clone())]
    }

    /// Classify a deployments-list health probe.
    pub fn health(status: u16) -> &'static str {
        match status {
            200..=299 => "ready",
            401 | 403 => "key rejected — check api-key",
            404 => "resource not found — check the resource name",
            _ => "unreachable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> AzureConfig {
        AzureConfig::new("my-res", "2024-10-21", "k").unwrap()
    }

    #[test]
    fn azure_openai_provider_normalises_resource_names() {
        assert_eq!(cfg().resource, "my-res");
        assert_eq!(
            AzureConfig::new("https://my-res.openai.azure.com/", "v", "k")
                .unwrap()
                .resource,
            "my-res"
        );
        assert!(AzureConfig::new("bad name!", "v", "k").is_err());
        assert!(AzureConfig::new("", "v", "k").is_err());
    }

    #[test]
    fn azure_openai_provider_per_deployment_urls() {
        let c = cfg();
        assert_eq!(
            c.chat_url("gpt4o-prod"),
            "https://my-res.openai.azure.com/openai/deployments/gpt4o-prod/chat/completions?api-version=2024-10-21"
        );
        assert!(c
            .embeddings_url("ada")
            .contains("/deployments/ada/embeddings"));
    }

    #[test]
    fn azure_openai_provider_auth_header_is_api_key() {
        let h = cfg().auth_headers();
        assert_eq!(h, vec![("api-key", "k".to_string())]);
    }

    #[test]
    fn azure_openai_provider_discovery_and_health() {
        let c = cfg();
        assert!(c
            .deployments_url()
            .contains("/openai/deployments?api-version="));
        assert!(c.models_url().contains("/openai/models?api-version="));
        assert_eq!(AzureConfig::health(200), "ready");
        assert!(AzureConfig::health(401).contains("key"));
        assert!(AzureConfig::health(404).contains("resource"));
    }
}
