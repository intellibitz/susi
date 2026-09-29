//! Validate a persisted `config.json` against the bundled schema.
//!
//! Reports unknown keys, deprecated keys (host-contract ports and
//! `api_auth_token`), and drift from bundled defaults — each with an
//! actionable fix. Secret-shaped fix values are redacted in rendered output.

use crate::json_util::{merge_missing_registry_defaults, DynamicRegistry, DynamicValue};
use crate::susi_error::{EaiError, EaiResult};
use crate::SusiConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Keys that must not live in `config.json` even when the bundled default
/// documents them. Host-contract ports are compile-time
/// (`susi_paths::ports` + `port_offset`); the bearer token lives only in
/// `~/.susi/api_token` (0600).
const DEPRECATED_TOP_LEVEL: &[(&str, &str)] = &[
    (
        "gmcp_port",
        "host-contract port; use port_offset (or SUSI_PORT_OFFSET), not a per-port override",
    ),
    (
        "gmcp_http_port",
        "host-contract port; use port_offset (or SUSI_PORT_OFFSET), not a per-port override",
    ),
    (
        "gemi_port",
        "host-contract port; use port_offset (or SUSI_PORT_OFFSET), not a per-port override",
    ),
    (
        "udp_discovery_port",
        "host-contract port; use port_offset (or SUSI_PORT_OFFSET), not a per-port override",
    ),
    (
        "a2a_http_port",
        "host-contract port; use port_offset (or SUSI_PORT_OFFSET), not a per-port override",
    ),
    (
        "api_auth_token",
        "bearer lives only in ~/.susi/api_token (0600); never persist it in config.json",
    ),
];

/// Kind of validation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigIssueKind {
    /// Present in the user file, absent from the bundled schema.
    Unknown,
    /// Known legacy / forbidden key that should be removed.
    Deprecated,
    /// Present in bundled defaults, absent from the user file (schema drift).
    Missing,
    /// Same path exists on both sides but with incompatible JSON types.
    TypeMismatch,
}

/// How to repair one issue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFix {
    /// Delete the key at `path`.
    Remove,
    /// Write this JSON value at `path` (bundled default).
    Set(DynamicValue),
}

/// One actionable finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigIssue {
    /// Dotted JSON path (`governance.secret_tokens`, `model_ladder.0.repo`).
    pub path: String,
    pub kind: ConfigIssueKind,
    pub message: String,
    pub fix: Option<ConfigFix>,
}

/// Full validation result.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigValidationReport {
    pub issues: Vec<ConfigIssue>,
}

impl ConfigValidationReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// True when any issue blocks a clean config (unknown or type mismatch).
    /// Deprecated and missing are healable warnings.
    #[must_use]
    pub fn has_blocking(&self) -> bool {
        self.issues.iter().any(|i| {
            matches!(
                i.kind,
                ConfigIssueKind::Unknown | ConfigIssueKind::TypeMismatch
            )
        })
    }

    /// Human-readable report with fixes; secret-shaped `Set` values are redacted.
    #[must_use]
    pub fn render(&self) -> String {
        if self.issues.is_empty() {
            return "config ok: no unknown, deprecated, or missing keys relative to bundled defaults"
                .to_string();
        }
        let mut out = String::new();
        for issue in &self.issues {
            let kind = match issue.kind {
                ConfigIssueKind::Unknown => "UNKNOWN",
                ConfigIssueKind::Deprecated => "DEPRECATED",
                ConfigIssueKind::Missing => "MISSING",
                ConfigIssueKind::TypeMismatch => "TYPE_MISMATCH",
            };
            out.push_str(kind);
            out.push(' ');
            out.push_str(&issue.path);
            out.push_str(": ");
            out.push_str(&issue.message);
            out.push('\n');
            if let Some(fix) = &issue.fix {
                out.push_str("  fix: ");
                out.push_str(&render_fix(&issue.path, fix));
                out.push('\n');
            }
        }
        out
    }
}

fn render_fix(path: &str, fix: &ConfigFix) -> String {
    match fix {
        ConfigFix::Remove => "remove key".to_string(),
        ConfigFix::Set(v) => {
            let rendered = if path_looks_secret(path) {
                DynamicValue::String("[redacted]".to_string())
            } else {
                v.clone()
            };
            match serde_json::to_string(&rendered) {
                Ok(s) => format!("set to {s}"),
                Err(_) => "set to <unserializable>".to_string(),
            }
        }
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

fn is_deprecated_top_level(key: &str) -> Option<&'static str> {
    DEPRECATED_TOP_LEVEL
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, why)| *why)
}

/// Validate an in-memory settings map against the heal schema (bundled
/// defaults minus host-contract ports that must never be persisted).
#[must_use]
pub fn validate_settings(settings: &DynamicRegistry) -> ConfigValidationReport {
    let schema = SusiConfig::heal_defaults();
    let mut issues = Vec::new();

    for (key, why) in DEPRECATED_TOP_LEVEL {
        if settings.contains_key(*key) {
            issues.push(ConfigIssue {
                path: (*key).to_string(),
                kind: ConfigIssueKind::Deprecated,
                message: (*why).to_string(),
                fix: Some(ConfigFix::Remove),
            });
        }
    }

    walk_object("", settings, schema, &mut issues);
    ConfigValidationReport { issues }
}

fn walk_object(
    prefix: &str,
    existing: &DynamicRegistry,
    schema: &DynamicRegistry,
    issues: &mut Vec<ConfigIssue>,
) {
    for (key, schema_val) in schema {
        let path = join_path(prefix, key);
        match existing.get(key) {
            None => {
                issues.push(ConfigIssue {
                    path: path.clone(),
                    kind: ConfigIssueKind::Missing,
                    message: "absent from config; bundled default will apply until healed"
                        .to_string(),
                    fix: Some(ConfigFix::Set(schema_val.clone())),
                });
            }
            Some(existing_val) => {
                walk_value(&path, existing_val, schema_val, issues);
            }
        }
    }

    for key in existing.keys() {
        if schema.contains_key(key) {
            continue;
        }
        if prefix.is_empty() && is_deprecated_top_level(key).is_some() {
            // Already reported as Deprecated.
            continue;
        }
        issues.push(ConfigIssue {
            path: join_path(prefix, key),
            kind: ConfigIssueKind::Unknown,
            message: "not in bundled schema".to_string(),
            fix: Some(ConfigFix::Remove),
        });
    }
}

fn walk_value(
    path: &str,
    existing: &DynamicValue,
    schema: &DynamicValue,
    issues: &mut Vec<ConfigIssue>,
) {
    match (existing, schema) {
        (DynamicValue::Object(ex), DynamicValue::Object(sch)) => {
            // Convert Map → HashMap-compatible iteration via DynamicRegistry shape.
            let ex_reg: DynamicRegistry = ex.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            let sch_reg: DynamicRegistry =
                sch.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            walk_object(path, &ex_reg, &sch_reg, issues);
        }
        (DynamicValue::Array(ex), DynamicValue::Array(sch)) if ex.len() == sch.len() => {
            for (i, (e, s)) in ex.iter().zip(sch.iter()).enumerate() {
                walk_value(&format!("{path}.{i}"), e, s, issues);
            }
        }
        (DynamicValue::Array(_), DynamicValue::Array(_)) => {
            // Length differs: not missing keys — user-shaped array; leave alone.
        }
        (a, b) if same_json_type(a, b) => {}
        (a, b) => {
            issues.push(ConfigIssue {
                path: path.to_string(),
                kind: ConfigIssueKind::TypeMismatch,
                message: format!(
                    "expected {}, found {}",
                    json_type_name(b),
                    json_type_name(a)
                ),
                fix: Some(ConfigFix::Set(schema.clone())),
            });
        }
    }
}

fn same_json_type(a: &DynamicValue, b: &DynamicValue) -> bool {
    json_type_name(a) == json_type_name(b)
}

fn json_type_name(v: &DynamicValue) -> &'static str {
    match v {
        DynamicValue::Null => "null",
        DynamicValue::Bool(_) => "bool",
        DynamicValue::Number(_) => "number",
        DynamicValue::String(_) => "string",
        DynamicValue::Array(_) => "array",
        DynamicValue::Object(_) => "object",
    }
}

fn join_path(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_string()
    } else {
        format!("{prefix}.{key}")
    }
}

/// Read `global_dir/config.json` as-is (no heal) and validate it.
/// An absent file is clean: runtime uses bundled defaults.
pub fn validate_dir(global_dir: &Path) -> EaiResult<ConfigValidationReport> {
    let path = SusiConfig::get_config_path(global_dir);
    if !path.exists() {
        return Ok(ConfigValidationReport::default());
    }
    let content = fs::read_to_string(&path).map_err(|e| EaiError::config(e.to_string()))?;
    let cfg: SusiConfig =
        serde_json::from_str(&content).map_err(|e| EaiError::config(e.to_string()))?;
    Ok(validate_settings(&cfg.settings))
}

/// Apply healable fixes: backfill missing defaults and strip deprecated keys.
/// Unknown and type-mismatch issues are left for the operator.
/// Returns the post-fix validation report.
pub fn apply_fixes(global_dir: &Path) -> EaiResult<ConfigValidationReport> {
    let path = SusiConfig::get_config_path(global_dir);
    let mut cfg = if path.exists() {
        let content = fs::read_to_string(&path).map_err(|e| EaiError::config(e.to_string()))?;
        serde_json::from_str::<SusiConfig>(&content).map_err(|e| EaiError::config(e.to_string()))?
    } else {
        SusiConfig::default()
    };

    for (key, _) in DEPRECATED_TOP_LEVEL {
        cfg.settings.remove(*key);
    }
    let _ = merge_missing_registry_defaults(&mut cfg.settings, SusiConfig::heal_defaults());
    cfg.save(global_dir)?;
    Ok(validate_settings(&cfg.settings))
}
