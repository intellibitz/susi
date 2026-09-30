//! Wiring: resource-constrained MissionDag admits (T-INTELLIBITZ-8 / VC-201-024).

use crate::dag::MissionDag;
use crate::resource_schedule::{admit, Admit, DagNode, Resources};
use std::collections::BTreeMap;

#[test]
fn resource_schedule_dispatch_wiring() {
    let free = Resources {
        cpu: 2.0,
        gpu_mem_gb: 4.0,
        mem_gb: 8.0,
        model_ready: true,
        tool_grants: vec!["exec_command".into()],
    };
    let mut reqs = BTreeMap::new();
    reqs.insert(
        0,
        DagNode {
            id: "n0".into(),
            cpu: 1.0,
            gpu_mem_gb: 1.0,
            mem_gb: 0.0,
            needs_model: true,
            needs_tools: vec!["exec_command".into()],
        },
    );
    reqs.insert(
        1,
        DagNode {
            id: "n1".into(),
            cpu: 2.0,
            gpu_mem_gb: 4.0,
            mem_gb: 0.0,
            needs_model: true,
            needs_tools: vec!["exec_command".into()],
        },
    );
    reqs.insert(
        2,
        DagNode {
            id: "n2".into(),
            cpu: 1.0,
            gpu_mem_gb: 1.0,
            mem_gb: 0.0,
            needs_model: false,
            needs_tools: vec!["missing_tool".into()],
        },
    );

    assert_eq!(admit(&reqs[&0], &free), Admit::Run);
    assert_eq!(admit(&reqs[&2], &free), Admit::Queue);

    let (admitted, remaining) = MissionDag::schedule_ready(&[0, 1, 2], &free, &reqs);
    // n0 fits; after reserve, n1 oversubscribes CPU; n2 lacks tool grant.
    assert_eq!(admitted, vec![0]);
    assert!((remaining.cpu - 1.0).abs() < f64::EPSILON);
    assert!(!admitted.contains(&1));
    assert!(!admitted.contains(&2));
}
