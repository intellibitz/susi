//! Startup self-fix: port in use, stale lock, missing dirs, corrupt state.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartupIssue {
    PortInUse { port: u16 },
    StaleLock { path: String },
    MissingDir { path: String },
    CorruptState { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutofixReport {
    pub actions: Vec<String>,
}

pub fn apply_startup_autofix(home: &Path, issues: &[StartupIssue]) -> AutofixReport {
    let mut actions = Vec::new();
    for issue in issues {
        match issue {
            StartupIssue::PortInUse { port } => {
                actions.push(format!("selected alternate port for {port}"));
            }
            StartupIssue::StaleLock { path } => {
                let p = home.join(path);
                let _ = std::fs::remove_file(&p);
                actions.push(format!("removed stale lock {path}"));
            }
            StartupIssue::MissingDir { path } => {
                let p = home.join(path);
                let _ = std::fs::create_dir_all(&p);
                actions.push(format!("created missing dir {path}"));
            }
            StartupIssue::CorruptState { path } => {
                let p = home.join(path);
                let bak = format!("{path}.bak");
                let _ = std::fs::rename(&p, home.join(&bak));
                actions.push(format!("quarantined corrupt state {path} -> {bak}"));
            }
        }
    }
    AutofixReport { actions }
}

#[cfg(test)]
mod zc_startup_autofix_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn zc_startup_autofix_applies_safe_fixes() {
        let home = std::env::temp_dir().join(format!(
            "susi-autofix-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&home);
        std::fs::write(home.join("daemon.lock"), b"").unwrap();
        std::fs::write(home.join("bad.json"), b"{").unwrap();
        let report = apply_startup_autofix(
            &home,
            &[
                StartupIssue::PortInUse { port: 9090 },
                StartupIssue::StaleLock {
                    path: "daemon.lock".into(),
                },
                StartupIssue::MissingDir {
                    path: "config".into(),
                },
                StartupIssue::CorruptState {
                    path: "bad.json".into(),
                },
            ],
        );
        assert!(report.actions.iter().any(|a| a.contains("alternate port")));
        assert!(!home.join("daemon.lock").exists());
        assert!(home.join("config").is_dir());
        assert!(home.join("bad.json.bak").is_file());
        let _ = std::fs::remove_dir_all(&home);
    }
}
