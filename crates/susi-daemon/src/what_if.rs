//! What-If Analysis (Swarm OS Bullet 76)
//!
//! Projects the effect of adding or removing a cell, or revoking one
//! capability grant, without mutating the live roster or policy. Routing
//! impact uses the same bloom-filter membership `find_capable_cells`
//! uses. Policy impact re-runs `CapabilityPolicy::evaluate` on a clone
//! after `revoke`.

use crate::susi_abi::swarm::SwarmCellManifest;
use crate::susi_abi::syscall::{SyscallOp, SyscallRequest};

use crate::security::{CapabilityPolicy, PolicyVerdict};

pub enum CellChange {
    Remove { cell_id: String },
    Add { manifest: SwarmCellManifest },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutingProjection {
    pub capable_before: usize,
    pub capable_after: usize,
}

fn is_capable(cell: &SwarmCellManifest, capability_hash: u64) -> bool {
    cell.bloom_filter.may_contain_hash(capability_hash)
}

/// How many registered cells can take `capability_hash` before and after
/// `change`. Adding a cell whose id is already registered replaces that
/// row, matching `register_cell`'s last-write-wins behavior.
pub fn project_cell_change(
    cells: &[SwarmCellManifest],
    capability_hash: u64,
    change: &CellChange,
) -> RoutingProjection {
    let capable_before = cells
        .iter()
        .filter(|cell| is_capable(cell, capability_hash))
        .count();
    let after: Vec<SwarmCellManifest> = match change {
        CellChange::Remove { cell_id } => cells
            .iter()
            .filter(|cell| &cell.cell_id != cell_id)
            .cloned()
            .collect(),
        CellChange::Add { manifest } => {
            let mut kept: Vec<SwarmCellManifest> = cells
                .iter()
                .filter(|cell| cell.cell_id != manifest.cell_id)
                .cloned()
                .collect();
            kept.push(manifest.clone());
            kept
        }
    };
    let capable_after = after
        .iter()
        .filter(|cell| is_capable(cell, capability_hash))
        .count();
    RoutingProjection {
        capable_before,
        capable_after,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyProjection {
    pub allowed_before: usize,
    pub newly_denied: Vec<SyscallOp>,
}

/// Calls in `requests` that `policy` currently allows and that a clone
/// with `capability` revoked would deny. The original policy is not
/// modified. Heartbeat stays allowed because `evaluate` never gates it
/// on a grant.
pub fn project_capability_revoke(
    policy: &CapabilityPolicy,
    capability: &str,
    requests: &[SyscallRequest],
) -> PolicyProjection {
    let mut revoked = policy.clone();
    revoked.revoke(capability);
    let mut allowed_before = 0;
    let mut newly_denied = Vec::new();
    for request in requests {
        let before = policy.evaluate(request);
        let after = revoked.evaluate(request);
        if before == PolicyVerdict::Allow {
            allowed_before += 1;
        }
        if before == PolicyVerdict::Allow && after == PolicyVerdict::Deny {
            newly_denied.push(request.op);
        }
    }
    PolicyProjection {
        allowed_before,
        newly_denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::CapabilityGrant;
    use crate::susi_abi::swarm::{CapabilityBloom, SwarmRole};

    fn manifest(cell_id: &str, hash: Option<u64>) -> SwarmCellManifest {
        let mut bloom_filter = CapabilityBloom::default();
        if let Some(hash) = hash {
            bloom_filter.insert_hash(hash);
        }
        SwarmCellManifest {
            cell_id: cell_id.to_string(),
            role: SwarmRole::ReflexCell,
            capabilities: Vec::new(),
            bloom_filter,
            endpoint: "ipc:///tmp/test.sock".to_string(),
            trust_score: 0.5,
            last_heartbeat: 0,
        }
    }

    fn request(op: SyscallOp) -> SyscallRequest {
        SyscallRequest {
            id: "req".to_string(),
            caller_id: "cell-a".to_string(),
            op,
            token: None,
            workspace: None,
            payload: serde_json::Value::Null,
            timestamp: 0,
        }
    }

    #[test]
    fn removing_the_only_capable_cell_drops_the_count_to_zero() {
        const HASH: u64 = 0xC0FFEE;
        let cells = vec![manifest("only", Some(HASH)), manifest("other", None)];
        let projection = project_cell_change(
            &cells,
            HASH,
            &CellChange::Remove {
                cell_id: "only".to_string(),
            },
        );
        assert_eq!(projection.capable_before, 1);
        assert_eq!(projection.capable_after, 0);
    }

    #[test]
    fn adding_a_capable_cell_raises_the_count_without_touching_the_input() {
        const HASH: u64 = 7;
        let cells = vec![manifest("existing", Some(HASH))];
        let before_len = cells.len();
        let projection = project_cell_change(
            &cells,
            HASH,
            &CellChange::Add {
                manifest: manifest("extra", Some(HASH)),
            },
        );
        assert_eq!(projection.capable_before, 1);
        assert_eq!(projection.capable_after, 2);
        assert_eq!(cells.len(), before_len);
    }

    #[test]
    fn revoking_infer_denies_infer_and_leaves_heartbeat_allowed() {
        let policy = CapabilityPolicy::new(
            "cell-a",
            vec![CapabilityGrant {
                capability: "infer".to_string(),
                scope: None,
                ephemeral: false,
            }],
        );
        let requests = vec![request(SyscallOp::Infer), request(SyscallOp::Heartbeat)];
        let projection = project_capability_revoke(&policy, "infer", &requests);
        assert_eq!(projection.allowed_before, 2);
        assert_eq!(projection.newly_denied, vec![SyscallOp::Infer]);
        assert_eq!(policy.grants().len(), 1);
    }
}
