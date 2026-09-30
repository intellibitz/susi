//! Local-only progress history of config debt (no telemetry).

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressEntry {
    pub unix_secs: u64,
    pub debt_count: u32,
    pub release: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressLog {
    pub entries: Vec<ProgressEntry>,
}

impl ProgressLog {
    #[must_use]
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn record(&mut self, unix_secs: u64, debt_count: u32, release: &str) {
        self.entries.push(ProgressEntry {
            unix_secs,
            debt_count,
            release: release.into(),
        });
        // Keep a small local history.
        if self.entries.len() > 64 {
            let skip = self.entries.len() - 64;
            self.entries.drain(..skip);
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }

    #[must_use]
    pub fn latest_debt(&self) -> Option<u32> {
        self.entries.last().map(|e| e.debt_count)
    }
}

#[cfg(test)]
mod zc_local_progress_tests {
    use super::*;

    #[test]
    fn zc_local_progress_records_and_bounds_history() {
        let dir = std::env::temp_dir().join(format!("susi_zc_progress_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("debt-progress.json");
        let mut log = ProgressLog::default();
        for i in 0..70u32 {
            log.record(u64::from(i), 10 - (i % 5), "0.19.0");
        }
        assert_eq!(log.entries.len(), 64);
        log.save(&path).unwrap();
        let loaded = ProgressLog::load(&path);
        assert_eq!(loaded.entries.len(), 64);
        assert!(loaded.latest_debt().is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
