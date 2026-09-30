//! Wiring: independent verifier evidence in MissionDag verify path (T-INTELLIBITZ-11).

use crate::dag::MissionDag;
use crate::independent_verify::{ReviewConclusion, ToolReceipt};
use crate::role_select::AgentEvidence;
use std::collections::BTreeSet;

#[test]
fn independent_verify_dispatch_wiring() {
    let fleet = vec![
        AgentEvidence {
            id: "DevOpsAgent".into(),
            capabilities: BTreeSet::from(["code".into()]),
            suitability: 0.9,
            cost_per_success: 1.0,
        },
        AgentEvidence {
            id: "EpistemicAuditorAgent".into(),
            capabilities: BTreeSet::from(["review".into()]),
            suitability: 0.8,
            cost_per_success: 0.5,
        },
    ];
    let mut dag = MissionDag::new("independent verify wiring");
    let roles = dag
        .assign_native_roles(
            &fleet,
            &BTreeSet::from(["code".into(), "review".into()]),
            10.0,
        )
        .expect("roles");

    // Implementer's own assertion cannot satisfy the verify path.
    let self_review = ReviewConclusion {
        reviewer: roles.implementer.clone(),
        pass: true,
        receipts: vec![ToolReceipt {
            tool: "test".into(),
            digest: "d1".into(),
        }],
        implementer: roles.implementer.clone(),
    };
    assert!(!dag.accept_independent_verify(&self_review));
    assert!(
        !dag.nodes
            .iter()
            .find(|n| n.title == "Independent Verify")
            .expect("verify node")
            .completed
    );

    // Pass without receipts fails.
    let no_receipts = ReviewConclusion {
        reviewer: roles.verifier.clone(),
        pass: true,
        receipts: vec![],
        implementer: roles.implementer.clone(),
    };
    assert!(!dag.accept_independent_verify(&no_receipts));

    // Independent reviewer + unique receipts completes the verify node.
    let ok = ReviewConclusion {
        reviewer: roles.verifier.clone(),
        pass: true,
        receipts: vec![ToolReceipt {
            tool: "cargo_test".into(),
            digest: "abc123".into(),
        }],
        implementer: roles.implementer.clone(),
    };
    assert!(dag.accept_independent_verify(&ok));
    assert!(
        dag.nodes
            .iter()
            .find(|n| n.title == "Independent Verify")
            .expect("verify node")
            .completed
    );
}
