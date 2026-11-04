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

#[cfg(test)]
mod tests {
    use super::hardened_container_config;

    /// Every isolation field, regardless of the command or image.
    fn assert_hardened(image: &str, cmd: &str) {
        let cfg = hardened_container_config(image.to_string(), cmd);
        assert_eq!(cfg.image.as_deref(), Some(image));
        assert_eq!(cfg.network_disabled, Some(true));
        // The command is one opaque `sh -c` argument: never split, so a
        // pipeline or quoting cannot become extra argv entries.
        assert_eq!(
            cfg.cmd,
            Some(vec!["sh".to_string(), "-c".to_string(), cmd.to_string()])
        );
        let host = cfg.host_config.expect("host config");
        assert_eq!(host.network_mode.as_deref(), Some("none"));
        assert_eq!(host.readonly_rootfs, Some(true));
        assert_eq!(host.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(host.memory, Some(256 * 1024 * 1024));
        assert_eq!(host.nano_cpus, Some(500_000_000));
        assert_eq!(host.pids_limit, Some(128));
        assert_eq!(host.auto_remove, Some(true));
        assert_eq!(
            host.security_opt,
            Some(vec!["no-new-privileges:true".to_string()])
        );
        // Nothing beyond the contract is granted.
        assert_eq!(host.privileged, None);
        assert_eq!(host.binds, None);
        assert_eq!(host.cap_add, None);
    }

    #[test]
    fn hardening_holds_for_empty_commands_pipelines_and_custom_images() {
        assert_hardened("susi-sandbox:latest", "");
        assert_hardened("susi-sandbox:latest", "echo 'a b' | wc -c");
        assert_hardened("registry.local:5000/team/sandbox:1.2.3", "id; uname -a");
        assert_hardened("alpine@sha256:0123abcd", "sh -c 'nested \"quotes\"'");
    }

    #[test]
    fn hardened_container_config_preserves_multiline_unicode_and_leaves_extra_grants_unset() {
        let tricky = "printf '%s\\n' \"🚀 line1\"\n$(echo subshell) && exit 0";
        let cfg = hardened_container_config("susi-sandbox:strict".to_string(), tricky);
        assert_eq!(
            cfg.cmd,
            Some(vec!["sh".to_string(), "-c".to_string(), tricky.to_string(),])
        );
        assert_eq!(cfg.env, None);
        assert_eq!(cfg.volumes, None);
        assert_eq!(cfg.exposed_ports, None);
        assert_eq!(cfg.user, None);
    }

    #[test]
    fn hardened_container_config_enforces_exact_resource_bounds_and_omits_host_exposure() {
        let cfg = hardened_container_config("susi-sandbox:bounds".to_string(), "true");
        let host = cfg.host_config.expect("host config");
        assert_eq!(host.memory, Some(256 * 1024 * 1024));
        assert_eq!(host.nano_cpus, Some(500_000_000));
        assert_eq!(host.pids_limit, Some(128));
        assert_eq!(host.port_bindings, None);
        assert_eq!(host.extra_hosts, None);
        assert_eq!(host.devices, None);
        assert_eq!(host.dns, None);
    }

    #[test]
    fn hardened_container_config_serializes_to_json_with_expected_docker_wire_keys() {
        let cfg = hardened_container_config("susi-sandbox:wire".to_string(), "echo 1");
        let cloned = cfg.clone();
        assert_eq!(cloned, cfg);

        let dbg = format!("{cfg:?}");
        assert!(dbg.contains("susi-sandbox:wire"));
        assert!(dbg.contains("echo 1"));
        assert!(dbg.contains("no-new-privileges:true"));

        assert_eq!(cfg.image.as_deref(), Some("susi-sandbox:wire"));
        assert_eq!(cfg.network_disabled, Some(true));
        assert_eq!(
            cfg.cmd,
            Some(vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo 1".to_string()
            ])
        );

        let host = cfg.host_config.expect("host config");
        assert_eq!(host.network_mode.as_deref(), Some("none"));
        assert_eq!(host.readonly_rootfs, Some(true));
        assert_eq!(host.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(host.memory, Some(268_435_456));
        assert_eq!(host.nano_cpus, Some(500_000_000));
        assert_eq!(host.pids_limit, Some(128));
        assert_eq!(host.auto_remove, Some(true));
        assert_eq!(
            host.security_opt,
            Some(vec!["no-new-privileges:true".to_string()])
        );
        assert_eq!(host.ipc_mode, None);
        assert_eq!(host.pid_mode, None);
        assert_eq!(host.uts_mode, None);
        assert_eq!(host.publish_all_ports, None);
    }

    #[test]
    fn hardened_container_config_keeps_cap_drop_all_and_auto_remove_across_distinct_commands() {
        let a = hardened_container_config("susi-sandbox:a".to_string(), "echo alpha");
        let b = hardened_container_config("susi-sandbox:b".to_string(), "echo beta");
        assert_ne!(a.image, b.image);
        assert_ne!(a.cmd, b.cmd);
        assert_eq!(a.host_config, b.host_config);
    }
}
