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

#[test]
fn zc_config_all_optional_default_settings_count_matches_registry() {
    let cfg = SusiConfig::default();
    assert_eq!(cfg.settings.len(), SETTING_REGISTRY.len());
}

#[test]
fn zc_config_all_optional_every_registry_default_value_is_non_empty() {
    let cfg = SusiConfig::default();
    for entry in SETTING_REGISTRY {
        let value = cfg
            .settings
            .get(entry.key)
            .expect("registry key must exist in SusiConfig::default()");
        assert!(
            !value.is_null(),
            "bundled default for {} must not be null",
            entry.key
        );
    }
}

#[test]
fn zc_config_all_optional_heal_defaults_are_registered_in_setting_registry() {
    let defaults = SusiConfig::heal_defaults();
    assert!(!defaults.is_empty());
    assert!(defaults.len() <= SETTING_REGISTRY.len());
    for key in defaults.keys() {
        assert!(
            SETTING_REGISTRY.iter().any(|entry| entry.key == key),
            "heal_defaults key {key} must be registered in SETTING_REGISTRY"
        );
    }
}
