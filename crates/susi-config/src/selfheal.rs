//! Startup self-heal: a bad `config.json` never blocks the substrate.
//!
//! Invalid JSON or a file that cannot be parsed is timestamp-backed-up,
//! replaced with healed bundled defaults, and the operator is told what
//! happened. Unknown/deprecated keys are stripped via [`crate::validate::apply_fixes`].

use crate::json_util::atomic_write_json_pretty;
use crate::susi_error::{EaiError, EaiResult};
use crate::validate::{apply_fixes, validate_settings, ConfigIssueKind};
use crate::SusiConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// What self-heal did (and why).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelfHealReport {
    pub repaired: bool,
    pub backup_path: Option<String>,
    pub messages: Vec<String>,
}

impl SelfHealReport {
    #[must_use]
    pub fn render(&self) -> String {
        if !self.repaired && self.messages.is_empty() {
            return "config self-heal: ok (no repair needed)".to_string();
        }
        let mut out = String::from("config self-heal:\n");
        for m in &self.messages {
            out.push_str("  - ");
            out.push_str(m);
            out.push('\n');
        }
        if let Some(b) = &self.backup_path {
            out.push_str("  backup: ");
            out.push_str(b);
            out.push('\n');
        }
        out
    }
}

fn stamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn backup_path(config_path: &Path) -> PathBuf {
    let parent = config_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("config.json.bak-{}", stamp()))
}

/// Load `global_dir/config.json`, self-healing on parse failure or
/// unknown/deprecated keys. Always returns a usable [`SusiConfig`].
pub fn load_or_selfheal(global_dir: &Path) -> EaiResult<(SusiConfig, SelfHealReport)> {
    let path = SusiConfig::get_config_path(global_dir);
    let mut report = SelfHealReport::default();

    if !path.exists() {
        let cfg = SusiConfig::default();
        report
            .messages
            .push("no config.json; using bundled defaults (zero-config)".to_string());
        return Ok((cfg, report));
    }

    let raw = fs::read_to_string(&path).map_err(|e| EaiError::config(e.to_string()))?;
    match serde_json::from_str::<SusiConfig>(&raw) {
        Ok(cfg) => {
            let validation = validate_settings(&cfg.settings);
            let needs_strip = validation.issues.iter().any(|i| {
                matches!(
                    i.kind,
                    ConfigIssueKind::Unknown | ConfigIssueKind::Deprecated
                )
            });
            if !needs_strip {
                return Ok((cfg, report));
            }
            // Backup before mutating.
            let bak = backup_path(&path);
            fs::write(&bak, &raw).map_err(|e| EaiError::config(e.to_string()))?;
            report.backup_path = Some(bak.display().to_string());
            report.repaired = true;
            for issue in &validation.issues {
                if matches!(
                    issue.kind,
                    ConfigIssueKind::Unknown | ConfigIssueKind::Deprecated
                ) {
                    report.messages.push(format!(
                        "{:?} `{}`: {}",
                        issue.kind, issue.path, issue.message
                    ));
                }
            }
            let after = apply_fixes(global_dir)?;
            // apply_fixes leaves unknown keys; strip them explicitly.
            let mut healed = SusiConfig::load(global_dir)?;
            let unknown: Vec<String> = after
                .issues
                .iter()
                .filter(|i| i.kind == ConfigIssueKind::Unknown)
                .map(|i| i.path.clone())
                .collect();
            for path_key in &unknown {
                // Only top-level unknown keys are stripped here.
                if !path_key.contains('.') {
                    healed.settings.remove(path_key);
                    report
                        .messages
                        .push(format!("removed unknown key `{path_key}`"));
                }
            }
            healed.save(global_dir)?;
            Ok((healed, report))
        }
        Err(e) => {
            let bak = backup_path(&path);
            fs::write(&bak, &raw).map_err(|io| EaiError::config(io.to_string()))?;
            report.backup_path = Some(bak.display().to_string());
            report.repaired = true;
            report
                .messages
                .push(format!("invalid JSON ({e}); restored bundled defaults"));
            let cfg = SusiConfig::default();
            fs::create_dir_all(global_dir).map_err(|io| EaiError::config(io.to_string()))?;
            atomic_write_json_pretty(&path, &cfg)?;
            Ok((cfg, report))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "susi_selfheal_{tag}_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn render_and_empty_dir_selfheal_report_zero_config_and_repair_details() {
        let clean = SelfHealReport::default();
        assert_eq!(clean.render(), "config self-heal: ok (no repair needed)");

        let repaired_report = SelfHealReport {
            repaired: true,
            backup_path: Some("/tmp/config.json.bak-1".into()),
            messages: vec!["removed unknown key `x`".into()],
        };
        let rendered = repaired_report.render();
        assert!(rendered.contains("  - removed unknown key `x`\n"));
        assert!(rendered.contains("  backup: /tmp/config.json.bak-1\n"));

        let dir = temp_dir("empty");
        let (cfg, report) = load_or_selfheal(&dir).expect("empty dir succeeds");
        assert!(!report.repaired);
        assert!(report.backup_path.is_none());
        assert!(report.messages.iter().any(|m| m.contains("zero-config")));
        assert!(!cfg.settings.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_and_unknown_key_configs_are_backed_up_and_self_healed() {
        // 1. Corrupt JSON is backed up and replaced with bundled defaults.
        let dir_corrupt = temp_dir("corrupt");
        let cfg_path = SusiConfig::get_config_path(&dir_corrupt);
        fs::write(&cfg_path, "{not valid json").unwrap();

        let (healed_cfg, report) = load_or_selfheal(&dir_corrupt).expect("corrupt JSON healed");
        assert!(report.repaired);
        let bak_path = PathBuf::from(report.backup_path.as_ref().expect("backup path set"));
        assert_eq!(fs::read_to_string(&bak_path).unwrap(), "{not valid json");
        assert!(!healed_cfg.settings.is_empty());
        assert!(SusiConfig::load(&dir_corrupt).is_ok());
        let _ = fs::remove_dir_all(&dir_corrupt);

        // 2. Unknown top-level key is backed up and stripped from memory & disk.
        let dir_unknown = temp_dir("unknown");
        let mut seeded = SusiConfig::default();
        seeded
            .settings
            .insert("totally_unknown_future_key".into(), serde_json::json!(42));
        seeded.save(&dir_unknown).unwrap();

        let (stripped_cfg, report2) =
            load_or_selfheal(&dir_unknown).expect("unknown key self-healed");
        assert!(report2.repaired);
        assert!(report2.backup_path.is_some());
        assert!(!stripped_cfg
            .settings
            .contains_key("totally_unknown_future_key"));
        assert!(report2
            .messages
            .iter()
            .any(|m| m == "removed unknown key `totally_unknown_future_key`"));

        let reloaded = SusiConfig::load(&dir_unknown).unwrap();
        assert!(!reloaded.settings.contains_key("totally_unknown_future_key"));
        let _ = fs::remove_dir_all(&dir_unknown);
    }

    #[test]
    fn clean_config_requires_no_repair_and_deprecated_keys_trigger_backup_and_strip() {
        // 1. Clean, already-valid config.json returns repaired == false and no backup file.
        let dir_clean = temp_dir("clean-valid");
        let clean_cfg = SusiConfig {
            settings: SusiConfig::heal_defaults().clone(),
        };
        clean_cfg.save(&dir_clean).unwrap();

        let (_, clean_report) = load_or_selfheal(&dir_clean).expect("clean config loads");
        assert!(!clean_report.repaired);
        assert!(clean_report.backup_path.is_none());
        assert!(clean_report.messages.is_empty());
        let has_bak = fs::read_dir(&dir_clean)
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().contains(".bak-"));
        assert!(!has_bak, "clean config must not write a .bak file");
        let _ = fs::remove_dir_all(&dir_clean);

        // 2. Deprecated top-level keys trigger backup, repair, and removal from saved config.json.
        let dir_dep = temp_dir("deprecated-keys");
        let mut dep_map = SusiConfig::heal_defaults().clone();
        dep_map.insert("gmcp_port".into(), serde_json::json!(9090));
        dep_map.insert("api_auth_token".into(), serde_json::json!("tok"));
        fs::write(
            SusiConfig::get_config_path(&dir_dep),
            serde_json::to_string_pretty(&SusiConfig { settings: dep_map }).unwrap(),
        )
        .unwrap();

        let (healed_cfg, dep_report) =
            load_or_selfheal(&dir_dep).expect("deprecated keys self-healed");
        assert!(dep_report.repaired);
        let bak_path = PathBuf::from(dep_report.backup_path.as_ref().expect("backup created"));
        assert!(bak_path.is_file());
        assert!(!healed_cfg.settings.contains_key("gmcp_port"));
        assert!(!healed_cfg.settings.contains_key("api_auth_token"));

        let saved_raw = fs::read_to_string(SusiConfig::get_config_path(&dir_dep)).unwrap();
        let saved_val: serde_json::Value = serde_json::from_str(&saved_raw).unwrap();
        assert!(saved_val.get("gmcp_port").is_none());
        assert!(saved_val.get("api_auth_token").is_none());
        let _ = fs::remove_dir_all(&dir_dep);
    }

    #[test]
    fn selfheal_report_serde_roundtrip_and_render_with_messages_without_backup() {
        let info_report = SelfHealReport {
            repaired: false,
            backup_path: None,
            messages: vec![
                "no config.json; using bundled defaults (zero-config)".into(),
                "verified default schema".into(),
            ],
        };
        let rendered = info_report.render();
        assert!(rendered.starts_with("config self-heal:\n"));
        assert!(rendered.contains("  - no config.json; using bundled defaults (zero-config)\n"));
        assert!(rendered.contains("  - verified default schema\n"));
        assert!(!rendered.contains("backup:"));

        let json = serde_json::to_string(&info_report).unwrap();
        let roundtrip: SelfHealReport = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip, info_report);
    }
}
