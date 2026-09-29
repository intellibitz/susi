//! Tests for `explain` (`zc_config_explain_*`).

use crate::explain::{explain_key, ConfigOrigin};
use crate::json_util::DynamicValue;
use crate::SusiConfig;
use std::fs;
use std::path::PathBuf;

struct TempHome {
    _guard: std::sync::MutexGuard<'static, ()>,
    home: PathBuf,
    global: PathBuf,
}

impl TempHome {
    fn new(tag: &str) -> Self {
        let guard = crate::env_test_lock();
        let home = std::env::temp_dir().join(format!(
            "susi_zc_explain_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::remove_dir_all(&home);
        let global = home.join(".susi");
        fs::create_dir_all(&global).unwrap();
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("SUSI_EXTENSION_PACK");
            std::env::remove_var("SUSI_PORT_OFFSET");
        }
        Self {
            _guard: guard,
            home,
            global,
        }
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("SUSI_PORT_OFFSET");
            std::env::remove_var("SUSI_EXTENSION_PACK");
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

#[test]
fn zc_config_explain_bundled_default_when_no_overrides() {
    let t = TempHome::new("bundled");
    let explained = explain_key("trust_level", &t.global, None);
    assert_eq!(explained.origin, ConfigOrigin::BundledDefault);
    assert!(explained.value != DynamicValue::Null);
    assert!(explained.render().contains("from bundled default"));
}

#[test]
fn zc_config_explain_host_wins_over_bundled() {
    let t = TempHome::new("host");
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert(
        "trust_level".into(),
        DynamicValue::String("paranoid".into()),
    );
    fs::write(
        SusiConfig::get_config_path(&t.global),
        serde_json::to_string_pretty(&SusiConfig { settings }).unwrap(),
    )
    .unwrap();
    let explained = explain_key("trust_level", &t.global, None);
    assert_eq!(explained.origin, ConfigOrigin::Host);
    assert_eq!(explained.value, DynamicValue::String("paranoid".into()));
}

#[test]
fn zc_config_explain_workspace_wins_over_host() {
    let t = TempHome::new("ws");
    let ws = t.home.join("project");
    fs::create_dir_all(ws.join(".susi")).unwrap();
    let mut host = SusiConfig::heal_defaults().clone();
    host.insert("trust_level".into(), DynamicValue::String("host".into()));
    fs::write(
        SusiConfig::get_config_path(&t.global),
        serde_json::to_string_pretty(&SusiConfig { settings: host }).unwrap(),
    )
    .unwrap();
    let mut workspace = SusiConfig::heal_defaults().clone();
    workspace.insert(
        "trust_level".into(),
        DynamicValue::String("workspace".into()),
    );
    fs::write(
        ws.join(".susi/config.json"),
        serde_json::to_string_pretty(&SusiConfig {
            settings: workspace,
        })
        .unwrap(),
    )
    .unwrap();
    let explained = explain_key("trust_level", &t.global, Some(&ws));
    assert_eq!(explained.origin, ConfigOrigin::Workspace);
    assert_eq!(explained.value, DynamicValue::String("workspace".into()));
}

#[test]
fn zc_config_explain_env_wins_and_redacts_secrets() {
    let t = TempHome::new("env");
    unsafe {
        std::env::set_var("SUSI_PORT_OFFSET", "42");
    }
    let explained = explain_key("port_offset", &t.global, None);
    assert_eq!(
        explained.origin,
        ConfigOrigin::Environment {
            var: "SUSI_PORT_OFFSET".into()
        }
    );
    assert_eq!(explained.value, DynamicValue::from(42));
    unsafe {
        std::env::remove_var("SUSI_PORT_OFFSET");
    }

    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert(
        "api_auth_token".into(),
        DynamicValue::String("super-secret-token".into()),
    );
    fs::write(
        SusiConfig::get_config_path(&t.global),
        serde_json::to_string_pretty(&SusiConfig { settings }).unwrap(),
    )
    .unwrap();
    let secret = explain_key("api_auth_token", &t.global, None);
    assert!(!secret.render().contains("super-secret-token"));
    assert_eq!(secret.value, DynamicValue::String("[redacted]".into()));
}

#[test]
fn zc_config_explain_pack_layer_between_bundled_and_host() {
    let t = TempHome::new("pack");
    let pack_root = t.global.join("extensions/default");
    fs::create_dir_all(&pack_root).unwrap();
    let mut pack_settings = SusiConfig::heal_defaults().clone();
    pack_settings.insert(
        "default_engine".into(),
        DynamicValue::String("pack-engine".into()),
    );
    fs::write(
        pack_root.join("config.default.json"),
        serde_json::to_string_pretty(&SusiConfig {
            settings: pack_settings,
        })
        .unwrap(),
    )
    .unwrap();
    let explained = explain_key("default_engine", &t.global, None);
    assert_eq!(
        explained.origin,
        ConfigOrigin::Pack {
            pack_id: "default".into()
        }
    );
    assert_eq!(explained.value, DynamicValue::String("pack-engine".into()));
}
