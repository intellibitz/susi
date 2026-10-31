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
