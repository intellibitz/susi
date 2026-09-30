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
