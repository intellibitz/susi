//! Acceptance: every bundled config key is registered; none is "user must set".

use crate::setting_registry::{SettingDerivation, SETTING_REGISTRY};
use crate::SusiConfig;

#[test]
fn zc_setting_registry_covers_every_bundled_key() {
    let cfg = SusiConfig::default();
    let registered: std::collections::BTreeSet<&str> =
        SETTING_REGISTRY.iter().map(|e| e.key).collect();
    let mut missing = Vec::new();
    for key in cfg.settings.keys() {
        if !registered.contains(key.as_str()) {
            missing.push(key.clone());
        }
    }
    assert!(
        missing.is_empty(),
        "bundled keys missing from setting registry: {missing:?}"
    );
    assert_eq!(
        registered.len(),
        cfg.settings.len(),
        "registry size {} != bundled top-level key count {}",
        registered.len(),
        cfg.settings.len()
    );
}

#[test]
fn zc_setting_registry_no_user_must_set() {
    // Zero-config: every key is Constant | Hardware | Detection | Measurement | Consent.
    // There is deliberately no SettingDerivation::UserMustSet variant.
    for entry in SETTING_REGISTRY {
        match entry.derivation {
            SettingDerivation::Constant
            | SettingDerivation::Hardware
            | SettingDerivation::Detection
            | SettingDerivation::Measurement
            | SettingDerivation::Consent => {}
        }
        assert!(!entry.key.is_empty(), "registry entry must name a key");
    }
    assert_eq!(
        SETTING_REGISTRY.len(),
        66,
        "bundled config has 66 top-level keys"
    );
}

#[test]
fn zc_setting_registry_lookup_and_keys() {
    assert!(crate::setting_registry::lookup("privacy").is_some());
    assert!(crate::setting_registry::lookup("no_such_key").is_none());
    let keys = crate::setting_registry::registry_keys();
    assert_eq!(keys.len(), SETTING_REGISTRY.len());
    assert!(keys.contains(&"default_model"));
}

#[test]
fn zc_setting_registry_keys_are_unique_and_lookup_matches_every_entry() {
    let keys = crate::setting_registry::registry_keys();
    assert_eq!(keys.len(), SETTING_REGISTRY.len());
    let unique: std::collections::BTreeSet<&str> = keys.iter().copied().collect();
    assert_eq!(unique.len(), keys.len(), "registry keys must be unique");

    for entry in SETTING_REGISTRY {
        let found = crate::setting_registry::lookup(entry.key).expect("every entry must resolve");
        assert_eq!(found.key, entry.key);
        assert_eq!(found.derivation, entry.derivation);
    }
}

#[test]
fn zc_setting_registry_contains_core_model_and_privacy_keys() {
    for core_key in ["default_model", "trust_level", "privacy"] {
        let entry = crate::setting_registry::lookup(core_key)
            .unwrap_or_else(|| panic!("expected core key {core_key} in SETTING_REGISTRY"));
        assert_eq!(entry.key, core_key);
    }
}
