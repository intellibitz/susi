use crate::role_select::{select_roles, AgentEvidence, SelectError};
use std::collections::BTreeSet;

#[test]
fn vc_201_027_picks_independent_verifier_and_specialist() {
    let agents = vec![
        AgentEvidence {
            id: "impl".into(),
            capabilities: BTreeSet::from(["code".into()]),
            suitability: 0.9,
            cost_per_success: 1.0,
        },
        AgentEvidence {
            id: "ver".into(),
            capabilities: BTreeSet::from(["review".into()]),
            suitability: 0.8,
            cost_per_success: 0.5,
        },
        AgentEvidence {
            id: "spec".into(),
            capabilities: BTreeSet::from(["gpu".into()]),
            suitability: 0.7,
            cost_per_success: 0.4,
        },
    ];
    let required = BTreeSet::from(["code".into(), "review".into(), "gpu".into()]);
    let a = select_roles(&agents, &required, 10.0).unwrap();
    assert_eq!(a.implementer, "impl");
    assert_eq!(a.verifier, "ver");
    assert_eq!(a.specialist.as_deref(), Some("spec"));
    assert!(a.multi_agent_cheaper);
}

#[test]
fn vc_201_027_reports_when_single_agent_cheaper() {
    let agents = vec![
        AgentEvidence {
            id: "a".into(),
            capabilities: BTreeSet::from(["all".into()]),
            suitability: 0.9,
            cost_per_success: 5.0,
        },
        AgentEvidence {
            id: "b".into(),
            capabilities: BTreeSet::from(["all".into()]),
            suitability: 0.5,
            cost_per_success: 5.0,
        },
    ];
    let required = BTreeSet::from(["all".into()]);
    let a = select_roles(&agents, &required, 1.0).unwrap();
    assert!(!a.multi_agent_cheaper);
}

#[test]
fn vc_201_027_requires_independent_verifier() {
    let agents = vec![AgentEvidence {
        id: "solo".into(),
        capabilities: BTreeSet::new(),
        suitability: 1.0,
        cost_per_success: 1.0,
    }];
    let err = select_roles(&agents, &BTreeSet::new(), 1.0).unwrap_err();
    assert_eq!(err, SelectError::NoIndependentVerifier);
}
