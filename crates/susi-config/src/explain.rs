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
