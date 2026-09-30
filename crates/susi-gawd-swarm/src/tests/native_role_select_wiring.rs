//! Wiring: native-fleet role selection into MissionDag (T-INTELLIBITZ-10).

use crate::agents::instantiate_native_agent;
use crate::dag::MissionDag;
use crate::role_select::AgentEvidence;
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
