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
