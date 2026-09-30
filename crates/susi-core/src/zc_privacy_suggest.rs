//! Suggest a privacy preset from the environment.

use crate::zc_privacy_default::PrivacyPreset;

#[must_use]
pub fn suggest_privacy_preset(has_corp_proxy: bool, airgapped: bool) -> PrivacyPreset {
    if airgapped {
        PrivacyPreset::LocalOnly
    } else if has_corp_proxy {
        PrivacyPreset::AllowlistedClouds
    } else {
        PrivacyPreset::Open
    }
}

#[cfg(test)]
mod zc_privacy_suggest_tests {
    use super::*;

    #[test]
    fn zc_privacy_suggest_from_environment() {
        assert_eq!(
            suggest_privacy_preset(false, true),
            PrivacyPreset::LocalOnly
        );
        assert_eq!(
            suggest_privacy_preset(true, false),
            PrivacyPreset::AllowlistedClouds
        );
        assert_eq!(suggest_privacy_preset(false, false), PrivacyPreset::Open);
    }
}
