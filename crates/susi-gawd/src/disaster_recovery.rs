//! Disaster recovery from authoritative backups (VC-201-093).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupBundle {
    pub config: String,
    pub workflow_state: String,
    pub keys: BTreeMap<String, String>,
    pub ledger_tail: String,
    pub audit_anchor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReport {
    pub restored: BackupBundle,
    pub audit_continuous: bool,
    pub recovery_ms: u64,
    pub data_loss_records: u64,
    pub within_rto: bool,
    pub within_rpo: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrTargets {
    pub rto_ms: u64,
    pub rpo_records: u64,
    pub allow_key_restore: bool,
}

/// Restore to an isolated node; keys only under explicit policy.
#[must_use]
pub fn restore_from_backup(
    backup: &BackupBundle,
    targets: &DrTargets,
    measured_recovery_ms: u64,
    lost_records: u64,
) -> RestoreReport {
    let mut restored = backup.clone();
    if !targets.allow_key_restore {
        restored.keys.clear();
    }
    RestoreReport {
        audit_continuous: restored.audit_anchor == backup.audit_anchor
            && !restored.ledger_tail.is_empty(),
        within_rto: measured_recovery_ms <= targets.rto_ms,
        within_rpo: lost_records <= targets.rpo_records,
        recovery_ms: measured_recovery_ms,
        data_loss_records: lost_records,
        restored,
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    fn sample_backup() -> BackupBundle {
        BackupBundle {
            config: "cfg-v1".to_string(),
            workflow_state: "running".to_string(),
            keys: BTreeMap::from([("openai".to_string(), "sk-test".to_string())]),
            ledger_tail: "rec-99".to_string(),
            audit_anchor: "sha256:anchor".to_string(),
        }
    }

    #[test]
    fn restore_strips_keys_when_key_restore_is_disallowed() {
        let backup = sample_backup();
        let targets = DrTargets {
            rto_ms: 1_000,
            rpo_records: 5,
            allow_key_restore: false,
        };
        let report = restore_from_backup(&backup, &targets, 400, 0);
        assert!(report.restored.keys.is_empty());
        assert_eq!(report.restored.config, "cfg-v1");
        assert!(report.audit_continuous);
    }

    #[test]
    fn restore_preserves_keys_when_explicitly_allowed_by_policy() {
        let backup = sample_backup();
        let targets = DrTargets {
            rto_ms: 1_000,
            rpo_records: 5,
            allow_key_restore: true,
        };
        let report = restore_from_backup(&backup, &targets, 500, 2);
        assert_eq!(
            report.restored.keys.get("openai").map(String::as_str),
            Some("sk-test")
        );
        assert!(report.within_rto);
        assert!(report.within_rpo);
    }

    #[test]
    fn restore_flags_rto_and_rpo_breaches_when_limits_exceeded() {
        let backup = sample_backup();
        let targets = DrTargets {
            rto_ms: 200,
            rpo_records: 1,
            allow_key_restore: false,
        };
        let report = restore_from_backup(&backup, &targets, 250, 2);
        assert!(!report.within_rto);
        assert!(!report.within_rpo);
        assert_eq!(report.recovery_ms, 250);
        assert_eq!(report.data_loss_records, 2);
    }

    #[test]
    fn empty_ledger_tail_marks_audit_continuity_false() {
        let mut backup = sample_backup();
        backup.ledger_tail.clear();
        let targets = DrTargets {
            rto_ms: 1_000,
            rpo_records: 5,
            allow_key_restore: true,
        };
        let report = restore_from_backup(&backup, &targets, 100, 0);
        assert!(!report.audit_continuous);
    }

    #[test]
    fn restore_report_and_dr_targets_serde_json_roundtrip_and_exact_rto_rpo_boundaries() {
        let backup = sample_backup();
        let targets = DrTargets {
            rto_ms: 250,
            rpo_records: 2,
            allow_key_restore: true,
        };
        let report = restore_from_backup(&backup, &targets, 250, 2);
        assert!(report.within_rto);
        assert!(report.within_rpo);
        let encoded = serde_json::to_string(&report).unwrap();
        let decoded: RestoreReport = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, report);
    }
}
