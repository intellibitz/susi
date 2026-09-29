//! Explain the effective value of a config key and where it came from.
//!
//! Precedence (highest wins): environment → workspace override → host
//! `config.json` → active extension pack `config.default.json` → bundled
//! defaults. Secret-shaped values are redacted in rendered output.

use crate::json_util::{DynamicRegistry, DynamicValue};
use crate::SusiConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Known env vars that override a config key at runtime.
const ENV_OVERRIDES: &[(&str, &str)] = &[
    ("port_offset", "SUSI_PORT_OFFSET"),
    ("model_idle_timeout_secs", "SUSI_MODEL_IDLE_TIMEOUT_SECS"),
];

/// Where an effective setting value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "layer")]
pub enum ConfigOrigin {
    /// Compile-time `config/config.default.json` bundled in the binary.
    BundledDefault,
    /// Active extension pack's `config.default.json`.
    Pack { pack_id: String },
    /// Host `~/.susi/config.json` (or the given global dir).
    Host,
    /// Workspace `<ws>/.susi/config.json`.
    Workspace,
    /// Process environment variable that wins over every file layer.
    Environment { var: String },
}

impl ConfigOrigin {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::BundledDefault => "bundled default".to_string(),
            Self::Pack { pack_id } => format!("pack:{pack_id}"),
            Self::Host => "host config.json".to_string(),
            Self::Workspace => "workspace .susi/config.json".to_string(),
            Self::Environment { var } => format!("env:{var}"),
        }
    }
}

/// One layer that contributed (or could contribute) a value for the key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigLayer {
    pub origin: ConfigOrigin,
    /// Raw value at this layer when present; omitted when the layer has no key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<DynamicValue>,
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

fn pack_config_path(pack_root: &Path) -> PathBuf {
    pack_root.join("config.default.json")
}

/// Read the active pack's config overlay when present — never seeds the
/// extensions substrate (tests and explain must stay side-effect free).
fn read_active_pack_registry() -> Option<(String, DynamicRegistry)> {
    let root = crate::extensions::extensions_root();
    if !root.is_dir() {
        return None;
    }
    let id = std::env::var("SUSI_EXTENSION_PACK")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            // Prefer state.active when present; else "default".
            let state_path = root.join("state.json");
            fs::read_to_string(state_path)
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .and_then(|v| v.get("active").and_then(|a| a.as_str()).map(str::to_string))
        })
        .unwrap_or_else(|| "default".to_string());
    let pack_root = root.join(&id);
    load_registry(&pack_config_path(&pack_root)).map(|reg| (id, reg))
}

fn env_override_for(key: &str) -> Option<(&'static str, DynamicValue)> {
    // Only top-level keys have env overrides today.
    let top = key.split('.').next().unwrap_or(key);
    for (cfg_key, env_var) in ENV_OVERRIDES {
        if *cfg_key != top {
            continue;
        }
        if let Ok(raw) = std::env::var(env_var) {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                continue;
            }
            // Prefer JSON parse so numbers/bools round-trip; else string.
            let value = serde_json::from_str(trimmed)
                .unwrap_or_else(|_| DynamicValue::String(trimmed.to_string()));
            return Some((*env_var, value));
        }
    }
    None
}

/// Explain `key` against the live layer stack for `global_dir` and optional
/// `workspace`. Precedence matches runtime accessors (env > workspace >
/// host > pack > bundled).
#[must_use]
pub fn explain_key(key: &str, global_dir: &Path, workspace: Option<&Path>) -> ExplainedSetting {
    let bundled = SusiConfig::heal_defaults();
    let pack = read_active_pack_registry();
    let host = load_registry(&SusiConfig::get_config_path(global_dir));
    let workspace_reg =
        workspace.and_then(|ws| load_registry(&ws.join(".susi").join("config.json")));

    let mut layers = Vec::new();
    layers.push(ConfigLayer {
        origin: ConfigOrigin::BundledDefault,
        value: get_path(bundled, key).map(|v| maybe_redact(key, v)),
    });
    if let Some((pack_id, reg)) = &pack {
        layers.push(ConfigLayer {
            origin: ConfigOrigin::Pack {
                pack_id: pack_id.clone(),
            },
            value: get_path(reg, key).map(|v| maybe_redact(key, v)),
        });
    }
    layers.push(ConfigLayer {
        origin: ConfigOrigin::Host,
        value: host
            .as_ref()
            .and_then(|r| get_path(r, key))
            .map(|v| maybe_redact(key, v)),
    });
    layers.push(ConfigLayer {
        origin: ConfigOrigin::Workspace,
        value: workspace_reg
            .as_ref()
            .and_then(|r| get_path(r, key))
            .map(|v| maybe_redact(key, v)),
    });
    if let Some((var, value)) = env_override_for(key) {
        layers.push(ConfigLayer {
            origin: ConfigOrigin::Environment {
                var: var.to_string(),
            },
            value: Some(maybe_redact(key, value)),
        });
    }

    // Highest layer with a value wins.
    let (origin, value) = layers
        .iter()
        .rev()
        .find_map(|layer| layer.value.clone().map(|v| (layer.origin.clone(), v)))
        .unwrap_or((ConfigOrigin::BundledDefault, DynamicValue::Null));

    ExplainedSetting {
        key: key.to_string(),
        value,
        origin,
        layers,
    }
}

/// Explain against the process global config dir (and no workspace).
#[must_use]
pub fn explain_key_global(key: &str) -> ExplainedSetting {
    explain_key(key, &susi_paths::SusiDirs::config_dir(), None)
}
