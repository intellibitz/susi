//! Wiring: native-fleet role selection into MissionDag (T-INTELLIBITZ-10).

use crate::agents::instantiate_native_agent;
use crate::dag::MissionDag;
use crate::role_select::{AgentEvidence, SelectError};
use std::collections::BTreeSet;

#[test]
fn native_role_select_wiring() {
    let fleet = vec![
        AgentEvidence {
            id: "DevOpsAgent".into(),
            capabilities: BTreeSet::from(["code".into(), "exec".into()]),
            suitability: 0.95,
            cost_per_success: 1.0,
        },
        AgentEvidence {
            id: "EpistemicAuditorAgent".into(),
            capabilities: BTreeSet::from(["review".into()]),
            suitability: 0.85,
            cost_per_success: 0.6,
        },
        AgentEvidence {
            id: "LibraryScoutAgent".into(),
            capabilities: BTreeSet::from(["crates".into()]),
            suitability: 0.7,
            cost_per_success: 0.4,
        },
        // Phantom: listed in fleet evidence but has no native factory.
        AgentEvidence {
            id: "TranslationAgent".into(),
            capabilities: BTreeSet::from(["translate".into(), "code".into(), "review".into()]),
            suitability: 0.99,
            cost_per_success: 0.1,
        },
    ];
    let required = BTreeSet::from(["code".into(), "review".into(), "crates".into()]);

    let mut dag = MissionDag::new("native role select wiring");
    let assignment = dag
        .assign_native_roles(&fleet, &required, 10.0)
        .expect("native roles");

    assert_eq!(assignment.implementer, "DevOpsAgent");
    assert_eq!(assignment.verifier, "EpistemicAuditorAgent");
    assert_eq!(assignment.specialist.as_deref(), Some("LibraryScoutAgent"));
    assert_ne!(
        assignment.implementer, "TranslationAgent",
        "DynamicAgent-only phantoms must not win implementer"
    );
    assert_ne!(
        assignment.verifier, "TranslationAgent",
        "DynamicAgent-only phantoms must not win verifier"
    );

    assert!(instantiate_native_agent(&assignment.implementer).is_some());
    assert!(instantiate_native_agent(&assignment.verifier).is_some());
    assert!(
        instantiate_native_agent(assignment.specialist.as_deref().expect("specialist")).is_some()
    );
    assert!(instantiate_native_agent("TranslationAgent").is_none());

    assert_eq!(dag.nodes[0].assigned_agent.as_deref(), Some("DevOpsAgent"));
    let verify = dag
        .nodes
        .iter()
        .find(|n| n.title == "Independent Verify")
        .expect("verifier node");
    assert_eq!(
        verify.assigned_agent.as_deref(),
        Some("EpistemicAuditorAgent")
    );
    let specialist = dag
        .nodes
        .iter()
        .find(|n| n.title == "Specialist")
        .expect("specialist node");
    assert_eq!(
        specialist.assigned_agent.as_deref(),
        Some("LibraryScoutAgent")
    );
}

#[test]
fn native_role_select_wiring_omits_specialist_when_primary_pair_covers_all_caps() {
    let fleet = vec![
        AgentEvidence {
            id: "DevOpsAgent".into(),
            capabilities: BTreeSet::from(["code".into(), "exec".into()]),
            suitability: 0.95,
            cost_per_success: 1.0,
        },
        AgentEvidence {
            id: "EpistemicAuditorAgent".into(),
            capabilities: BTreeSet::from(["review".into()]),
            suitability: 0.85,
            cost_per_success: 0.6,
        },
    ];
    let required = BTreeSet::from(["code".into(), "review".into()]);

    let mut dag = MissionDag::new("no specialist needed");
    let assignment = dag
        .assign_native_roles(&fleet, &required, 5.0)
        .expect("native roles without specialist");

    assert_eq!(assignment.implementer, "DevOpsAgent");
    assert_eq!(assignment.verifier, "EpistemicAuditorAgent");
    assert!(assignment.specialist.is_none());
    assert!(assignment.multi_agent_cheaper);
    assert!(dag.nodes.iter().all(|n| n.title != "Specialist"));
}

#[test]
fn native_role_select_wiring_rejects_all_phantom_or_single_native_fleet() {
    let phantom_only = vec![AgentEvidence {
        id: "TranslationAgent".into(),
        capabilities: BTreeSet::from(["code".into(), "review".into()]),
        suitability: 0.99,
        cost_per_success: 0.1,
    }];
    let required = BTreeSet::from(["code".into()]);

    let mut dag = MissionDag::new("phantom fleet");
    assert_eq!(
        dag.assign_native_roles(&phantom_only, &required, 10.0),
        Err(SelectError::NoImplementer)
    );

    let single_native = vec![AgentEvidence {
        id: "DevOpsAgent".into(),
        capabilities: BTreeSet::from(["code".into()]),
        suitability: 0.9,
        cost_per_success: 1.0,
    }];
    assert_eq!(
        dag.assign_native_roles(&single_native, &required, 10.0),
        Err(SelectError::NoIndependentVerifier)
    );
}

#[test]
fn native_role_select_wiring_reports_multi_agent_not_cheaper_when_exceeding_solo_cost() {
    let fleet = vec![
        AgentEvidence {
            id: "DevOpsAgent".into(),
            capabilities: BTreeSet::from(["code".into()]),
            suitability: 0.95,
            cost_per_success: 1.5,
        },
        AgentEvidence {
            id: "EpistemicAuditorAgent".into(),
            capabilities: BTreeSet::from(["review".into()]),
            suitability: 0.85,
            cost_per_success: 1.0,
        },
    ];
    let required = BTreeSet::from(["code".into(), "review".into()]);

    let mut dag = MissionDag::new("cost comparison");
    let assignment = dag
        .assign_native_roles(&fleet, &required, 2.0)
        .expect("native roles");
    assert!(!assignment.multi_agent_cheaper);
}

#[test]
fn native_role_select_wiring_rejects_empty_fleet() {
    let mut dag = MissionDag::new("empty fleet");
    let required = BTreeSet::from(["code".into()]);
    assert_eq!(
        dag.assign_native_roles(&[], &required, 10.0),
        Err(SelectError::NoImplementer)
    );
}
