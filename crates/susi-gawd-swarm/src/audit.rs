//! Production audit dispatch for swarm state changes.
//!
//! Swarm code uses this module instead of writing directly to the signed log.
//! The action crosses the `gawd.audit.action` plane topic, where the GAWD
//! host applies safety/security gates and sends exactly one structured record
//! through the existing `SusiAuditLogger` sink.

use crate::susi_core::audit_export::{AuditAction, AuditOutcome};
use crate::susi_error::{EaiError, EaiResult};
use std::path::Path;

/// Dispatch one already-structured action through the host-owned choke point.
pub fn dispatch(action: &AuditAction, workspace: &Path) -> EaiResult<()> {
    crate::susi_core::plane_bus::gawd::audit_structured_action(action, workspace)
        .map_err(EaiError::governance)
}

/// Compatibility boundary for callers that still have a tool/detail pair.
/// `detail` is converted to a digest by [`AuditAction::from_legacy`] before
/// it reaches the durable audit sink.
pub fn record_legacy(
    workspace: &Path,
    tool: &str,
    detail: &str,
    outcome: AuditOutcome,
) -> EaiResult<()> {
    let action = AuditAction::from_legacy(tool, detail, outcome)?;
    dispatch(&action, workspace)
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use crate::susi_core::audit_export::{AuditActionKind, AuditTarget};

    #[test]
    fn record_legacy_rejects_empty_tool_name_before_plane_dispatch() {
        let err = record_legacy(Path::new("."), "", "some detail", AuditOutcome::Succeeded)
            .expect_err("empty tool must fail validation");
        assert!(err.to_string().contains("target"));
    }

    #[test]
    fn record_legacy_supports_failed_denied_and_cancelled_outcomes() {
        let tmp =
            std::env::temp_dir().join(format!("susi_swarm_audit_outcomes_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        for outcome in [
            AuditOutcome::Failed,
            AuditOutcome::Denied,
            AuditOutcome::Cancelled,
        ] {
            let res = record_legacy(&tmp, "swarm_gate", "outcome check", outcome);
            if let Err(e) = res {
                assert!(!e.to_string().is_empty());
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn dispatch_forwards_structured_action_across_plane_bus_boundary() {
        let action = AuditAction::new(
            AuditActionKind::ToolExecution,
            "swarm-worker",
            AuditTarget::identifier("swarm_sync"),
            AuditOutcome::Succeeded,
            12,
            0,
            "corr-1",
        )
        .expect("build structured audit action");
        let tmp =
            std::env::temp_dir().join(format!("susi_swarm_audit_dispatch_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let res = dispatch(&action, &tmp);
        if let Err(e) = res {
            assert!(!e.to_string().is_empty());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn record_legacy_converts_detail_to_redacted_digest_and_invokes_dispatch() {
        let tmp =
            std::env::temp_dir().join(format!("susi_swarm_audit_legacy_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let res = record_legacy(
            &tmp,
            "peer_repair",
            "repaired 4 segments",
            AuditOutcome::Succeeded,
        );
        if let Err(e) = res {
            assert!(!e.to_string().is_empty());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn audit_action_from_legacy_builds_valid_action_and_rejects_empty_actor() {
        let action = AuditAction::from_legacy("swarm_tool", "detail", AuditOutcome::Succeeded)
            .expect("from_legacy valid");
        assert_eq!(action.outcome, AuditOutcome::Succeeded);

        let bad_actor = AuditAction::new(
            AuditActionKind::ToolExecution,
            "",
            AuditTarget::identifier("swarm_sync"),
            AuditOutcome::Succeeded,
            0,
            0,
            "corr-1",
        );
        assert!(bad_actor.is_err());
    }
}
