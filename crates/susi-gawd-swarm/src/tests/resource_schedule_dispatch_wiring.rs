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
        subprocesses: 2,
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
            subprocesses: 1,
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
            subprocesses: 1,
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
            subprocesses: 1,
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

#[test]
fn resource_schedule_dispatch_wiring_model_ready_false_queues_model_dependent_nodes() {
    let free = Resources {
        cpu: 4.0,
        gpu_mem_gb: 8.0,
        mem_gb: 16.0,
        subprocesses: 4,
        model_ready: false,
        tool_grants: vec!["exec_command".into()],
    };
    let mut reqs = BTreeMap::new();
    reqs.insert(
        0,
        DagNode {
            id: "n0".into(),
            cpu: 1.0,
            gpu_mem_gb: 0.0,
            mem_gb: 1.0,
            subprocesses: 1,
            needs_model: true,
            needs_tools: vec!["exec_command".into()],
        },
    );
    reqs.insert(
        1,
        DagNode {
            id: "n1".into(),
            cpu: 1.0,
            gpu_mem_gb: 0.0,
            mem_gb: 1.0,
            subprocesses: 1,
            needs_model: false,
            needs_tools: vec!["exec_command".into()],
        },
    );

    let (admitted, remaining) = MissionDag::schedule_ready(&[0, 1], &free, &reqs);
    assert_eq!(admitted, vec![1]);
    assert_eq!(remaining.subprocesses, 3);
}

#[test]
fn resource_schedule_dispatch_wiring_gpu_and_subprocess_exhaustion_preserves_remaining() {
    let free = Resources {
        cpu: 8.0,
        gpu_mem_gb: 6.0,
        mem_gb: 16.0,
        subprocesses: 2,
        model_ready: true,
        tool_grants: vec!["exec_command".into()],
    };
    let mut reqs = BTreeMap::new();
    reqs.insert(
        0,
        DagNode {
            id: "n0".into(),
            cpu: 2.0,
            gpu_mem_gb: 4.0,
            mem_gb: 4.0,
            subprocesses: 2,
            needs_model: true,
            needs_tools: vec!["exec_command".into()],
        },
    );
    reqs.insert(
        1,
        DagNode {
            id: "n1".into(),
            cpu: 1.0,
            gpu_mem_gb: 1.0,
            mem_gb: 1.0,
            subprocesses: 1,
            needs_model: true,
            needs_tools: vec!["exec_command".into()],
        },
    );

    let (admitted, remaining) = MissionDag::schedule_ready(&[0, 1], &free, &reqs);
    assert_eq!(admitted, vec![0]);
    assert!((remaining.gpu_mem_gb - 2.0).abs() < f64::EPSILON);
    assert_eq!(remaining.subprocesses, 0);
}

#[test]
fn resource_schedule_dispatch_wiring_default_resource_node_and_non_finite_rejection() {
    let free = Resources {
        cpu: 2.0,
        gpu_mem_gb: 0.0,
        mem_gb: 4.0,
        subprocesses: 2,
        model_ready: true,
        tool_grants: vec!["exec_command".into()],
    };
    let empty_reqs = BTreeMap::new();
    let (admitted, remaining) = MissionDag::schedule_ready(&[0, 1, 2], &free, &empty_reqs);
    assert_eq!(admitted, vec![0, 1]);
    assert_eq!(remaining.subprocesses, 0);

    let mut invalid = MissionDag::default_resource_node(9);
    invalid.cpu = f64::NAN;
    assert_eq!(admit(&invalid, &free), Admit::Queue);
}

#[test]
fn resource_schedule_dispatch_wiring_empty_ready_slice_leaves_resources_unchanged() {
    let free = Resources {
        cpu: 4.0,
        gpu_mem_gb: 2.0,
        mem_gb: 8.0,
        subprocesses: 3,
        model_ready: true,
        tool_grants: vec!["exec_command".into()],
    };
    let reqs = BTreeMap::new();
    let (admitted, remaining) = MissionDag::schedule_ready(&[], &free, &reqs);
    assert!(admitted.is_empty());
    assert!((remaining.cpu - 4.0).abs() < f64::EPSILON);
    assert_eq!(remaining.subprocesses, 3);
}
