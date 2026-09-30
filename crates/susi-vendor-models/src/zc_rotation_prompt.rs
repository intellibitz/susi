//! Detect expired/revoked keys and guide rotation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeySignal {
    Valid,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotationGuide {
    pub vendor: String,
    pub message: String,
}

#[must_use]
pub fn rotation_prompt(vendor: &str, signal: KeySignal) -> Option<RotationGuide> {
    match signal {
        KeySignal::Valid => None,
        KeySignal::Expired => Some(RotationGuide {
            vendor: vendor.into(),
            message: format!("replace expired {vendor} key (susi keys add)"),
        }),
        KeySignal::Revoked => Some(RotationGuide {
            vendor: vendor.into(),
            message: format!("replace revoked {vendor} key (susi keys add)"),
        }),
    }
}

#[cfg(test)]
mod zc_rotation_prompt_tests {
    use super::*;

    #[test]
    fn zc_rotation_prompt_guides_expired_or_revoked() {
        assert!(rotation_prompt("openai", KeySignal::Valid).is_none());
        assert!(rotation_prompt("openai", KeySignal::Expired)
            .unwrap()
            .message
            .contains("expired"));
        assert!(rotation_prompt("anthropic", KeySignal::Revoked)
            .unwrap()
            .message
            .contains("revoked"));
    }
}
