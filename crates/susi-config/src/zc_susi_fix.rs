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
