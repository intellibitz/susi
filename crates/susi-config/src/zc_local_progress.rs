//! Local-only progress history of config debt.
//!
//! A small JSON log in the config dir records the debt score each time it
//! changes — no telemetry, nothing leaves the host. `susi doctor
//! --zero-config` renders the trend so progress is visible.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::susi_error::{EaiError, EaiResult};
use crate::zc_debt_report::scan_debt;

const MAX_ENTRIES: usize = 200;

/// One measured debt score.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DebtSnapshot {
    /// Unix seconds when the measurement was taken.
    pub unix: u64,
    /// Required-input count at that moment.
    pub count: usize,
    /// Version/tag string of the susi that measured it.
    #[serde(default)]
    pub version: String,
}

/// The append-only local history.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct DebtHistory {
    #[serde(default)]
    pub entries: Vec<DebtSnapshot>,
}

impl DebtHistory {
    #[must_use]
    pub fn path_in(config_dir: &Path) -> PathBuf {
        config_dir.join("config-debt-history.json")
    }

    /// Load; missing file yields an empty history.
    ///
    /// # Errors
    /// [`EaiError::io`] on unreadable/corrupt files.
    pub fn load(path: &Path) -> EaiResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| EaiError::io(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(EaiError::io(e.to_string())),
        }
    }

    /// # Errors
    /// [`EaiError::io`] on encode/write failures.
    pub fn save(&self, path: &Path) -> EaiResult<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| EaiError::io(e.to_string()))
    }

    /// Record a measurement. Consecutive identical counts collapse (the
    /// timestamp updates instead of duplicating the row).
    pub fn record(&mut self, snap: DebtSnapshot) {
        if let Some(last) = self.entries.last_mut() {
            if last.count == snap.count && last.version == snap.version {
                last.unix = snap.unix;
                return;
            }
        }
        self.entries.push(snap);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.drain(..self.entries.len() - MAX_ENTRIES);
        }
    }

    /// Net change since the first record — negative is progress.
    #[must_use]
    pub fn net_change(&self) -> Option<i64> {
        let first = self.entries.first()?;
        let last = self.entries.last()?;
        Some(last.count as i64 - first.count as i64)
    }

    /// Render a compact history for `susi doctor`.
    #[must_use]
    pub fn render(&self) -> String {
        if self.entries.is_empty() {
            return "config debt: no history yet\n".to_string();
        }
        let mut out = String::from("config debt history:\n");
        for e in &self.entries {
            out.push_str(&format!(
                "  {}: {} required inputs ({})\n",
                e.unix, e.count, e.version
            ));
        }
        if let Some(n) = self.net_change() {
            let arrow = if n < 0 {
                "improved"
            } else if n > 0 {
                "regressed"
            } else {
                "unchanged"
            };
            out.push_str(&format!("  net: {n:+} ({arrow})\n"));
        }
        out
    }
}

/// Measure current debt and append to the local history.
///
/// # Errors
/// [`EaiError::io`] on load/save failures.
pub fn measure_and_record(
    path: &Path,
    version: &str,
    now_unix: u64,
    extra: &[crate::zc_debt_report::DebtItem],
) -> EaiResult<usize> {
    let count = scan_debt(extra).len();
    let mut h = DebtHistory::load(path)?;
    h.record(DebtSnapshot {
        unix: now_unix,
        count,
        version: version.to_string(),
    });
    h.save(path)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zc_debt_report::DebtItem;

    fn snap(unix: u64, count: usize) -> DebtSnapshot {
        DebtSnapshot {
            unix,
            count,
            version: "0.19".to_string(),
        }
    }

    #[test]
    fn zc_local_progress_collapses_repeats() {
        let mut h = DebtHistory::default();
        h.record(snap(1, 5));
        h.record(snap(2, 5));
        h.record(snap(3, 4));
        assert_eq!(h.entries.len(), 2);
        assert_eq!(h.entries[0].unix, 2); // timestamp updated
    }

    #[test]
    fn zc_local_progress_net_change_tracks_direction() {
        let mut h = DebtHistory::default();
        h.record(snap(1, 8));
        h.record(snap(2, 5));
        h.record(snap(3, 3));
        assert_eq!(h.net_change(), Some(-5));
        assert!(h.render().contains("improved"));
        assert!(DebtHistory::default().net_change().is_none());
    }

    #[test]
    fn zc_local_progress_bounded() {
        let mut h = DebtHistory::default();
        for i in 0..250u64 {
            h.record(DebtSnapshot {
                unix: i,
                count: i as usize,
                version: "x".to_string(),
            });
        }
        assert_eq!(h.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn zc_local_progress_persist_roundtrip() {
        let dir = std::env::temp_dir().join(format!("debtprog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = DebtHistory::path_in(&dir);
        let mut h = DebtHistory::default();
        h.record(snap(7, 4));
        h.save(&path).unwrap();
        assert_eq!(DebtHistory::load(&path).unwrap().entries.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn zc_local_progress_measure_writes_current_count() {
        let dir = std::env::temp_dir().join(format!("debtmeas-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = DebtHistory::path_in(&dir);
        let extra = vec![DebtItem {
            id: "secret:test".to_string(),
            kind: crate::zc_debt_report::DebtKind::Secret,
            description: "x".to_string(),
            removes_via: "T-X".to_string(),
        }];
        let n = measure_and_record(&path, "0.19", 1, &extra).unwrap();
        assert_eq!(n, scan_debt(&extra).len());
        let h = DebtHistory::load(&path).unwrap();
        assert_eq!(h.entries.last().map(|e| e.count), Some(n));
        std::fs::remove_dir_all(&dir).ok();
    }
}
