//! Google Vertex AI provider with service-account auth.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VertexEndpoint {
    pub project: String,
    pub location: String,
    pub model: String,
    pub auth: String,
}

/// Build a Vertex publish URL descriptor using service-account auth.
#[must_use]
pub fn vertex_endpoint(project: &str, location: &str, model: &str) -> VertexEndpoint {
    VertexEndpoint {
        project: project.into(),
        location: location.into(),
        model: model.into(),
        auth: "service_account".into(),
    }
}

#[must_use]
pub fn vertex_url(ep: &VertexEndpoint) -> String {
    format!(
        "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:generateContent",
        ep.location, ep.project, ep.location, ep.model
    )
}

#[cfg(test)]
mod vertex_provider_tests {
    use super::*;

    #[test]
    fn vertex_provider_service_account_endpoint() {
        let ep = vertex_endpoint("p", "us-central1", "gemini-1.5-pro");
        assert_eq!(ep.auth, "service_account");
        assert!(vertex_url(&ep).contains("aiplatform.googleapis.com"));
    }
}
