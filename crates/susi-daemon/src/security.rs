//! Capability‑Based Security Model (Swarm OS Vision – Bullet 4).
//!
//! Every cell declares required capabilities in its manifest. The kernel
//! enforces a deny‑by‑default policy: a syscall is only dispatched when the
//! calling cell possesses the appropriate capability token.
//!
//! Tokens are modelled as opaque strings that can be:
//! - **Static**: baked into the manifest at registration time.
//! - **Dynamic**: granted at runtime via the blackboard's `ConsensusVote`
//!   mechanism after multi‑agent approval (Bullet 53).
//!
//! The [`CapabilityPolicy`] struct evaluates a [`SyscallRequest`] against a
//! registered cell's manifest and returns `Allow` or `Deny`.

use serde::{Deserialize, Serialize};
use susi_abi::swarm::SwarmCellManifest;
use susi_abi::syscall::{SyscallOp, SyscallRequest};

// ──────────────────────────────────────────────────────────
// Policy types
// ──────────────────────────────────────────────────────────

/// A single capability grant attached to a cell or role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityGrant {
    /// The capability name, e.g. `"infer"`, `"tool:exec_command"`, `"blackboard:write"`.
    pub capability: String,
    /// Optional scope constraint (e.g. a workspace path or topic prefix).
    pub scope: Option<String>,
    /// Whether this grant is permanent or one‑shot.
    pub ephemeral: bool,
}

/// Result of evaluating a syscall against the security policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyVerdict {
    /// The call is authorised.
    Allow,
    /// The call is denied — reason attached to the syscall response.
    Deny,
}

/// Deny‑by‑default capability policy evaluator.
///
/// Holds the set of grants for a single cell and evaluates incoming
/// syscalls against them.
#[derive(Debug, Clone)]
pub struct CapabilityPolicy {
    /// Cell identity this policy belongs to.
    cell_id: String,
    /// Granted capabilities.
    grants: Vec<CapabilityGrant>,
}

impl CapabilityPolicy {
    /// Creates a new policy for the given cell with an initial set of grants.
    pub fn new(cell_id: impl Into<String>, grants: Vec<CapabilityGrant>) -> Self {
        Self {
            cell_id: cell_id.into(),
            grants,
        }
    }

    /// Adds a capability grant at runtime (e.g. after consensus approval).
    pub fn grant(&mut self, g: CapabilityGrant) {
        if !self.grants.iter().any(|existing| existing.capability == g.capability && existing.scope == g.scope) {
            self.grants.push(g);
        }
    }

    /// Revokes a previously granted capability.
    pub fn revoke(&mut self, capability: &str) {
        self.grants.retain(|g| g.capability != capability);
    }

    /// Evaluates whether a syscall request is allowed under this policy.
    ///
    /// The mapping from [`SyscallOp`] to required capability strings is
    /// intentionally simple and extensible:
    ///
    /// | Op                | Required capability          |
    /// |-------------------|------------------------------|
    /// | `Infer`           | `"infer"`                    |
    /// | `ToolCall`        | `"tool:*"` or specific name  |
    /// | `BlackboardPost`  | `"blackboard:write"`         |
    /// | `BlackboardRead`  | `"blackboard:read"`          |
    /// | `ConsensusVote`   | `"consensus:vote"`           |
    /// | `AuditSeal`       | `"audit:seal"`               |
    /// | `Heartbeat`       | always allowed               |
    /// | `TelemetryGet`    | `"telemetry:read"`           |
    /// | Others            | capability matching op name  |
    pub fn evaluate(&self, req: &SyscallRequest) -> PolicyVerdict {
        // Heartbeat is always allowed — every cell must be able to report liveness.
        if req.op == SyscallOp::Heartbeat {
            return PolicyVerdict::Allow;
        }

        let required = required_capability_for_op(req.op);

        // Check if any grant satisfies the requirement.
        let authorised = self.grants.iter().any(|g| {
            // Exact match
            if g.capability == required {
                return scope_matches(g, req);
            }
            // Wildcard match for tool:* style grants
            if g.capability
                .strip_suffix('*')
                .is_some_and(|prefix| required.starts_with(prefix))
            {
                return scope_matches(g, req);
            }
            false
        });

        if authorised {
            PolicyVerdict::Allow
        } else {
            PolicyVerdict::Deny
        }
    }

    /// Returns the cell ID this policy governs.
    #[must_use]
    pub fn cell_id(&self) -> &str {
        &self.cell_id
    }

    /// Returns the current grants (read-only).
    #[must_use]
    pub fn grants(&self) -> &[CapabilityGrant] {
        &self.grants
    }
}

// ──────────────────────────────────────────────────────────
// Helpers
// ──────────────────────────────────────────────────────────

/// Maps a syscall operation to the capability string required to execute it.
fn required_capability_for_op(op: SyscallOp) -> &'static str {
    match op {
        SyscallOp::Infer => "infer",
        SyscallOp::ToolCall => "tool:*",
        SyscallOp::ContextAlloc => "context:alloc",
        SyscallOp::BlackboardPost => "blackboard:write",
        SyscallOp::BlackboardRead => "blackboard:read",
        SyscallOp::ConsensusVote => "consensus:vote",
        SyscallOp::ReflexMount => "reflex:mount",
        SyscallOp::AuditSeal => "audit:seal",
        SyscallOp::Heartbeat => "heartbeat", // unreachable in evaluate(), listed for exhaustiveness
        SyscallOp::TelemetryGet => "telemetry:read",
    }
}

/// Checks scope constraints: if the grant has a scope, the request's
/// workspace must start with that scope prefix.
fn scope_matches(grant: &CapabilityGrant, req: &SyscallRequest) -> bool {
    match (&grant.scope, &req.workspace) {
        (Some(scope), Some(workspace)) => workspace.starts_with(scope.as_str()),
        (Some(_), None) => false, // scoped grant but no workspace in request
        (None, _) => true,       // unscoped grant — matches everything
    }
}

/// Build a default capability policy from a [`SwarmCellManifest`].
///
/// Converts the manifest's string capabilities into [`CapabilityGrant`]s.
/// This is the bridge between the ABI‑level manifest and the daemon's
/// security enforcement layer.
pub fn policy_from_manifest(manifest: &SwarmCellManifest) -> CapabilityPolicy {
    let grants = manifest
        .capabilities
        .iter()
        .map(|cap| CapabilityGrant {
            capability: cap.clone(),
            scope: None,
            ephemeral: false,
        })
        .collect();
    CapabilityPolicy::new(manifest.cell_id.clone(), grants)
}

// ──────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_request(op: SyscallOp, workspace: Option<&str>) -> SyscallRequest {
        SyscallRequest {
            id: "req-001".to_string(),
            caller_id: "cell-test".to_string(),
            op,
            token: None,
            workspace: workspace.map(ToString::to_string),
            payload: serde_json::Value::Null,
            timestamp: 0,
        }
    }

    #[test]
    fn heartbeat_always_allowed() {
        let policy = CapabilityPolicy::new("cell-1", vec![]);
        let req = make_request(SyscallOp::Heartbeat, None);
        assert_eq!(policy.evaluate(&req), PolicyVerdict::Allow);
    }

    #[test]
    fn deny_by_default_when_no_grants() {
        let policy = CapabilityPolicy::new("cell-1", vec![]);
        let req = make_request(SyscallOp::Infer, None);
        assert_eq!(policy.evaluate(&req), PolicyVerdict::Deny);
    }

    #[test]
    fn exact_grant_allows() {
        let policy = CapabilityPolicy::new("cell-1", vec![
            CapabilityGrant {
                capability: "infer".to_string(),
                scope: None,
                ephemeral: false,
            },
        ]);
        let req = make_request(SyscallOp::Infer, None);
        assert_eq!(policy.evaluate(&req), PolicyVerdict::Allow);
    }

    #[test]
    fn wildcard_grant_allows() {
        let policy = CapabilityPolicy::new("cell-1", vec![
            CapabilityGrant {
                capability: "tool:*".to_string(),
                scope: None,
                ephemeral: false,
            },
        ]);
        let req = make_request(SyscallOp::ToolCall, None);
        assert_eq!(policy.evaluate(&req), PolicyVerdict::Allow);
    }

    #[test]
    fn scoped_grant_restricts_workspace() {
        let policy = CapabilityPolicy::new("cell-1", vec![
            CapabilityGrant {
                capability: "infer".to_string(),
                scope: Some("/home/user/project".to_string()),
                ephemeral: false,
            },
        ]);

        let allowed = make_request(SyscallOp::Infer, Some("/home/user/project/src"));
        assert_eq!(policy.evaluate(&allowed), PolicyVerdict::Allow);

        let denied = make_request(SyscallOp::Infer, Some("/tmp/other"));
        assert_eq!(policy.evaluate(&denied), PolicyVerdict::Deny);

        let no_ws = make_request(SyscallOp::Infer, None);
        assert_eq!(policy.evaluate(&no_ws), PolicyVerdict::Deny);
    }

    #[test]
    fn grant_and_revoke() {
        let mut policy = CapabilityPolicy::new("cell-1", vec![]);
        let req = make_request(SyscallOp::Infer, None);

        assert_eq!(policy.evaluate(&req), PolicyVerdict::Deny);

        policy.grant(CapabilityGrant {
            capability: "infer".to_string(),
            scope: None,
            ephemeral: false,
        });
        assert_eq!(policy.evaluate(&req), PolicyVerdict::Allow);

        policy.revoke("infer");
        assert_eq!(policy.evaluate(&req), PolicyVerdict::Deny);
    }

    #[test]
    fn policy_from_manifest_converts_capabilities() {
        let manifest = SwarmCellManifest {
            cell_id: "cell-infer".to_string(),
            role: susi_abi::swarm::SwarmRole::InferenceDriver,
            capabilities: vec!["infer".to_string(), "blackboard:read".to_string()],
            bloom_filter: susi_abi::swarm::CapabilityBloom::empty(),
            endpoint: "http://127.0.0.1:9999".to_string(),
            trust_score: 1.0,
            last_heartbeat: 0,
        };
        let policy = policy_from_manifest(&manifest);
        assert_eq!(policy.grants().len(), 2);

        let req_infer = make_request(SyscallOp::Infer, None);
        assert_eq!(policy.evaluate(&req_infer), PolicyVerdict::Allow);

        let req_tool = make_request(SyscallOp::ToolCall, None);
        assert_eq!(policy.evaluate(&req_tool), PolicyVerdict::Deny);
    }
}
