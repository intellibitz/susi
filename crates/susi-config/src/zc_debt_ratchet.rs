//! Config-debt ratchet: required-input count may never rise.

use crate::zc_debt_report::{scan_debt, DebtItem};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Pinned baseline for the zero-config debt count (like coverage floors).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtBaseline {
    pub max_count: u32,
}

impl DebtBaseline {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Self {
                max_count: u32::MAX,
            })
    }

    /// # Errors
    /// Returns err when current debt exceeds the pinned baseline.
    pub fn check(&self, items: &[DebtItem]) -> Result<(), String> {
        let n = items.len() as u32;
        if n > self.max_count {
            return Err(format!(
                "zero-config debt rose: {n} > baseline {}",
                self.max_count
            ));
        }
        Ok(())
    }

    /// Persist a new floor (only when debt dropped).
    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

/// Scan current debt and compare against a baseline file.
pub fn ratchet(baseline_path: &Path, extra: &[DebtItem]) -> Result<u32, String> {
    let items = scan_debt(extra);
    let baseline = DebtBaseline::load(baseline_path);
    baseline.check(&items)?;
    Ok(items.len() as u32)
}

#[cfg(test)]
mod zc_debt_ratchet_tests {
    use super::*;
    use crate::zc_debt_report::DebtKind;

    #[test]
    fn zc_debt_ratchet_rejects_rise() {
        let dir = std::env::temp_dir().join(format!("susi_debt_ratchet_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("baseline.json");
        DebtBaseline { max_count: 2 }.save(&path).unwrap();
        let extra = vec![
            DebtItem {
                id: "a".into(),
                kind: DebtKind::EnvVar,
                description: "a".into(),
                removes_via: "T-X".into(),
            },
            DebtItem {
                id: "b".into(),
                kind: DebtKind::EnvVar,
                description: "b".into(),
                removes_via: "T-X".into(),
            },
            DebtItem {
                id: "c".into(),
                kind: DebtKind::EnvVar,
                description: "c".into(),
                removes_via: "T-X".into(),
            },
        ];
        // scan_debt always adds registry + cloud key; force check via baseline only
        let baseline = DebtBaseline::load(&path);
        assert!(baseline.check(&extra[..2]).is_ok());
        assert!(baseline.check(&extra).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn zc_debt_ratchet_allows_equal_or_lower() {
        let b = DebtBaseline { max_count: 5 };
        assert!(b.check(&[]).is_ok());
    }
}
