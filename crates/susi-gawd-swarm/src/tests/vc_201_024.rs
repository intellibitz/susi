use crate::resource_schedule::{admit, reserve, Admit, DagNode, Resources};

#[test]
fn vc_201_024_queues_when_oversubscribed() {
    let free = Resources {
        cpu: 1.0,
        gpu_mem_gb: 4.0,
        mem_gb: 8.0,
        subprocesses: 1,
        model_ready: true,
        tool_grants: vec!["exec".into()],
    };
    let node = DagNode {
        id: "n1".into(),
        cpu: 2.0,
        gpu_mem_gb: 1.0,
        subprocesses: 1,
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
        subprocesses: 1,
        model_ready: true,
        tool_grants: vec!["exec".into()],
    };
    let node = DagNode {
        id: "n1".into(),
        cpu: 2.0,
        gpu_mem_gb: 3.0,
        subprocesses: 1,
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
        subprocesses: 1,
        mem_gb: 0.0,
        needs_model: true,
        needs_tools: vec!["exec".into()],
    };
    assert_eq!(admit(&bigger, &left), Admit::Queue);
}

#[test]
fn vc_201_024_queues_on_unready_model_and_missing_mem_or_subprocesses() {
    let base_free = Resources {
        cpu: 4.0,
        gpu_mem_gb: 8.0,
        mem_gb: 4.0,
        subprocesses: 1,
        model_ready: false,
        tool_grants: vec!["exec".into()],
    };
    let model_node = DagNode {
        id: "needs-model".into(),
        cpu: 1.0,
        gpu_mem_gb: 1.0,
        mem_gb: 1.0,
        subprocesses: 1,
        needs_model: true,
        needs_tools: vec![],
    };
    assert_eq!(admit(&model_node, &base_free), Admit::Queue);

    let ready_free = Resources {
        model_ready: true,
        ..base_free
    };
    let mem_heavy = DagNode {
        id: "mem-heavy".into(),
        mem_gb: 8.0,
        ..model_node.clone()
    };
    assert_eq!(admit(&mem_heavy, &ready_free), Admit::Queue);

    let subproc_heavy = DagNode {
        id: "subproc-heavy".into(),
        subprocesses: 2,
        ..model_node
    };
    assert_eq!(admit(&subproc_heavy, &ready_free), Admit::Queue);
}

#[test]
fn vc_201_024_queues_when_required_tool_grant_is_missing_or_nan_resource() {
    let free = Resources {
        cpu: 4.0,
        gpu_mem_gb: 8.0,
        mem_gb: 16.0,
        subprocesses: 2,
        model_ready: true,
        tool_grants: vec!["read".into()],
    };
    let missing_tool = DagNode {
        id: "needs-write".into(),
        cpu: 1.0,
        gpu_mem_gb: 0.0,
        mem_gb: 1.0,
        subprocesses: 1,
        needs_model: false,
        needs_tools: vec!["read".into(), "write".into()],
    };
    assert_eq!(admit(&missing_tool, &free), Admit::Queue);

    let invalid_cpu = DagNode {
        id: "negative-cpu".into(),
        cpu: -0.5,
        gpu_mem_gb: 0.0,
        mem_gb: 1.0,
        subprocesses: 1,
        needs_model: false,
        needs_tools: vec!["read".into()],
    };
    assert_eq!(admit(&invalid_cpu, &free), Admit::Queue);
}

#[test]
fn vc_201_024_node_without_model_requirement_runs_when_model_unready() {
    let free = Resources {
        cpu: 2.0,
        gpu_mem_gb: 0.0,
        mem_gb: 4.0,
        subprocesses: 1,
        model_ready: false,
        tool_grants: vec!["read".into()],
    };
    let tool_only = DagNode {
        id: "tool-only".into(),
        cpu: 1.0,
        gpu_mem_gb: 0.0,
        mem_gb: 1.0,
        subprocesses: 1,
        needs_model: false,
        needs_tools: vec!["read".into()],
    };
    assert_eq!(admit(&tool_only, &free), Admit::Run);
}
