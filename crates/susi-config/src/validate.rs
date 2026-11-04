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
    (
        "mcp_registry_url",
        "remote MCP registry refresh was removed (zero-UI plan A8); the key is ignored",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "susi-validate-unit-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn validate_settings_classifies_deprecated_missing_unknown_and_type_mismatch_and_redacts_secret_fixes(
    ) {
        let clean = validate_settings(SusiConfig::heal_defaults());
        assert!(clean.is_clean());
        assert!(!clean.has_blocking());
        assert!(clean.render().starts_with("config ok:"));

        let mut deprecated_map = SusiConfig::heal_defaults().clone();
        deprecated_map.insert("gmcp_port".into(), DynamicValue::from(9090));
        deprecated_map.insert(
            "api_auth_token".into(),
            DynamicValue::String("legacy-token".into()),
        );
        deprecated_map.insert(
            "mcp_registry_url".into(),
            DynamicValue::String("https://example.invalid".into()),
        );
        let dep_report = validate_settings(&deprecated_map);
        assert!(!dep_report.is_clean());
        assert!(!dep_report.has_blocking());
        for key in ["gmcp_port", "api_auth_token", "mcp_registry_url"] {
            assert!(dep_report.issues.iter().any(|i| {
                i.path == key
                    && i.kind == ConfigIssueKind::Deprecated
                    && i.fix == Some(ConfigFix::Remove)
            }));
        }

        let mut drifted = SusiConfig::heal_defaults().clone();
        drifted.remove("trust_level");
        drifted.insert("unknown_custom_key".into(), DynamicValue::Bool(true));
        drifted.insert(
            "port_offset".into(),
            DynamicValue::String("not-a-number".into()),
        );
        if let Some(DynamicValue::Object(gov)) = drifted.get_mut("governance") {
            gov.remove("secret_tokens");
        }
        let drift_report = validate_settings(&drifted);
        assert!(drift_report.has_blocking());
        assert!(drift_report.issues.iter().any(|i| {
            i.path == "trust_level"
                && i.kind == ConfigIssueKind::Missing
                && matches!(i.fix, Some(ConfigFix::Set(_)))
        }));
        assert!(drift_report.issues.iter().any(|i| {
            i.path == "unknown_custom_key"
                && i.kind == ConfigIssueKind::Unknown
                && i.fix == Some(ConfigFix::Remove)
        }));
        assert!(drift_report.issues.iter().any(|i| {
            i.path == "port_offset"
                && i.kind == ConfigIssueKind::TypeMismatch
                && matches!(i.fix, Some(ConfigFix::Set(_)))
        }));

        let rendered = drift_report.render();
        assert!(rendered.contains("MISSING governance.secret_tokens:"));
        assert!(rendered.contains("fix: set to \"[redacted]\""));
    }

    #[test]
    fn validate_dir_and_apply_fixes_strip_deprecated_keys_and_backfill_missing_defaults() {
        let dir = unique_temp_dir("apply-fixes");
        let absent_report = validate_dir(&dir).expect("validate_dir on absent config.json");
        assert!(absent_report.is_clean());

        let mut settings = SusiConfig::heal_defaults().clone();
        settings.remove("trust_level");
        settings.insert("gmcp_port".into(), DynamicValue::from(9090));
        settings.insert(
            "api_auth_token".into(),
            DynamicValue::String("secret-bearer".into()),
        );
        settings.insert("operator_unknown_key".into(), DynamicValue::Bool(true));
        fs::write(
            SusiConfig::get_config_path(&dir),
            serde_json::to_string_pretty(&SusiConfig { settings }).expect("serialize config"),
        )
        .expect("write config.json");

        let pre_report = validate_dir(&dir).expect("validate_dir before fixes");
        assert!(!pre_report.is_clean());
        assert!(pre_report.has_blocking());

        let post_report = apply_fixes(&dir).expect("apply_fixes");
        assert!(post_report.has_blocking());
        assert_eq!(post_report.issues.len(), 1);
        assert_eq!(post_report.issues[0].path, "operator_unknown_key");
        assert_eq!(post_report.issues[0].kind, ConfigIssueKind::Unknown);

        let reloaded: SusiConfig = serde_json::from_str(
            &fs::read_to_string(SusiConfig::get_config_path(&dir)).expect("read fixed config.json"),
        )
        .expect("parse fixed config.json");
        assert!(!reloaded.settings.contains_key("gmcp_port"));
        assert!(!reloaded.settings.contains_key("api_auth_token"));
        assert!(reloaded.settings.contains_key("trust_level"));
        assert!(reloaded.settings.contains_key("operator_unknown_key"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_dir_rejects_malformed_json_and_validate_settings_flags_nested_unknown_keys() {
        let dir = unique_temp_dir("malformed-and-nested");
        fs::write(SusiConfig::get_config_path(&dir), "{not valid json")
            .expect("write malformed config.json");
        assert!(
            validate_dir(&dir).is_err(),
            "validate_dir must return Err on malformed config.json"
        );
        let _ = fs::remove_dir_all(&dir);

        let mut settings = SusiConfig::heal_defaults().clone();
        if let Some(DynamicValue::Object(gov)) = settings.get_mut("governance") {
            gov.insert(
                "unknown_nested_key".into(),
                DynamicValue::String("unexpected".into()),
            );
        }
        let report = validate_settings(&settings);
        assert!(report.has_blocking());
        assert!(report.issues.iter().any(|i| {
            i.path == "governance.unknown_nested_key"
                && i.kind == ConfigIssueKind::Unknown
                && i.fix == Some(ConfigFix::Remove)
        }));
    }

    #[test]
    fn validate_settings_walks_same_length_arrays_ignores_resized_arrays_and_apply_fixes_creates_missing_file(
    ) {
        let mut same_len = SusiConfig::heal_defaults().clone();
        if let Some(DynamicValue::Array(assets)) = same_len.get_mut("discoverable_assets") {
            assert!(!assets.is_empty());
            if let Some(DynamicValue::Object(first)) = assets.get_mut(0) {
                first.insert("url".into(), DynamicValue::from(404));
            }
        }
        let array_report = validate_settings(&same_len);
        assert!(array_report.has_blocking());
        assert!(array_report.issues.iter().any(|i| {
            i.path == "discoverable_assets.0.url" && i.kind == ConfigIssueKind::TypeMismatch
        }));

        let mut resized = SusiConfig::heal_defaults().clone();
        resized.insert("discoverable_assets".into(), DynamicValue::Array(vec![]));
        let resized_report = validate_settings(&resized);
        assert!(resized_report.is_clean());

        let empty_dir = unique_temp_dir("apply-fixes-absent");
        assert!(!SusiConfig::get_config_path(&empty_dir).exists());
        let healed_report = apply_fixes(&empty_dir).expect("apply_fixes on absent config.json");
        assert!(healed_report.is_clean());
        assert!(SusiConfig::get_config_path(&empty_dir).is_file());
        assert!(validate_dir(&empty_dir)
            .expect("validate_dir after apply_fixes")
            .is_clean());
        let _ = fs::remove_dir_all(&empty_dir);
    }

    #[test]
    fn validate_settings_on_bundled_heal_defaults_is_clean_with_no_blocking_issues() {
        let report = validate_settings(SusiConfig::heal_defaults());
        assert!(report.is_clean());
        assert!(!report.has_blocking());
        assert!(report.issues.is_empty());
    }
}
