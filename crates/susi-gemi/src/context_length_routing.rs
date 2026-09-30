//! Context-window aware routing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelWindow {
    pub name: String,
    pub context_tokens: u64,
}

/// Pick the smallest model whose context window fits the prompt.
#[must_use]
pub fn route_by_context(needed_tokens: u64, models: &[ModelWindow]) -> Option<String> {
    models
        .iter()
        .filter(|m| m.context_tokens >= needed_tokens)
        .min_by_key(|m| m.context_tokens)
        .map(|m| m.name.clone())
}

#[cfg(test)]
mod context_length_routing_tests {
    use super::*;

    #[test]
    fn context_length_routing_picks_smallest_fit() {
        let models = [
            ModelWindow {
                name: "small".into(),
                context_tokens: 8_000,
            },
            ModelWindow {
                name: "mid".into(),
                context_tokens: 32_000,
            },
            ModelWindow {
                name: "large".into(),
                context_tokens: 128_000,
            },
        ];
        assert_eq!(route_by_context(10_000, &models).as_deref(), Some("mid"));
        assert_eq!(route_by_context(200_000, &models), None);
    }
}
