//! Security cell scan (Swarm OS Bullet 59)
//!
//! Walks the world model for missing evidence, residency outside an
//! allowed set, dangling `affects` edges, and org-policy denials recorded
//! against a capability the node claims in its payload.

use crate::org_policy::{self, OrgRule, PolicyDecision};
use crate::world_model::{Finding, WorldModel, structural_findings};

/// `claimed_capability` is read from each node's payload when it is shaped
/// `capability:<name>`. Other payloads are not treated as capability claims.
pub fn scan(model: &WorldModel, allowed_regions: &[String], rules: &[OrgRule]) -> Vec<Finding> {
    let mut findings = structural_findings(model, allowed_regions);
    for node in model.latest_nodes() {
        if let Some(capability) = node.payload.strip_prefix("capability:")
            && org_policy::decide(rules, capability, Some(node.region.as_str()))
                == PolicyDecision::Deny
        {
            findings.push(Finding {
                subject: node.id.clone(),
                reason: format!("capability '{capability}' is denied by org policy"),
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org_policy::OrgRule;
    use crate::world_model::{NodeDraft, TypePerm, WorldModel};

    #[test]
    fn a_claim_without_evidence_and_a_denied_capability_are_findings() {
        let mut model = WorldModel::new();
        model.grant("scanner", "claim", TypePerm::Write);
        model
            .put(
                "scanner",
                NodeDraft {
                    id: "c1".into(),
                    type_name: "claim".into(),
                    namespace: "proj".into(),
                    region: "eu".into(),
                    payload: "capability:tool:drop".into(),
                },
            )
            .unwrap();
        let rules = vec![OrgRule {
            capability: "tool:drop".into(),
            scope_prefix: None,
            decision: PolicyDecision::Deny,
        }];
        let findings = scan(&model, &["eu".into()], &rules);
        assert!(
            findings
                .iter()
                .any(|finding| finding.reason.contains("no evidence"))
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.reason.contains("denied"))
        );
    }
}
