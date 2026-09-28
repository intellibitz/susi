//! Docker execution for the `susi-sandbox` service. bollard is linked only
//! here; every other process reaches it through the sandbox client.

use bollard::models::{ContainerCreateBody, HostConfig};
use susi_config::SusiConfig;
use susi_error::{EaiError, EaiResult};

/// Hardened sandbox container contract: no network, drop capabilities,
/// read-only rootfs, memory cap, auto-remove. Authenticated callers still get
/// a shell inside the image — isolation limits blast radius, not intent.
/// Kept pure so the contract is testable without a docker daemon.
pub(crate) fn hardened_container_config(sandbox_image: String, cmd: &str) -> ContainerCreateBody {
    ContainerCreateBody {
        image: Some(sandbox_image),
        cmd: Some(vec!["sh".to_string(), "-c".to_string(), cmd.to_string()]),
        network_disabled: Some(true),
        host_config: Some(HostConfig {
            network_mode: Some("none".to_string()),
            readonly_rootfs: Some(true),
            cap_drop: Some(vec!["ALL".to_string()]),
            memory: Some(256 * 1024 * 1024),
            nano_cpus: Some(500_000_000), // 0.5 CPU
            // Fork bombs stay inside the container's pid budget.
            pids_limit: Some(128),
            auto_remove: Some(true),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Runs `cmd` in the hardened sandbox image and returns its captured output.
pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
    use bollard::container::LogOutput;
    use bollard::query_parameters::{
        CreateContainerOptions, LogsOptions, RemoveContainerOptions, StartContainerOptions,
    };
    use bollard::Docker;
    use futures::stream::StreamExt;

    let docker = Docker::connect_with_local_defaults()
        .map_err(|e| EaiError::process(format!("Docker connection failed: {}", e)))?;

    let sandbox_image = SusiConfig::load_global()
        .unwrap_or_default()
        .sandbox_image();
    let config = hardened_container_config(sandbox_image, cmd);

    let container = docker
        .create_container(None::<CreateContainerOptions>, config)
        .await
        .map_err(|e| EaiError::process(format!("Container creation failed: {}", e)))?;

    docker
        .start_container(&container.id, None::<StartContainerOptions>)
        .await
        .map_err(|e| EaiError::process(format!("Container start failed: {}", e)))?;

    let mut logs = docker.logs(
        &container.id,
        Some(LogsOptions {
            stdout: true,
            stderr: true,
            follow: true,
            ..Default::default()
        }),
    );
    // Bound wall time and captured output: a `sleep infinity` or an
    // endless printer must not hold the request (or daemon memory) open.
    const MAX_RUNTIME: std::time::Duration = std::time::Duration::from_secs(120);
    const MAX_OUTPUT: usize = 1024 * 1024;
    let mut output = String::new();
    let collect = async {
        while let Some(log) = logs.next().await {
            match log {
                Ok(LogOutput::StdOut { message }) | Ok(LogOutput::StdErr { message }) => {
                    output.push_str(&String::from_utf8_lossy(&message));
                    if output.len() > MAX_OUTPUT {
                        output.truncate(MAX_OUTPUT);
                        output.push_str("\n[output truncated at 1 MiB]");
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    output.push_str(&format!("\n[docker log error: {e}]"));
                    break;
                }
            }
        }
    };
    if tokio::time::timeout(MAX_RUNTIME, collect).await.is_err() {
        output.push_str("\n[sandbox command timed out after 120s; container killed]");
    }

    // Best-effort cleanup if auto_remove did not fire (e.g. never started).
    let _ = docker
        .remove_container(
            &container.id,
            Some(RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;

    Ok(output)
}
