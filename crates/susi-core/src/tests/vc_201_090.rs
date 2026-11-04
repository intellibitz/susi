use crate::vertical_workflow::{
    run_vertical_workflow, WorkflowError, WorkflowMode, WorkflowRequest,
};

#[test]
fn vc_201_090_online_completes_with_mcp_and_delegate() {
    let e = run_vertical_workflow(&WorkflowRequest {
        mode: WorkflowMode::Online,
        allow_delegate: true,
        model_allowed: true,
        mcp_tool: "search".into(),
        mcp_available: true,
    })
    .unwrap();
    assert!(e.retrieval_hit);
    assert_eq!(e.mcp_invoked.as_deref(), Some("search"));
    assert!(e.delegated_agent.is_some());
    assert_eq!(e.outcome, "completed");
}

#[test]
fn vc_201_090_offline_denied_and_dependency_failures() {
    let off = run_vertical_workflow(&WorkflowRequest {
        mode: WorkflowMode::Offline,
        allow_delegate: true,
        model_allowed: true,
        mcp_tool: "search".into(),
        mcp_available: true,
    })
    .unwrap();
    assert_eq!(off.outcome, "offline_local_only");
    assert!(off.model_used.is_none());

    assert_eq!(
        run_vertical_workflow(&WorkflowRequest {
            mode: WorkflowMode::DeniedEgress,
            allow_delegate: true,
            model_allowed: true,
            mcp_tool: "search".into(),
            mcp_available: true,
        }),
        Err(WorkflowError::EgressDenied)
    );
    assert_eq!(
        run_vertical_workflow(&WorkflowRequest {
            mode: WorkflowMode::Online,
            allow_delegate: false,
            model_allowed: true,
            mcp_tool: "search".into(),
            mcp_available: false,
        }),
        Err(WorkflowError::DependencyFailed)
    );
}

#[test]
fn vc_201_090_denied_egress_without_delegation_completes_locally_when_model_and_mcp_allowed() {
    let out = run_vertical_workflow(&WorkflowRequest {
        mode: WorkflowMode::DeniedEgress,
        allow_delegate: false,
        model_allowed: true,
        mcp_tool: "fs".into(),
        mcp_available: true,
    })
    .expect("denied-egress local workflow without delegation must succeed");
    assert!(out.retrieval_hit);
    assert_eq!(out.model_used.as_deref(), Some("local-allowed"));
    assert_eq!(out.mcp_invoked.as_deref(), Some("fs"));
    assert!(out.delegated_agent.is_none());
    assert_eq!(out.outcome, "completed");
}

#[test]
fn vc_201_090_online_without_delegation_or_with_disallowed_model_enforces_contract() {
    let out = run_vertical_workflow(&WorkflowRequest {
        mode: WorkflowMode::Online,
        allow_delegate: false,
        model_allowed: true,
        mcp_tool: "fs".into(),
        mcp_available: true,
    })
    .expect("online workflow without delegation must succeed");
    assert!(out.delegated_agent.is_none());
    assert_eq!(out.outcome, "completed");

    assert_eq!(
        run_vertical_workflow(&WorkflowRequest {
            mode: WorkflowMode::Online,
            allow_delegate: false,
            model_allowed: false,
            mcp_tool: "fs".into(),
            mcp_available: true,
        }),
        Err(WorkflowError::DependencyFailed)
    );
}

#[test]
fn vc_201_090_offline_mode_succeeds_even_when_mcp_is_unavailable() {
    let out = run_vertical_workflow(&WorkflowRequest {
        mode: WorkflowMode::Offline,
        allow_delegate: false,
        model_allowed: false,
        mcp_tool: "search".into(),
        mcp_available: false,
    })
    .expect("offline local-only mode does not depend on remote model or mcp availability");
    assert_eq!(out.outcome, "offline_local_only");
    assert!(out.retrieval_hit);
}
