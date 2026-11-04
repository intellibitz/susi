//! Explain the effective value of a config key and where it came from.
//!
//! Precedence (highest wins): environment → host `config.json` → bundled
//! defaults — exactly the layers `SusiConfig::load` and the env-aware
//! accessors resolve at runtime. Secret-shaped values are redacted in
//! rendered output.
//!
//! Extension packs and workspace `.susi/` directories are not config layers:
//! no runtime resolver reads `<pack>/config.default.json` or
//! `<ws>/.susi/config.json`, so explain must not report them as origins.
//! A set-but-rejected env override (e.g. an unparseable `SUSI_PORT_OFFSET`)
//! is still listed for debugging, marked `effective: false`.

use crate::json_util::{DynamicRegistry, DynamicValue};
use crate::SusiConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Value an env override contributes, in the shape the accessor consumes —
/// `None` when the accessor would reject it (the accessor's parse wins).
fn u16_env_value(raw: &str) -> Option<DynamicValue> {
    raw.parse::<u16>().ok().map(DynamicValue::from)
}

fn u64_env_value(raw: &str) -> Option<DynamicValue> {
    raw.parse::<u64>().ok().map(DynamicValue::from)
}

/// The parse an env override's owning accessor applies to the raw value.
type EnvValidator = fn(&str) -> Option<DynamicValue>;

/// Known env vars that override a config key at runtime, each with the parse
/// the owning accessor applies — an env value that fails it never wins.
const ENV_OVERRIDES: &[(&str, &str, EnvValidator)] = &[
    ("port_offset", "SUSI_PORT_OFFSET", u16_env_value),
    (
        "model_idle_timeout_secs",
        "SUSI_MODEL_IDLE_TIMEOUT_SECS",
        u64_env_value,
    ),
];

/// Where an effective setting value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "layer")]
pub enum ConfigOrigin {
    /// Compile-time `config/config.default.json` bundled in the binary.
    BundledDefault,
    /// Host `~/.susi/config.json` (or the given global dir).
    Host,
    /// Process environment variable that wins over every file layer.
    Environment { var: String },
}

impl ConfigOrigin {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::BundledDefault => "bundled default".to_string(),
            Self::Host => "host config.json".to_string(),
            Self::Environment { var } => format!("env:{var}"),
        }
    }
}

fn layer_effective_default() -> bool {
    true
}

/// One layer that contributed (or could contribute) a value for the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigLayer {
    pub origin: ConfigOrigin,
    /// Raw value at this layer when present; omitted when the layer has no key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<DynamicValue>,
    /// Whether the runtime resolver actually consumes this layer's value.
    /// False when the layer is set but rejected — e.g. an env override the
    /// accessor cannot parse. Never effective layers cannot win.
    #[serde(
        default = "layer_effective_default",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub effective: bool,
}

/// Resolved setting with provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplainedSetting {
    pub key: String,
    /// Effective value after precedence (secret-shaped values already redacted).
    pub value: DynamicValue,
    pub origin: ConfigOrigin,
    /// Full stack from lowest to highest precedence (for fixtures / debugging).
    pub layers: Vec<ConfigLayer>,
}

impl ExplainedSetting {
    /// One-line operator summary: `key = <value> (from <origin>)`.
    #[must_use]
    pub fn render(&self) -> String {
        let rendered = match serde_json::to_string(&self.value) {
            Ok(s) => s,
            Err(_) => "<unserializable>".to_string(),
        };
        format!("{} = {} (from {})", self.key, rendered, self.origin.label())
    }
}

fn path_looks_secret(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("secret")
        || lower.contains("token")
        || lower.contains("password")
        || lower.contains("api_key")
        || lower.ends_with("_key")
}

fn maybe_redact(key: &str, value: DynamicValue) -> DynamicValue {
    if path_looks_secret(key) {
        DynamicValue::String("[redacted]".to_string())
    } else {
        value
    }
}

fn get_path(settings: &DynamicRegistry, key: &str) -> Option<DynamicValue> {
    if !key.contains('.') {
        return settings.get(key).cloned();
    }
    let mut parts = key.split('.');
    let top = parts.next()?;
    let mut cur = settings.get(top)?;
    for part in parts {
        cur = match cur {
            DynamicValue::Object(map) => map.get(part)?,
            DynamicValue::Array(arr) => {
                let idx: usize = part.parse().ok()?;
                arr.get(idx)?
            }
            DynamicValue::Null
            | DynamicValue::Bool(_)
            | DynamicValue::Number(_)
            | DynamicValue::String(_) => return None,
        };
    }
    Some(cur.clone())
}

fn load_registry(path: &Path) -> Option<DynamicRegistry> {
    let text = fs::read_to_string(path).ok()?;
    let cfg: SusiConfig = serde_json::from_str(&text).ok()?;
    Some(cfg.settings)
}

/// Env layer for `key` when its override var is set: `(var, value, effective)`.
/// `effective` is false when the value fails the accessor's parse — the raw
/// text is reported for debugging but the layer can never win, matching the
/// runtime accessor that discards it.
fn env_override_for(key: &str) -> Option<(&'static str, DynamicValue, bool)> {
    // Only top-level keys have env overrides today.
    let top = key.split('.').next().unwrap_or(key);
    for (cfg_key, env_var, validate) in ENV_OVERRIDES {
        if *cfg_key != top {
            continue;
        }
        if let Ok(raw) = std::env::var(env_var) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            return Some(match validate(trimmed) {
                Some(value) => (*env_var, value, true),
                None => (*env_var, DynamicValue::String(trimmed.to_string()), false),
            });
        }
    }
    None
}

/// Explain `key` against the live layer stack for `global_dir`. The stack is
/// exactly what the runtime resolves: environment → host → bundled.
#[must_use]
pub fn explain_key(key: &str, global_dir: &Path) -> ExplainedSetting {
    let bundled = SusiConfig::heal_defaults();
    let host = load_registry(&SusiConfig::get_config_path(global_dir));

    let mut layers = Vec::new();
    layers.push(ConfigLayer {
        origin: ConfigOrigin::BundledDefault,
        value: get_path(bundled, key).map(|v| maybe_redact(key, v)),
        effective: true,
    });
    layers.push(ConfigLayer {
        origin: ConfigOrigin::Host,
        value: host
            .as_ref()
            .and_then(|r| get_path(r, key))
            .map(|v| maybe_redact(key, v)),
        effective: true,
    });
    if let Some((var, value, effective)) = env_override_for(key) {
        layers.push(ConfigLayer {
            origin: ConfigOrigin::Environment {
                var: var.to_string(),
            },
            value: Some(maybe_redact(key, value)),
            effective,
        });
    }

    // Highest effective layer with a value wins — matching the accessors.
    let (origin, value) = layers
        .iter()
        .rev()
        .filter(|layer| layer.effective)
        .find_map(|layer| layer.value.clone().map(|v| (layer.origin.clone(), v)))
        .unwrap_or((ConfigOrigin::BundledDefault, DynamicValue::Null));

    ExplainedSetting {
        key: key.to_string(),
        value,
        origin,
        layers,
    }
}

/// Explain against the process global config dir.
#[must_use]
pub fn explain_key_global(key: &str) -> ExplainedSetting {
    explain_key(key, &susi_paths::SusiDirs::config_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_origin_labels_secret_redaction_and_nested_get_path() {
        assert_eq!(ConfigOrigin::BundledDefault.label(), "bundled default");
        assert_eq!(ConfigOrigin::Host.label(), "host config.json");
        assert_eq!(
            ConfigOrigin::Environment {
                var: "SUSI_PORT_OFFSET".into()
            }
            .label(),
            "env:SUSI_PORT_OFFSET"
        );

        for secret_key in [
            "client_secret",
            "api_token",
            "db_password",
            "openai_api_key",
            "signing_key",
        ] {
            assert!(
                path_looks_secret(secret_key),
                "{secret_key} must look secret"
            );
            assert_eq!(
                maybe_redact(secret_key, DynamicValue::String("raw-val".into())),
                DynamicValue::String("[redacted]".into())
            );
        }
        assert!(!path_looks_secret("port_offset"));
        assert_eq!(
            maybe_redact("port_offset", DynamicValue::from(15)),
            DynamicValue::from(15)
        );

        let mut reg = DynamicRegistry::new();
        reg.insert("top".into(), DynamicValue::String("val".into()));
        reg.insert("a".into(), serde_json::json!({ "b": 99 }));
        reg.insert(
            "arr".into(),
            DynamicValue::Array(vec![
                DynamicValue::String("zero".into()),
                DynamicValue::String("one".into()),
            ]),
        );

        assert_eq!(
            get_path(&reg, "top"),
            Some(DynamicValue::String("val".into()))
        );
        assert_eq!(get_path(&reg, "a.b"), Some(DynamicValue::from(99)));
        assert_eq!(
            get_path(&reg, "arr.1"),
            Some(DynamicValue::String("one".into()))
        );
        assert_eq!(get_path(&reg, "arr.5"), None);
        assert_eq!(get_path(&reg, "arr.not_an_index"), None);
        assert_eq!(get_path(&reg, "top.child"), None);
        assert_eq!(get_path(&reg, "missing"), None);
    }

    #[test]
    fn explain_key_resolves_bundled_host_and_secret_redaction_in_hermetic_tempdir() {
        let _guard = crate::env_test_lock();
        unsafe {
            std::env::remove_var("SUSI_PORT_OFFSET");
        }

        let dir =
            std::env::temp_dir().join(format!("susi-explain-inline-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        let global_dir = dir.as_path();

        let bundled = explain_key("port_offset", global_dir);
        assert_eq!(bundled.origin, ConfigOrigin::BundledDefault);
        assert_eq!(bundled.value, DynamicValue::from(0));
        assert_eq!(bundled.render(), "port_offset = 0 (from bundled default)");

        let mut settings = SusiConfig::heal_defaults().clone();
        settings.insert("port_offset".into(), DynamicValue::from(7));
        settings.insert(
            "api_token".into(),
            DynamicValue::String("top-secret-token-value".into()),
        );
        fs::write(
            SusiConfig::get_config_path(global_dir),
            serde_json::to_string_pretty(&SusiConfig { settings }).expect("serialize host config"),
        )
        .expect("write host config.json");

        let host_explained = explain_key("port_offset", global_dir);
        assert_eq!(host_explained.origin, ConfigOrigin::Host);
        assert_eq!(host_explained.value, DynamicValue::from(7));
        assert_eq!(
            host_explained.render(),
            "port_offset = 7 (from host config.json)"
        );

        let secret_explained = explain_key("api_token", global_dir);
        assert_eq!(secret_explained.origin, ConfigOrigin::Host);
        assert_eq!(
            secret_explained.value,
            DynamicValue::String("[redacted]".into())
        );
        assert!(!secret_explained.render().contains("top-secret-token-value"));
        for layer in &secret_explained.layers {
            if let Some(val) = &layer.value {
                assert_eq!(val, &DynamicValue::String("[redacted]".into()));
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
