#![forbid(unsafe_code)]

use std::sync::Arc;

use susi_gawd::susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use susi_gawd::susi_abi::swarm::SwarmRole;
use susi_gawd::susi_abi::syscall::{
    SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};

use susi_gawd::susi_abi;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind_addr = cell_bind_addr(cell_ports::GAWD);
    println!("susi-gawd micro-daemon starting on {bind_addr}");

    let mut cell = SwarmCell::new(
        "gawd-planner-01".to_string(),
        SwarmRole::PlannerCell,
        format!("tcp://{bind_addr}"),
    );

    // Register capabilities
    cell.register_capability("swarm-scheduling");
    cell.register_capability("autonomous-planner");

    // Every syscall must present the host API token (or an explicit
    // SUSI_CELL_TOKEN override); heartbeats stay open for liveness.
    let expected_token: Arc<str> = match std::env::var(CELL_TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
    {
        Some(token) => token.into(),
        None => susi_gawd::susi_config::SusiConfig::ensure_api_auth_token_seeded()?.into(),
    };

    susi_abi::cell_server::serve(
        cell,
        bind_addr,
        expected_token,
        "susi-gawd Swarm Cell",
        handle_plan,
    )?;
    Ok(())
}

fn handle_plan(req: SyscallRequest) -> SyscallResponse {
    // Run the mission through the real master agent; an empty goal is an
    // error, never a fabricated "plan executed" success.
    let goal = req
        .payload
        .get("goal")
        .or_else(|| req.payload.get("intent"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let workspace = std::path::PathBuf::from(req.workspace.as_deref().unwrap_or("."));
    let started = std::time::Instant::now();
    let outcome = if goal.trim().is_empty() {
        Err("missing goal in payload (`goal`)".to_string())
    } else {
        // The cell server runs each connection on its own thread, so the
        // blocking mission runs here directly; a panic in it is answered as
        // an `Error` by the server.
        Ok(susi_gawd::ama::SusiMasterAgent::new().solve_stream_report(
            &goal,
            &workspace,
            env!("CARGO_PKG_VERSION"),
            &|_| {},
        ))
    };
    let latency_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    match outcome {
        // Outcome comes from the mission report, never from the prose.
        Ok(report) => {
            let ok = report.is_success();
            SyscallResponse {
                id: req.id,
                status: if ok {
                    SyscallStatus::Success
                } else {
                    SyscallStatus::Error
                },
                data: serde_json::json!({
                    "result": report.final_answer,
                    "mission_status": report.status,
                }),
                receipt: None,
                latency_us,
                message: (!ok).then(|| format!("mission ended {}", report.status)),
            }
        }
        Err(e) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::Value::Null,
            receipt: None,
            latency_us,
            message: Some(e),
        },
    }
}

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod inline_tests {
    use super::*;
    use susi_gawd::susi_abi::syscall::SyscallOp;

    fn sample_request(id: &str, payload: serde_json::Value) -> SyscallRequest {
        SyscallRequest {
            id: id.to_string(),
            caller_id: "test-caller".to_string(),
            op: SyscallOp::Infer,
            token: None,
            workspace: Some(".".to_string()),
            payload,
            timestamp: 1,
        }
    }

    #[test]
    fn handle_plan_rejects_missing_goal_field_with_error_status() {
        let resp = handle_plan(sample_request("req-1", serde_json::json!({})));
        assert_eq!(resp.id, "req-1");
        assert_eq!(resp.status, SyscallStatus::Error);
        assert!(resp
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("missing goal"));
    }

    #[test]
    fn handle_plan_rejects_whitespace_only_goal_with_error_status() {
        let resp = handle_plan(sample_request(
            "req-2",
            serde_json::json!({ "goal": "   \t\n  " }),
        ));
        assert_eq!(resp.id, "req-2");
        assert_eq!(resp.status, SyscallStatus::Error);
        assert_eq!(resp.data, serde_json::Value::Null);
    }

    #[test]
    fn handle_plan_rejects_empty_intent_fallback_with_error_status() {
        let resp = handle_plan(sample_request("req-3", serde_json::json!({ "intent": "" })));
        assert_eq!(resp.id, "req-3");
        assert_eq!(resp.status, SyscallStatus::Error);
    }

    #[test]
    fn planner_swarm_cell_registers_scheduling_and_planner_capabilities() {
        let bind_addr = cell_bind_addr(cell_ports::GAWD);
        let mut cell = SwarmCell::new(
            "gawd-planner-01".to_string(),
            SwarmRole::PlannerCell,
            format!("tcp://{bind_addr}"),
        );
        cell.register_capability("swarm-scheduling");
        cell.register_capability("autonomous-planner");
        assert_eq!(cell.manifest.role, SwarmRole::PlannerCell);
        assert_eq!(cell.manifest.capabilities.len(), 2);
    }
}
