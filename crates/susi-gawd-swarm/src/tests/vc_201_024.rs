use crate::resource_schedule::{admit, reserve, Admit, DagNode, Resources};

#[test]
fn vc_201_024_queues_when_oversubscribed() {
    let free = Resources {
        cpu: 1.0,
        gpu_mem_gb: 4.0,
        mem_gb: 8.0,
        model_ready: true,
        tool_grants: vec!["exec".into()],
    };
    let node = DagNode {
        id: "n1".into(),
        cpu: 2.0,
        gpu_mem_gb: 1.0,
        mem_gb: 0.0,
        needs_model: true,
        needs_tools: vec!["exec".into()],
    };
    assert_eq!(admit(&node, &free), Admit::Queue);
}

#[test]
fn vc_201_024_never_exceeds_reservation() {
    let free = Resources {
        cpu: 4.0,
        gpu_mem_gb: 8.0,
        mem_gb: 16.0,
        model_ready: true,
        tool_grants: vec!["exec".into()],
    };
    let node = DagNode {
        id: "n1".into(),
        cpu: 2.0,
        gpu_mem_gb: 3.0,
        mem_gb: 0.0,
        needs_model: true,
        needs_tools: vec!["exec".into()],
    };
    assert_eq!(admit(&node, &free), Admit::Run);
    let left = reserve(&free, &node);
    assert!(left.cpu <= free.cpu);
    assert!(left.gpu_mem_gb <= free.gpu_mem_gb);
    let bigger = DagNode {
        id: "n2".into(),
        cpu: 3.0,
        gpu_mem_gb: 6.0,
        mem_gb: 0.0,
        needs_model: true,
        needs_tools: vec!["exec".into()],
    };
    assert_eq!(admit(&bigger, &left), Admit::Queue);
}
