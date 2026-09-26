#![forbid(unsafe_code)]

use std::sync::Arc;

use susi_gawd::susi_abi::cell::{cell_bind_addr, cell_ports, SwarmCell};
use susi_gawd::susi_abi::swarm::SwarmRole;
use susi_gawd::susi_abi::syscall::{
    SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV,
};

use susi_gawd::susi_abi;

// Shared swarm-cell server loop (canonical: crates/susi-abi/src/cell_server.rs).
#[rustfmt::skip]
#[path = "../../susi-abi/src/cell_server.rs"]
mod cell_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
    let expected_token: Arc<str> = std::env::var(CELL_TOKEN_ENV)
        .ok()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(susi_gawd::susi_config::SusiConfig::ensure_api_auth_token_seeded)
        .into();

    cell_server::serve(
        cell,
        bind_addr,
        expected_token,
        "susi-gawd Swarm Cell",
        handle_plan,
    )
    .await?;
    Ok(())
}

async fn handle_plan(req: SyscallRequest) -> SyscallResponse {
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
        tokio::task::spawn_blocking(move || {
            susi_gawd::ama::SusiMasterAgent::new().solve_stream_report(
                &goal,
                &workspace,
                env!("CARGO_PKG_VERSION"),
                &|_| {},
            )
        })
        .await
        .map_err(|e| format!("planner task failed: {e}"))
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
