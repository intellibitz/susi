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
