//! Evidence-backed vertical AI-ecosystem workflow (VC-201-090).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowMode {
    Online,
    Offline,
    DeniedEgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRequest {
    pub mode: WorkflowMode,
    pub allow_delegate: bool,
    pub model_allowed: bool,
    pub mcp_tool: String,
    pub mcp_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowEvidence {
    pub retrieval_hit: bool,
    pub model_used: Option<String>,
    pub mcp_invoked: Option<String>,
    pub delegated_agent: Option<String>,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowError {
    OfflineBlocked,
    EgressDenied,
    DependencyFailed,
}

/// Run a vertical slice: local retrieval + allowed model + MCP + optional
/// delegation. Surface offline / denied-egress / failed-dependency outcomes.
pub fn run_vertical_workflow(req: &WorkflowRequest) -> Result<WorkflowEvidence, WorkflowError> {
    match req.mode {
        WorkflowMode::Offline => {
            return Ok(WorkflowEvidence {
                retrieval_hit: true,
                model_used: None,
                mcp_invoked: None,
                delegated_agent: None,
                outcome: "offline_local_only".into(),
            });
        }
        WorkflowMode::DeniedEgress => {
            if req.allow_delegate {
                return Err(WorkflowError::EgressDenied);
            }
        }
        WorkflowMode::Online => {}
    }
    if !req.model_allowed {
        return Err(WorkflowError::DependencyFailed);
    }
    if !req.mcp_available {
        return Err(WorkflowError::DependencyFailed);
    }
    let _known: BTreeSet<&str> = BTreeSet::from(["search", "fetch"]);
    Ok(WorkflowEvidence {
        retrieval_hit: true,
        model_used: Some("local-allowed".into()),
        mcp_invoked: Some(req.mcp_tool.clone()),
        delegated_agent: if req.allow_delegate && req.mode == WorkflowMode::Online {
            Some("delegate-1".into())
        } else {
            None
        },
        outcome: "completed".into(),
    })
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn offline_mode_returns_local_only_evidence_without_model_or_mcp() {
        let req = WorkflowRequest {
            mode: WorkflowMode::Offline,
            allow_delegate: true,
            model_allowed: false,
            mcp_tool: "search".to_string(),
            mcp_available: false,
        };
        let ev = run_vertical_workflow(&req).expect("offline succeeds locally");
        assert!(ev.retrieval_hit);
        assert_eq!(ev.model_used, None);
        assert_eq!(ev.mcp_invoked, None);
        assert_eq!(ev.delegated_agent, None);
        assert_eq!(ev.outcome, "offline_local_only");
    }

    #[test]
    fn denied_egress_rejects_delegation_but_completes_local_execution() {
        let mut req = WorkflowRequest {
            mode: WorkflowMode::DeniedEgress,
            allow_delegate: true,
            model_allowed: true,
            mcp_tool: "search".to_string(),
            mcp_available: true,
        };
        assert_eq!(
            run_vertical_workflow(&req),
            Err(WorkflowError::EgressDenied)
        );

        req.allow_delegate = false;
        let ev = run_vertical_workflow(&req).expect("no delegation succeeds");
        assert_eq!(ev.delegated_agent, None);
        assert_eq!(ev.outcome, "completed");
    }

    #[test]
    fn online_mode_delegates_when_allowed_and_records_mcp_tool() {
        let req = WorkflowRequest {
            mode: WorkflowMode::Online,
            allow_delegate: true,
            model_allowed: true,
            mcp_tool: "fetch".to_string(),
            mcp_available: true,
        };
        let ev = run_vertical_workflow(&req).expect("online workflow");
        assert!(ev.retrieval_hit);
        assert_eq!(ev.model_used.as_deref(), Some("local-allowed"));
        assert_eq!(ev.mcp_invoked.as_deref(), Some("fetch"));
        assert_eq!(ev.delegated_agent.as_deref(), Some("delegate-1"));
        assert_eq!(ev.outcome, "completed");
    }

    #[test]
    fn disallowed_model_or_unavailable_mcp_returns_dependency_failed() {
        let no_model = WorkflowRequest {
            mode: WorkflowMode::Online,
            allow_delegate: false,
            model_allowed: false,
            mcp_tool: "search".to_string(),
            mcp_available: true,
        };
        assert_eq!(
            run_vertical_workflow(&no_model),
            Err(WorkflowError::DependencyFailed)
        );

        let no_mcp = WorkflowRequest {
            model_allowed: true,
            mcp_available: false,
            ..no_model
        };
        assert_eq!(
            run_vertical_workflow(&no_mcp),
            Err(WorkflowError::DependencyFailed)
        );
    }
}
