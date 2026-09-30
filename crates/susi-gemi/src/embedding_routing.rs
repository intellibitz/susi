//! Embedding provider abstraction and routing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingProvider {
    pub name: String,
    pub dim: u32,
    pub local: bool,
}

/// Prefer local embedding providers when available.
#[must_use]
pub fn route_embedding(providers: &[EmbeddingProvider], prefer_local: bool) -> Option<String> {
    if prefer_local {
        if let Some(p) = providers.iter().find(|p| p.local) {
            return Some(p.name.clone());
        }
    }
    providers.first().map(|p| p.name.clone())
}

#[cfg(test)]
mod embedding_routing_tests {
    use super::*;

    #[test]
    fn embedding_routing_prefers_local() {
        let providers = [
            EmbeddingProvider {
                name: "openai".into(),
                dim: 1536,
                local: false,
            },
            EmbeddingProvider {
                name: "fastembed".into(),
                dim: 384,
                local: true,
            },
        ];
        assert_eq!(
            route_embedding(&providers, true).as_deref(),
            Some("fastembed")
        );
        assert_eq!(
            route_embedding(&providers, false).as_deref(),
            Some("openai")
        );
    }
}
