//! `susi fix` / doctor auto-fix applicator for zero-config safe repairs.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixKind {
    MissingDir,
    StaleLock,
    PortConflict,
    HooksMissing,
    KeyRegistrationPrompt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorFinding {
    pub kind: FixKind,
    pub path: String,
    pub detail: String,
    pub safe: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixReport {
    pub applied: Vec<String>,
    pub skipped: Vec<String>,
}

/// Parse a non-zero lock holder PID from either a plain integer (`"12345"`)
/// or a JSON lock document (`{"pid": 12345}`).
#[must_use]
pub fn parse_lock_pid(raw: &str) -> Option<u32> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(pid) = trimmed.parse::<u32>() {
        return (pid > 0).then_some(pid);
    }
    let val: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let pid = u32::try_from(val.get("pid")?.as_u64()?).ok()?;
    (pid > 0).then_some(pid)
}

/// Scan a home-like root for safe auto-fix opportunities.
#[must_use]
pub fn doctor_scan(root: &Path) -> Vec<DoctorFinding> {
    let mut findings = Vec::new();
    for dir in ["config", "evidence", "brain", "tasks"] {
        let p = root.join(dir);
        if !p.is_dir() {
            findings.push(DoctorFinding {
                kind: FixKind::MissingDir,
                path: p.display().to_string(),
                detail: format!("missing {dir}"),
                safe: true,
            });
        }
    }
    let lock = root.join("daemon.lock");
    if lock.is_file() {
        // Treat zero-length lock as stale (safe to remove in tests).
        if std::fs::metadata(&lock)
            .map(|m| m.len() == 0)
            .unwrap_or(false)
        {
            findings.push(DoctorFinding {
                kind: FixKind::StaleLock,
                path: lock.display().to_string(),
                detail: "empty daemon.lock".into(),
                safe: true,
            });
        } else if let Ok(raw) = std::fs::read_to_string(&lock) {
            if let Some(pid) = parse_lock_pid(&raw) {
                if crate::process_probe::pid_alive(pid) == Some(false) {
                    findings.push(DoctorFinding {
                        kind: FixKind::StaleLock,
                        path: lock.display().to_string(),
                        detail: format!("stale daemon.lock (dead pid {pid})"),
                        safe: true,
                    });
                }
            }
        }
    }
    let hooks = root.join("hooks").join("pre-commit");
    if root.join("hooks").is_dir() && !hooks.is_file() {
        findings.push(DoctorFinding {
            kind: FixKind::HooksMissing,
            path: hooks.display().to_string(),
            detail: "hooks dir without pre-commit".into(),
            safe: true,
        });
    }
    findings
}

/// Apply every safe finding; report each action.
pub fn apply_safe_fixes(root: &Path, findings: &[DoctorFinding]) -> FixReport {
    let mut report = FixReport {
        applied: Vec::new(),
        skipped: Vec::new(),
    };
    for f in findings {
        if !f.safe {
            report.skipped.push(format!("unsafe: {}", f.detail));
            continue;
        }
        match f.kind {
            FixKind::MissingDir => {
                let _ = std::fs::create_dir_all(&f.path);
                report.applied.push(format!("created dir {}", f.path));
            }
            FixKind::StaleLock => {
                let _ = std::fs::remove_file(&f.path);
                report
                    .applied
                    .push(format!("removed stale lock {}", f.path));
            }
            FixKind::HooksMissing => {
                if let Some(parent) = PathBuf::from(&f.path).parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&f.path, "#!/bin/sh\n# installed by susi fix\n");
                report
                    .applied
                    .push(format!("installed hook stub {}", f.path));
            }
            FixKind::PortConflict | FixKind::KeyRegistrationPrompt => {
                report.skipped.push(format!("needs operator: {}", f.detail));
            }
        }
        let _ = root; // reserved for future relative checks
    }
    report
}

/// One-shot doctor + fix (the `susi fix` contract).
pub fn susi_fix(root: &Path) -> FixReport {
    let findings = doctor_scan(root);
    apply_safe_fixes(root, &findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lock_pid_handles_plain_json_zero_and_malformed_inputs() {
        assert_eq!(parse_lock_pid("12345\n"), Some(12345));
        assert_eq!(parse_lock_pid("{\"pid\": 67890}"), Some(67890));
        assert_eq!(parse_lock_pid("0"), None);
        assert_eq!(parse_lock_pid("{\"pid\": 0}"), None);
        assert_eq!(parse_lock_pid(""), None);
        assert_eq!(parse_lock_pid("not-a-pid"), None);
    }

    #[test]
    fn doctor_scan_and_susi_fix_reclaim_empty_and_dead_pid_locks_while_preserving_live_pid() {
        let root = std::env::temp_dir().join(format!(
            "susi-zc-fix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();

        let lock = root.join("daemon.lock");
        std::fs::write(&lock, "").unwrap();
        let report = susi_fix(&root);
        assert!(
            !lock.exists(),
            "empty daemon.lock must be removed by susi_fix"
        );
        assert!(root.join("config").is_dir());
        assert!(root.join("evidence").is_dir());
        assert!(root.join("brain").is_dir());
        assert!(root.join("tasks").is_dir());
        assert!(report
            .applied
            .iter()
            .any(|a| a.contains("removed stale lock")));

        // Dead PID lock (well outside valid OS PID allocation table).
        let dead_pid: u32 = 4_194_304 * 4 + 4;
        if crate::process_probe::pid_alive(dead_pid) == Some(false) {
            std::fs::write(&lock, format!("{{\"pid\": {dead_pid}}}")).unwrap();
            let findings = doctor_scan(&root);
            assert!(
                findings.iter().any(|f| {
                    f.kind == FixKind::StaleLock && f.detail.contains(&dead_pid.to_string())
                }),
                "dead PID daemon.lock must be flagged as StaleLock"
            );
            let _ = susi_fix(&root);
            assert!(
                !lock.exists(),
                "dead PID daemon.lock must be removed by susi_fix"
            );
        }

        // Live PID lock (this test runner's own PID) must never be flagged or removed.
        let our_pid = std::process::id();
        std::fs::write(&lock, format!("{{\"pid\": {our_pid}}}")).unwrap();
        let live_findings = doctor_scan(&root);
        assert!(
            !live_findings.iter().any(|f| f.kind == FixKind::StaleLock),
            "live PID daemon.lock must never be flagged as StaleLock"
        );
        let _ = susi_fix(&root);
        assert!(lock.exists(), "live PID daemon.lock must remain untouched");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn doctor_scan_detects_hooks_missing_and_apply_safe_fixes_installs_stub_and_skips_unsafe_or_operator_items(
    ) {
        let root = std::env::temp_dir().join(format!("susi-zc-fix-hooks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["config", "evidence", "brain", "tasks", "hooks"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }

        let scanned = doctor_scan(&root);
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].kind, FixKind::HooksMissing);
        assert!(scanned[0].safe);

        let mut findings = scanned;
        findings.push(DoctorFinding {
            kind: FixKind::MissingDir,
            path: root.join("untrusted").display().to_string(),
            detail: "manual migration".into(),
            safe: false,
        });
        findings.push(DoctorFinding {
            kind: FixKind::PortConflict,
            path: String::new(),
            detail: "port 8080 busy".into(),
            safe: true,
        });
        findings.push(DoctorFinding {
            kind: FixKind::KeyRegistrationPrompt,
            path: String::new(),
            detail: "missing cloud key".into(),
            safe: true,
        });

        let report = apply_safe_fixes(&root, &findings);
        let hook_path = root.join("hooks").join("pre-commit");
        assert!(hook_path.is_file());
        assert!(std::fs::read_to_string(&hook_path)
            .unwrap()
            .contains("installed by susi fix"));
        assert_eq!(report.applied.len(), 1);
        assert!(report.applied[0].starts_with("installed hook stub "));
        assert_eq!(
            report.skipped,
            vec![
                "unsafe: manual migration".to_string(),
                "needs operator: port 8080 busy".to_string(),
                "needs operator: missing cloud key".to_string(),
            ]
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
