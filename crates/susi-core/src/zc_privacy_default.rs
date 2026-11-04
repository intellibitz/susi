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

    #[test]
    fn consent_once_with_local_only_clears_prompt_but_still_disallows_cloud() {
        let mut s = PrivacyState::default();
        s.consent_once(PrivacyPreset::LocalOnly);
        assert!(!s.needs_prompt());
        assert!(!s.allows_cloud());
    }

    #[test]
    fn consent_once_with_open_preset_allows_cloud_and_clears_prompt() {
        let mut s = PrivacyState::default();
        s.consent_once(PrivacyPreset::Open);
        assert!(!s.needs_prompt());
        assert!(s.allows_cloud());
        assert_eq!(s.preset, PrivacyPreset::Open);
    }

    #[test]
    fn privacy_state_and_preset_serialize_as_snake_case_json() {
        let s = PrivacyState {
            preset: PrivacyPreset::AllowlistedClouds,
            consented: true,
        };
        let json = serde_json::to_string(&s).expect("serialize");
        assert!(json.contains("\"allowlisted_clouds\""), "{json}");
        let back: PrivacyState = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, s);
    }

    #[test]
    fn unconsented_open_preset_still_blocks_cloud_and_requires_prompt() {
        let mut s = PrivacyState {
            preset: PrivacyPreset::Open,
            consented: false,
        };
        assert!(s.needs_prompt());
        assert!(!s.allows_cloud());
        s.consent_once(PrivacyPreset::Open);
        assert!(!s.needs_prompt());
        assert!(s.allows_cloud());
    }
}
