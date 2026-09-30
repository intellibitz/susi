//! Ask for HF token only when a gated model is required.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HfAccess {
    pub model: String,
    pub gated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HfTokenPrompt {
    NotNeeded,
    AskOnce,
}

#[must_use]
pub fn hf_token_prompt(access: &HfAccess, already_have_token: bool) -> HfTokenPrompt {
    if !access.gated || already_have_token {
        HfTokenPrompt::NotNeeded
    } else {
        HfTokenPrompt::AskOnce
    }
}

#[cfg(test)]
mod zc_hf_token_jit_tests {
    use super::*;

    #[test]
    fn zc_hf_token_jit_asks_only_for_gated() {
        assert_eq!(
            hf_token_prompt(
                &HfAccess {
                    model: "open".into(),
                    gated: false
                },
                false
            ),
            HfTokenPrompt::NotNeeded
        );
        assert_eq!(
            hf_token_prompt(
                &HfAccess {
                    model: "gated".into(),
                    gated: true
                },
                false
            ),
            HfTokenPrompt::AskOnce
        );
        assert_eq!(
            hf_token_prompt(
                &HfAccess {
                    model: "gated".into(),
                    gated: true
                },
                true
            ),
            HfTokenPrompt::NotNeeded
        );
    }
}
