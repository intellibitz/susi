//! Mastery verification for VC-201-062: explain the resolved value and origin
//! of each setting across the layers runtime actually resolves — bundled
//! defaults, host config, environment — with secret-shaped values redacted.
//!
//! The claim under test is not "explain_key renders a layer stack" — the
//! cited `zc_config_explain` tests already show the stack. The distinguishing
//! property is that the stack explain reports is the stack the runtime
//! actually resolves. These tests compare `explain_key` against the real
//! resolver (`SusiConfig::load` + accessors) on the same fixture. Extension
//! pack and workspace `config.default.json`/`config.json` files exist on
//! disk but are not runtime config layers, so explain must not report them.

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
            "susi_vc201062_{tag}_{}_{}",
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
            std::env::remove_var("SUSI_HOME");
            std::env::remove_var("SUSI_EXTENSION_PACK");
            std::env::remove_var("SUSI_PORT_OFFSET");
            std::env::remove_var("SUSI_MODEL_IDLE_TIMEOUT_SECS");
        }
        Self {
            _guard: guard,
            home,
            global,
        }
    }

    fn write_host(&self, key: &str, value: DynamicValue) {
        let mut settings = SusiConfig::heal_defaults().clone();
        settings.insert(key.into(), value);
        fs::write(
            SusiConfig::get_config_path(&self.global),
            serde_json::to_string_pretty(&SusiConfig { settings }).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        unsafe {
            std::env::remove_var("SUSI_PORT_OFFSET");
            std::env::remove_var("SUSI_MODEL_IDLE_TIMEOUT_SECS");
            std::env::remove_var("SUSI_EXTENSION_PACK");
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

/// A pack's `config.default.json` is not a runtime config layer:
/// `SusiConfig::load` never reads it, so explain must not report it. With a
/// pack file on disk, the explained value and origin equal the bundled
/// default the runtime resolves.
#[test]
fn vc_201_062_mastery_pack_file_is_not_reported_as_a_layer() {
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

    let explained = explain_key("default_engine", &t.global);
    assert_eq!(
        explained.layers.len(),
        2,
        "pack is not a runtime layer — only bundled and host are reported"
    );
    assert_eq!(explained.origin, ConfigOrigin::BundledDefault);

    let runtime = SusiConfig::load(&t.global).unwrap();
    assert_eq!(
        explained.value,
        DynamicValue::String(runtime.get::<String>("default_engine").unwrap()),
        "explained effective value equals the runtime resolution"
    );
}

/// A workspace `<ws>/.susi/config.json` is not a runtime config layer either:
/// nothing reads it, so explain must not let it beat the host file.
#[test]
fn vc_201_062_mastery_workspace_config_is_not_reported_as_a_layer() {
    let t = TempHome::new("ws");
    t.write_host("trust_level", DynamicValue::String("host".into()));
    let ws = t.home.join("project");
    fs::create_dir_all(ws.join(".susi")).unwrap();
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

    let explained = explain_key("trust_level", &t.global);
    assert_eq!(explained.origin, ConfigOrigin::Host);
    assert_eq!(explained.value, DynamicValue::String("host".into()));

    let runtime = SusiConfig::load(&t.global).unwrap();
    assert_eq!(runtime.trust_level(), "host");
}

/// An env override the accessor cannot parse is reported (for debugging) but
/// marked ineffective and never wins — matching the accessor that discards it.
#[test]
fn vc_201_062_mastery_unparseable_env_value_is_reported_but_never_wins() {
    let t = TempHome::new("env-bad");
    t.write_host("port_offset", DynamicValue::from(7));
    unsafe {
        std::env::set_var("SUSI_PORT_OFFSET", "not-a-port");
    }

    let explained = explain_key("port_offset", &t.global);
    assert_eq!(explained.origin, ConfigOrigin::Host);
    assert_eq!(explained.value, DynamicValue::from(7));
    let env_layer = explained
        .layers
        .iter()
        .find(|l| {
            l.origin
                == ConfigOrigin::Environment {
                    var: "SUSI_PORT_OFFSET".into(),
                }
        })
        .expect("set-but-rejected env override is still listed");
    assert!(!env_layer.effective, "unparseable env value cannot win");
    assert_eq!(
        env_layer.value,
        Some(DynamicValue::String("not-a-port".into())),
        "the raw rejected text stays visible for debugging"
    );

    let runtime = SusiConfig::load(&t.global).unwrap();
    assert_eq!(runtime.port_offset(), 7);
}

/// Where runtime does apply a layer, explain must agree with it — the
/// precedence that *is* implemented (env over host over bundled) matches.
#[test]
fn vc_201_062_mastery_implemented_layers_agree_with_runtime() {
    let t = TempHome::new("layers-ok");

    // Bundled default.
    let explained = explain_key("trust_level", &t.global);
    let runtime = SusiConfig::load(&t.global).unwrap();
    assert_eq!(explained.origin, ConfigOrigin::BundledDefault);
    assert_eq!(
        explained.value,
        DynamicValue::String(runtime.trust_level()),
        "bundled layer value must match runtime"
    );

    // Host wins over bundled, matching runtime.
    t.write_host("trust_level", DynamicValue::String("host".into()));
    let explained = explain_key("trust_level", &t.global);
    let runtime = SusiConfig::load(&t.global).unwrap();
    assert_eq!(explained.origin, ConfigOrigin::Host);
    assert_eq!(explained.value, DynamicValue::String("host".into()));
    assert_eq!(runtime.trust_level(), "host");

    // Valid env wins over host, matching runtime.
    unsafe {
        std::env::set_var("SUSI_PORT_OFFSET", "9");
    }
    let explained = explain_key("port_offset", &t.global);
    let runtime = SusiConfig::load(&t.global).unwrap();
    assert_eq!(
        explained.origin,
        ConfigOrigin::Environment {
            var: "SUSI_PORT_OFFSET".into()
        }
    );
    assert_eq!(explained.value, DynamicValue::from(9));
    assert_eq!(runtime.port_offset(), 9);
    unsafe {
        std::env::remove_var("SUSI_PORT_OFFSET");
    }
}

/// Redaction: every secret-shaped layer value and the rendered line must
/// hide the raw value. `api_auth_token` carries a credential-shaped name.
#[test]
fn vc_201_062_mastery_secret_values_redacted_in_value_layers_and_render() {
    let t = TempHome::new("secret");
    t.write_host(
        "api_auth_token",
        DynamicValue::String("super-secret-token".into()),
    );
    let explained = explain_key("api_auth_token", &t.global);
    assert_eq!(explained.value, DynamicValue::String("[redacted]".into()));
    for layer in &explained.layers {
        if let Some(v) = &layer.value {
            assert_eq!(
                *v,
                DynamicValue::String("[redacted]".into()),
                "layer {:?} leaked the raw secret",
                layer.origin
            );
        }
    }
    assert!(!explained.render().contains("super-secret-token"));
}
