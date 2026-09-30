//! Config-debt ratchet: the count of required inputs may never rise.
//!
//! `zc_debt_report::scan_debt` computes the remaining manual inputs; this
//! module pins that count in `.agents/config-debt-baseline.json` (like the
//! coverage ratchet). Lowering the baseline is always allowed — commit the
//! improvement with the change that earned it. Raising it fails CI.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::susi_error::{EaiError, EaiResult};
use crate::zc_debt_report::{scan_debt, DebtItem};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtBaseline {
    /// Maximum allowed count of required inputs.
    pub max_required: usize,
    /// Which debt ids were present when the baseline was pinned — lets the
    /// report say *which* inputs are new, not just that the count grew.
    #[serde(default)]
    pub known_ids: Vec<String>,
}

impl DebtBaseline {
    #[must_use]
    pub fn from_scan(extra: &[DebtItem]) -> Self {
        let items = scan_debt(extra);
        Self {
            max_required: items.len(),
            known_ids: items.iter().map(|i| i.id.clone()).collect(),
        }
    }

    /// # Errors
    /// [`EaiError::io`] on unreadable/corrupt files.
    pub fn load(path: &Path) -> EaiResult<Self> {
        let text = std::fs::read_to_string(path).map_err(|e| EaiError::io(e.to_string()))?;
        serde_json::from_str(&text).map_err(|e| EaiError::io(e.to_string()))
    }

    /// # Errors
    /// [`EaiError::io`] on encode/write failures.
    pub fn save(&self, path: &Path) -> EaiResult<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| EaiError::io(e.to_string()))
    }
}

/// What the ratchet check concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RatchetVerdict {
    /// Count below the baseline — caller should lower it and commit.
    Improved { now: usize, was: usize },
    /// At the baseline — pass.
    Holds { count: usize },
    /// Above the baseline — fail; `new_ids` are the offending additions
    /// (empty when the count grew only by id renames).
    Regressed {
        now: usize,
        max: usize,
        new_ids: Vec<String>,
    },
}

/// Compare the live debt scan against `baseline`.
#[must_use]
pub fn check(baseline: &DebtBaseline, extra: &[DebtItem]) -> RatchetVerdict {
    let items = scan_debt(extra);
    let now = items.len();
    if now < baseline.max_required {
        return RatchetVerdict::Improved {
            now,
            was: baseline.max_required,
        };
    }
    if now == baseline.max_required {
        return RatchetVerdict::Holds { count: now };
    }
    let new_ids: Vec<String> = items
        .iter()
        .map(|i| i.id.clone())
        .filter(|id| !baseline.known_ids.contains(id))
        .collect();
    RatchetVerdict::Regressed {
        now,
        max: baseline.max_required,
        new_ids,
    }
}

/// The check CI/`susi doctor` runs: load the baseline and evaluate. A
/// missing baseline file is a failure to pin, not a pass.
///
/// # Errors
/// [`EaiError::io`] when the baseline file is absent or corrupt.
pub fn ratchet_check(baseline_path: &Path, extra: &[DebtItem]) -> EaiResult<RatchetVerdict> {
    let baseline = DebtBaseline::load(baseline_path)?;
    Ok(check(&baseline, extra))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zc_debt_report::{DebtItem, DebtKind};

    fn item(id: &str) -> DebtItem {
        DebtItem {
            id: id.to_string(),
            kind: DebtKind::Secret,
            description: "x".to_string(),
            removes_via: "T".to_string(),
        }
    }

    #[test]
    fn zc_debt_ratchet_holds_at_baseline() {
        let extra = vec![item("a"), item("b")];
        let base = DebtBaseline::from_scan(&extra);
        match check(&base, &extra) {
            RatchetVerdict::Holds { count } => assert_eq!(count, base.max_required),
            other => panic!("expected Holds, got {other:?}"),
        }
    }

    #[test]
    fn zc_debt_ratchet_improved_below_baseline() {
        let mut base = DebtBaseline::from_scan(&[item("a"), item("b")]);
        base.max_required += 2; // pretend baseline was higher
        match check(&base, &[item("a"), item("b")]) {
            RatchetVerdict::Improved { now, was } => {
                assert_eq!(now + 2, was);
            }
            other => panic!("expected Improved, got {other:?}"),
        }
    }

    #[test]
    fn zc_debt_ratchet_regresses_and_names_new_ids() {
        let base = DebtBaseline::from_scan(&[item("a")]);
        match check(&base, &[item("a"), item("b"), item("c"), item("d")]) {
            RatchetVerdict::Regressed { now, max, new_ids } => {
                assert!(now > max);
                assert_eq!(new_ids, vec!["b", "c", "d"]);
            }
            other => panic!("expected Regressed, got {other:?}"),
        }
    }

    #[test]
    fn zc_debt_ratchet_baseline_roundtrips() {
        let dir = std::env::temp_dir().join(format!("debtrat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("baseline.json");
        let base = DebtBaseline::from_scan(&[item("a")]);
        base.save(&path).unwrap();
        let loaded = DebtBaseline::load(&path).unwrap();
        assert_eq!(loaded, base);
        assert!(ratchet_check(&dir.join("absent.json"), &[]).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
