//! Offer to pull a recommended model into an idle local engine.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutopullOffer {
    pub engine: String,
    pub model: String,
}

#[must_use]
pub fn offer_autopull(
    engine_idle: bool,
    recommended: Option<&str>,
    engine: &str,
) -> Option<AutopullOffer> {
    if !engine_idle {
        return None;
    }
    let model = recommended?;
    Some(AutopullOffer {
        engine: engine.to_string(),
        model: model.to_string(),
    })
}

#[cfg(test)]
mod zc_engine_autopull_tests {
    use super::*;

    #[test]
    fn zc_engine_autopull_offers_when_idle() {
        assert!(offer_autopull(false, Some("m"), "ollama").is_none());
        let o = offer_autopull(true, Some("llama"), "ollama").unwrap();
        assert_eq!(o.model, "llama");
    }
}
