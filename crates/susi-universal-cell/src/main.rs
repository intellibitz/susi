#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use serde::Deserialize;
use std::sync::Arc;
use tokio::process::Command;

use susi_abi::cell::SwarmCell;
use susi_abi::cell_server;
use susi_abi::swarm::SwarmRole;
use susi_abi::syscall::{SyscallRequest, SyscallResponse, SyscallStatus, CELL_TOKEN_ENV};

#[derive(Debug, Deserialize)]
struct PluginManifest {
    name: String,
    role: String,
    capabilities: Vec<String>,
    command: String,
    args: Vec<String>,
}

/// Map a manifest role string to the swarm role; anything unrecognized is
/// an external peer, never silently privileged.
fn role_of(role: &str) -> SwarmRole {
    match role {
        "ToolDriver" => SwarmRole::ToolDriver,
        "InferenceDriver" => SwarmRole::InferenceDriver,
        "PlannerCell" => SwarmRole::PlannerCell,
        _ => SwarmRole::ExternalPeer,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: susi-universal-cell <bind_addr> <manifest.json>");
        std::process::exit(1);
    }

    let bind_addr = &args[1];
    let manifest_path = &args[2];

    let manifest_str = tokio::fs::read_to_string(manifest_path).await?;
    let manifest: PluginManifest = serde_json::from_str(&manifest_str)?;

    println!(
        "susi-universal-cell managing ecosystem plugin '{}' on {}",
        manifest.name, bind_addr
    );

    let role = role_of(&manifest.role);

    let mut cell = SwarmCell::new(
        format!("universal-{}", manifest.name),
        role,
        format!("tcp://{}", bind_addr),
    );

    for cap in &manifest.capabilities {
        cell.register_capability(cap);
    }

    let manifest = Arc::new(manifest);

    // Every syscall must present the token from SUSI_CELL_TOKEN; without
    // it the cell denies all syscalls. Heartbeats stay open for liveness.
    let expected_token: Arc<str> = std::env::var(CELL_TOKEN_ENV).unwrap_or_default().into();
    if expected_token.trim().is_empty() {
        eprintln!("SUSI_CELL_TOKEN is not set: every syscall will be denied");
    }

    cell_server::serve(
        cell,
        bind_addr,
        expected_token,
        "Universal Ecosystem Cell",
        move |req| {
            let manifest = Arc::clone(&manifest);
            async move { handle_external(req, &manifest).await }
        },
    )
    .await?;
    Ok(())
}

async fn handle_external(req: SyscallRequest, manifest: &PluginManifest) -> SyscallResponse {
    // For MCP stdio protocols or CLI agents, we proxy the JSON request via stdin.
    // As a generic adapter, we just shell out with the payload.
    let payload_str = serde_json::to_string(&req.payload).unwrap_or_default();

    let mut cmd = Command::new(&manifest.command);
    for arg in &manifest.args {
        cmd.arg(arg);
    }
    cmd.arg(&payload_str);

    let started = std::time::Instant::now();
    match cmd.output().await {
        Ok(out) => {
            let result_text = String::from_utf8_lossy(&out.stdout).to_string();
            let stderr_text = String::from_utf8_lossy(&out.stderr).to_string();

            SyscallResponse {
                id: req.id,
                status: if out.status.success() {
                    SyscallStatus::Success
                } else {
                    SyscallStatus::Error
                },
                data: serde_json::json!({ "stdout": result_text, "stderr": stderr_text, "code": out.status.code() }),
                receipt: None,
                latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                message: if out.status.success() {
                    None
                } else {
                    Some("Ecosystem execution failed".into())
                },
            }
        }
        Err(e) => SyscallResponse {
            id: req.id,
            status: SyscallStatus::Error,
            data: serde_json::json!({ "error": e.to_string() }),
            receipt: None,
            latency_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
            message: Some(format!("Failed to spawn ecosystem plugin: {}", e)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_deserializes_full_shape() {
        let m: PluginManifest = serde_json::from_str(
            r#"{"name":"aws","role":"ToolDriver","capabilities":["aws.ec2"],"command":"aws","args":["--json"]}"#,
        )
        .unwrap();
        assert_eq!(m.name, "aws");
        assert_eq!(m.capabilities, vec!["aws.ec2"]);
        assert_eq!(m.command, "aws");
    }

    #[test]
    fn manifest_rejects_malformed_json() {
        assert!(serde_json::from_str::<PluginManifest>("{}").is_err());
        assert!(serde_json::from_str::<PluginManifest>("not json").is_err());
    }

    #[test]
    fn known_roles_map_to_their_swarm_role() {
        assert!(matches!(role_of("ToolDriver"), SwarmRole::ToolDriver));
        assert!(matches!(
            role_of("InferenceDriver"),
            SwarmRole::InferenceDriver
        ));
        assert!(matches!(role_of("PlannerCell"), SwarmRole::PlannerCell));
    }

    #[test]
    fn unknown_roles_fall_back_to_external_peer() {
        for bad in ["", "root", "Admin", "tooldriver", "SUPERUSER"] {
            assert!(matches!(role_of(bad), SwarmRole::ExternalPeer), "{bad:?}");
        }
    }

    fn manifest(command: &str, args: &[&str]) -> PluginManifest {
        PluginManifest {
            name: "test".to_string(),
            role: "ToolDriver".to_string(),
            capabilities: vec![],
            command: command.to_string(),
            args: args.iter().map(|a| (*a).to_string()).collect(),
        }
    }

    fn request(payload: serde_json::Value) -> SyscallRequest {
        SyscallRequest {
            id: "req-1".to_string(),
            caller_id: "test".to_string(),
            op: susi_abi::syscall::SyscallOp::ToolCall,
            token: None,
            workspace: None,
            payload,
            timestamp: 0,
        }
    }

    #[tokio::test]
    async fn handle_external_returns_stdout_on_success() {
        let m = manifest("echo", &[]);
        let res = handle_external(request(serde_json::json!({"k": 1})), &m).await;
        assert_eq!(res.status, SyscallStatus::Success);
        assert_eq!(res.id, "req-1");
        // The JSON payload is appended as the final argument — echo prints it.
        assert!(res.data["stdout"].as_str().unwrap().contains("\"k\":1"));
        assert!(res.message.is_none());
    }

    #[tokio::test]
    async fn handle_external_marks_nonzero_exit_as_error() {
        let m = manifest("false", &[]);
        let res = handle_external(request(serde_json::json!({})), &m).await;
        assert_eq!(res.status, SyscallStatus::Error);
        assert_eq!(res.message.as_deref(), Some("Ecosystem execution failed"));
    }

    #[tokio::test]
    async fn handle_external_surfaces_spawn_failure() {
        let m = manifest("/nonexistent-susi-plugin-xyz", &[]);
        let res = handle_external(request(serde_json::json!({})), &m).await;
        assert_eq!(res.status, SyscallStatus::Error);
        assert!(res
            .message
            .as_deref()
            .unwrap_or("")
            .contains("Failed to spawn ecosystem plugin"));
    }

    #[tokio::test]
    async fn handle_external_propagates_stderr() {
        let m = manifest("sh", &["-c", "echo out; echo err >&2; exit 3"]);
        let res = handle_external(request(serde_json::json!({})), &m).await;
        assert_eq!(res.status, SyscallStatus::Error);
        assert!(res.data["stdout"].as_str().unwrap().contains("out"));
        assert!(res.data["stderr"].as_str().unwrap().contains("err"));
        assert_eq!(res.data["code"], serde_json::json!(3));
    }
}
