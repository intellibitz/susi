//! DeepSeek default model and no-credit key state.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyState {
    Missing,
    Present,
    NoCredit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepSeekDefault {
    pub model: String,
    pub key_state: KeyState,
    pub served: bool,
}

/// Settle the DeepSeek default and whether it can be served.
#[must_use]
pub fn deepseek_default(key_state: KeyState) -> DeepSeekDefault {
    let model = "deepseek-chat".to_string();
    let served = matches!(key_state, KeyState::Present);
    DeepSeekDefault {
        model,
        key_state,
        served,
    }
}

#[cfg(test)]
mod deepseek_default_is_served_tests {
    use super::*;

    #[test]
    fn deepseek_default_is_served_only_with_credit() {
        let ok = deepseek_default(KeyState::Present);
        assert!(ok.served);
        assert_eq!(ok.model, "deepseek-chat");
        assert!(!deepseek_default(KeyState::NoCredit).served);
        assert!(!deepseek_default(KeyState::Missing).served);
    }
}
