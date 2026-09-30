//! Azure OpenAI provider with deployments.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AzureDeployment {
    pub name: String,
    pub model: String,
    pub endpoint: String,
}

/// Build a deployment URL for Azure OpenAI chat completions.
#[must_use]
pub fn azure_chat_url(endpoint: &str, deployment: &str, api_version: &str) -> String {
    format!(
        "{}/openai/deployments/{deployment}/chat/completions?api-version={api_version}",
        endpoint.trim_end_matches('/')
    )
}

#[cfg(test)]
mod azure_openai_provider_tests {
    use super::*;

    #[test]
    fn azure_openai_provider_builds_deployment_url() {
        let u = azure_chat_url("https://ex.openai.azure.com", "gpt4", "2024-02-01");
        assert!(u.contains("/deployments/gpt4/chat/completions"));
        assert!(u.contains("api-version=2024-02-01"));
    }
}
