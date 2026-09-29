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
        68,
        "bundled config has 68 top-level keys"
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
