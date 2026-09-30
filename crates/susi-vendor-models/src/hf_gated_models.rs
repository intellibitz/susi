//! Hugging Face token and gated-model handling.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum GatedAccess {
    Open,
    NeedsToken,
    Denied,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatedDecision {
    pub access: GatedAccess,
    pub prompt: Option<String>,
}

/// Decide access for a gated repo given token presence and server status.
#[must_use]
pub fn gated_access(is_gated: bool, has_token: bool, authorized: bool) -> GatedDecision {
    if !is_gated {
        return GatedDecision {
            access: GatedAccess::Open,
            prompt: None,
        };
    }
    if !has_token {
        return GatedDecision {
            access: GatedAccess::NeedsToken,
            prompt: Some("HF_TOKEN required for gated model; set once with consent".into()),
        };
    }
    if authorized {
        GatedDecision {
            access: GatedAccess::Open,
            prompt: None,
        }
    } else {
        GatedDecision {
            access: GatedAccess::Denied,
            prompt: Some("token present but not authorized for this gated repo".into()),
        }
    }
}

#[cfg(test)]
mod hf_gated_models_tests {
    use super::*;

    #[test]
    fn hf_gated_models_prompt_when_token_missing() {
        let d = gated_access(true, false, false);
        assert_eq!(d.access, GatedAccess::NeedsToken);
        assert!(d.prompt.is_some());
        assert_eq!(gated_access(false, false, false).access, GatedAccess::Open);
        assert_eq!(gated_access(true, true, true).access, GatedAccess::Open);
    }
}
