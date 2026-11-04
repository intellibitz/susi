//! Zero-config debt report: every remaining manual input a fresh user must
//! supply, with the task that removes it.

use crate::setting_registry::{SettingDerivation, SETTING_REGISTRY};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DebtKind {
    Secret,
    Consent,
    EnvVar,
    Prompt,
    SetupStep,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtItem {
    pub id: String,
    pub kind: DebtKind,
    pub description: String,
    /// Task id that removes this debt when closed.
    pub removes_via: String,
}

/// Scan setting registry + known env/prompt/setup surfaces for remaining
/// manual inputs. Secrets and consent are listed; derived/constant keys are not.
#[must_use]
pub fn scan_debt(extra: &[DebtItem]) -> Vec<DebtItem> {
    let mut items = Vec::new();
    for entry in SETTING_REGISTRY {
        if matches!(entry.derivation, SettingDerivation::Consent) {
            items.push(DebtItem {
                id: format!("setting:{}", entry.key),
                kind: DebtKind::Consent,
                description: format!("consent required for {}", entry.key),
                removes_via: "T-CLAUDE-160".into(),
            });
        }
    }
    // Known user-facing secrets that are asked just-in-time (never required files).
    items.push(DebtItem {
        id: "secret:cloud_api_key".into(),
        kind: DebtKind::Secret,
        description: "cloud provider API key (prompted at first use)".into(),
        removes_via: "T-CLAUDE-33".into(),
    });
    for e in extra {
        if !items.iter().any(|i| i.id == e.id) {
            items.push(e.clone());
        }
    }
    items.sort_by(|a, b| a.id.cmp(&b.id));
    items
}

/// Format for `susi doctor --zero-config`.
#[must_use]
pub fn format_debt_report(items: &[DebtItem]) -> String {
    if items.is_empty() {
        return "zero-config debt: none\n".into();
    }
    let mut out = format!("zero-config debt: {} item(s)\n", items.len());
    for it in items {
        out.push_str(&format!(
            "- [{}] {} — removes via {}\n",
            format!("{:?}", it.kind).to_ascii_lowercase(),
            it.description,
            it.removes_via
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scan_debt_includes_consent_settings_and_cloud_secret_sorted_and_deduplicated() {
        let base = scan_debt(&[]);
        assert!(base
            .iter()
            .any(|i| i.id == "secret:cloud_api_key" && i.kind == DebtKind::Secret));
        for entry in SETTING_REGISTRY {
            if matches!(entry.derivation, SettingDerivation::Consent) {
                let expected_id = format!("setting:{}", entry.key);
                assert!(base
                    .iter()
                    .any(|i| i.id == expected_id && i.kind == DebtKind::Consent));
            }
        }
        assert!(base.windows(2).all(|w| w[0].id <= w[1].id));

        let extra = vec![
            DebtItem {
                id: "secret:cloud_api_key".into(),
                kind: DebtKind::Secret,
                description: "duplicate should be ignored".into(),
                removes_via: "T-DUP".into(),
            },
            DebtItem {
                id: "env:custom_flag".into(),
                kind: DebtKind::EnvVar,
                description: "custom env flag".into(),
                removes_via: "T-ENV-1".into(),
            },
            DebtItem {
                id: "env:custom_flag".into(),
                kind: DebtKind::EnvVar,
                description: "duplicate extra should be ignored".into(),
                removes_via: "T-ENV-2".into(),
            },
        ];
        let with_extra = scan_debt(&extra);
        assert_eq!(with_extra.len(), base.len() + 1);
        assert!(with_extra.windows(2).all(|w| w[0].id < w[1].id));
        let custom = with_extra
            .iter()
            .find(|i| i.id == "env:custom_flag")
            .expect("custom extra item present");
        assert_eq!(custom.removes_via, "T-ENV-1");
    }

    #[test]
    fn format_debt_report_renders_empty_and_all_debt_kinds_and_roundtrips_serde() {
        assert_eq!(format_debt_report(&[]), "zero-config debt: none\n");

        let items = vec![
            DebtItem {
                id: "a".into(),
                kind: DebtKind::Secret,
                description: "secret desc".into(),
                removes_via: "T-1".into(),
            },
            DebtItem {
                id: "b".into(),
                kind: DebtKind::Consent,
                description: "consent desc".into(),
                removes_via: "T-2".into(),
            },
            DebtItem {
                id: "c".into(),
                kind: DebtKind::EnvVar,
                description: "envvar desc".into(),
                removes_via: "T-3".into(),
            },
            DebtItem {
                id: "d".into(),
                kind: DebtKind::Prompt,
                description: "prompt desc".into(),
                removes_via: "T-4".into(),
            },
            DebtItem {
                id: "e".into(),
                kind: DebtKind::SetupStep,
                description: "setup desc".into(),
                removes_via: "T-5".into(),
            },
        ];
        let report = format_debt_report(&items);
        assert!(report.starts_with("zero-config debt: 5 item(s)\n"));
        assert!(report.contains("- [secret] secret desc — removes via T-1\n"));
        assert!(report.contains("- [consent] consent desc — removes via T-2\n"));
        assert!(report.contains("- [envvar] envvar desc — removes via T-3\n"));
        assert!(report.contains("- [prompt] prompt desc — removes via T-4\n"));
        assert!(report.contains("- [setupstep] setup desc — removes via T-5\n"));

        let json = serde_json::to_string(&items).expect("serialize DebtItems");
        assert!(json.contains("\"setup_step\""));
        assert!(json.contains("\"env_var\""));
        let roundtrip: Vec<DebtItem> = serde_json::from_str(&json).expect("deserialize DebtItems");
        assert_eq!(roundtrip, items);
    }
}
