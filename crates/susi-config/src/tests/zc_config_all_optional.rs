use crate::setting_registry::{SettingDerivation, SETTING_REGISTRY};
use crate::SusiConfig;

#[test]
fn zc_config_all_optional_empty_home_loads_defaults() {
    // Empty settings overlay must not be required — defaults fill every key.
    let cfg = SusiConfig::default();
    assert!(!cfg.settings.is_empty());
    for entry in SETTING_REGISTRY {
        assert!(
            cfg.settings.contains_key(entry.key),
            "missing default for {}",
            entry.key
        );
        // Consent is optional (JIT), never a required pre-set file.
        match entry.derivation {
            SettingDerivation::Constant
            | SettingDerivation::Hardware
            | SettingDerivation::Detection
            | SettingDerivation::Measurement
            | SettingDerivation::Consent => {}
        }
    }
}

#[test]
fn zc_config_all_optional_no_required_user_keys() {
    // Registry deliberately has no UserMustSet — every key is derived/defaulted.
    assert!(SETTING_REGISTRY
        .iter()
        .all(|e| !matches!(format!("{:?}", e.derivation).as_str(), "UserMustSet")));
}
