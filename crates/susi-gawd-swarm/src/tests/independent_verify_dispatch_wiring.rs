//! Wiring: independent verifier evidence in MissionDag verify path (T-INTELLIBITZ-11).

use crate::dag::MissionDag;
use crate::independent_verify::{digest_fields, ReviewConclusion, ToolReceipt};
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
            digest: digest_fields(&["receipt", "d1"]),
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
            digest: digest_fields(&["receipt", "abc123"]),
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

#[test]
fn independent_verify_rejects_failing_conclusion_even_from_distinct_verifier() {
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
    let mut dag = MissionDag::new("reject pass=false");
    let roles = dag
        .assign_native_roles(
            &fleet,
            &BTreeSet::from(["code".into(), "review".into()]),
            10.0,
        )
        .expect("roles");
    let failed = ReviewConclusion {
        reviewer: roles.verifier,
        pass: false,
        receipts: vec![ToolReceipt {
            tool: "cargo_test".into(),
            digest: digest_fields(&["receipt", "fail-1"]),
        }],
        implementer: roles.implementer,
    };
    assert!(!dag.accept_independent_verify(&failed));
    assert!(
        !dag.nodes
            .iter()
            .find(|n| n.title == "Independent Verify")
            .expect("verify node")
            .completed
    );
}

#[test]
fn independent_verify_rejects_duplicate_receipt_digests() {
    let mut dag = MissionDag::new("reject duplicate receipt digests");
    let dup_digest = digest_fields(&["receipt", "dup"]);
    let replayed = ReviewConclusion {
        reviewer: "EpistemicAuditorAgent".into(),
        pass: true,
        receipts: vec![
            ToolReceipt {
                tool: "cargo_test".into(),
                digest: dup_digest.clone(),
            },
            ToolReceipt {
                tool: "cargo_clippy".into(),
                digest: dup_digest,
            },
        ],
        implementer: "DevOpsAgent".into(),
    };
    assert!(!dag.accept_independent_verify(&replayed));
}

#[test]
fn assign_native_roles_errors_without_independent_verifier_and_tracks_cost_bound() {
    let single_agent = vec![AgentEvidence {
        id: "DevOpsAgent".into(),
        capabilities: BTreeSet::from(["code".into()]),
        suitability: 0.9,
        cost_per_success: 5.0,
    }];
    let mut dag = MissionDag::new("role selection bounds");
    assert!(dag
        .assign_native_roles(&single_agent, &BTreeSet::from(["code".into()]), 10.0)
        .is_err());

    let pair = vec![
        AgentEvidence {
            id: "DevOpsAgent".into(),
            capabilities: BTreeSet::from(["code".into()]),
            suitability: 0.9,
            cost_per_success: 5.0,
        },
        AgentEvidence {
            id: "EpistemicAuditorAgent".into(),
            capabilities: BTreeSet::from(["review".into()]),
            suitability: 0.8,
            cost_per_success: 5.0,
        },
    ];
    let assigned = dag
        .assign_native_roles(
            &pair,
            &BTreeSet::from(["code".into(), "review".into()]),
            4.0,
        )
        .expect("two native agents assign");
    assert!(!assigned.multi_agent_cheaper);
}

#[test]
fn digest_fields_is_deterministic_and_distinguishes_field_order() {
    let d1 = digest_fields(&["cargo_test", "pass"]);
    let d2 = digest_fields(&["cargo_test", "pass"]);
    let d3 = digest_fields(&["pass", "cargo_test"]);
    assert_eq!(d1, d2);
    assert_ne!(d1, d3);
    assert!(!d1.is_empty());
}
