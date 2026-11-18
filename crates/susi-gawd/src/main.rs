#![forbid(unsafe_code)]

use susi_gawd::susi_abi::cell::cell_ports;
use susi_gawd::susi_abi::swarm::SwarmRole;
use susi_gawd::susi_abi::syscall::{SyscallRequest, SyscallResponse};

use susi_gawd::susi_abi;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    susi_abi::cell_server::run_pillar(
        "susi-gawd",
        cell_ports::GAWD,
        "gawd-planner-01",
        SwarmRole::PlannerCell,
        &["swarm-scheduling", "autonomous-planner"],
        susi_gawd::susi_config::SusiConfig::ensure_api_auth_token_seeded,
        handle_plan,
    )
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
    let latency_us = susi_abi::cell_server::elapsed_micros(started);
    let (data, error) = match outcome {
        // Mission status is independent of the prose returned by the planner.
        Ok(report) => {
            let error = (!report.is_success()).then(|| format!("mission ended {}", report.status));
            (
                serde_json::json!({ "result": report.final_answer, "mission_status": report.status }),
                error,
            )
        }
        Err(message) => (serde_json::Value::Null, Some(message)),
    };
    susi_abi::cell_server::handler_reply(req.id, latency_us, data, error)
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
        assert_eq!(resp.status, susi_abi::syscall::SyscallStatus::Error);
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
        assert_eq!(resp.status, susi_abi::syscall::SyscallStatus::Error);
        assert_eq!(resp.data, serde_json::Value::Null);
    }

    #[test]
    fn handle_plan_rejects_empty_intent_fallback_with_error_status() {
        let resp = handle_plan(sample_request("req-3", serde_json::json!({ "intent": "" })));
        assert_eq!(resp.id, "req-3");
        assert_eq!(resp.status, susi_abi::syscall::SyscallStatus::Error);
    }

    #[test]
    fn planner_swarm_cell_registers_scheduling_and_planner_capabilities() {
        let bind_addr = susi_abi::cell::cell_bind_addr(cell_ports::GAWD);
        let mut cell = susi_abi::cell::SwarmCell::new(
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
