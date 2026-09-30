//! Discover credentials where they already live, with consent.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeySource {
    pub provider: String,
    pub path: String,
    pub needs_consent: bool,
}

/// Propose discovery locations for cloud keys (never read without consent).
#[must_use]
pub fn discover_key_sources(home: &str) -> Vec<KeySource> {
    vec![
        KeySource {
            provider: "openai".into(),
            path: format!("{home}/.config/openai/api_key"),
            needs_consent: true,
        },
        KeySource {
            provider: "anthropic".into(),
            path: format!("{home}/.config/anthropic/api_key"),
            needs_consent: true,
        },
        KeySource {
            provider: "huggingface".into(),
            path: format!("{home}/.cache/huggingface/token"),
            needs_consent: true,
        },
    ]
}

#[cfg(test)]
mod zc_key_sources_tests {
    use super::*;

    #[test]
    fn zc_key_sources_require_consent() {
        let s = discover_key_sources("/home/u");
        assert!(s.len() >= 3);
        assert!(s.iter().all(|k| k.needs_consent));
        assert!(s.iter().any(|k| k.provider == "huggingface"));
    }
}
