//! Safe default privacy posture with one first-run consent.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyPreset {
    LocalOnly,
    AllowlistedClouds,
    Open,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrivacyState {
    pub preset: PrivacyPreset,
    pub consented: bool,
}

impl Default for PrivacyState {
    fn default() -> Self {
        Self {
            preset: PrivacyPreset::LocalOnly,
            consented: false,
        }
    }
}

impl PrivacyState {
    pub fn consent_once(&mut self, preset: PrivacyPreset) {
        self.preset = preset;
        self.consented = true;
    }

    #[must_use]
    pub fn allows_cloud(&self) -> bool {
        self.consented && !matches!(self.preset, PrivacyPreset::LocalOnly)
    }

    #[must_use]
    pub fn needs_prompt(&self) -> bool {
        !self.consented
    }
}

#[cfg(test)]
mod zc_privacy_default_tests {
    use super::*;

    #[test]
    fn zc_privacy_default_local_until_one_consent() {
        let mut s = PrivacyState::default();
        assert!(s.needs_prompt());
        assert!(!s.allows_cloud());
        s.consent_once(PrivacyPreset::AllowlistedClouds);
        assert!(!s.needs_prompt());
        assert!(s.allows_cloud());
    }
}
