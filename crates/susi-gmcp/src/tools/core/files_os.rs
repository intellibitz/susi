//! CoreTools handlers and pipe helpers for workspace file I/O, shell execution, and OS inspection.

use std::io::Write;
use std::path::Path;
use std::process::Command;
use susi_config::JsonFields as _;

use super::CoreTools;
use crate::susi_core::plane_bus::gawd_hooks;
use crate::susi_error::{EaiError, EaiResult};
use crate::tools::helpers::{
    confine_exec_argv, os_ps_proc, os_sysinfo_proc, read_file_nofollow, secure_path,
    tool_string_arg,
};

impl CoreTools {
    pub fn read_file(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let arg_s = tool_string_arg(arg, &["path", "file", "filename"])?;
        let path = secure_path(workspace, &arg_s)?;
        read_file_nofollow(&path)
    }

    pub fn write_file(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let path_s = arg.field_str("path");
        let content_s = arg.field_str("content");

        if let (Some(p), Some(content)) = (path_s, content_s) {
            // confined_workspace_join resolves through the deepest existing
            // ancestor, so new parent dirs are allowed (secure_path demanded
            // the parent already exist, making the create_dir_all below dead
            // and `newdir/file` unwritable) while symlinks still cannot
            // escape the workspace.
            let dest = crate::susi_config::confined_workspace_join(workspace, p)
                .map_err(|e| EaiError::filesystem(e.to_string()))?;
            // Atomic replace: a crash mid-write never truncates the user's
            // file, and a symlink that raced in after confinement is refused
            // (rename never writes through it) instead of followed.
            crate::susi_config::atomic_replace_file(&dest, content.as_bytes())
                .map_err(|e| EaiError::filesystem(e.to_string()))?;
            Ok(format!("Wrote to {}", p))
        } else {
            Err(EaiError::protocol(
                "Usage: write_file {path: <path>, content: <content>}",
            ))
        }
    }

    pub fn exec_command(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let arg_s = tool_string_arg(arg, &["command", "cmd", "input"])?;
        let clean = arg_s.trim();
        if clean.is_empty() {
            return Err(EaiError::protocol("Usage: exec_command <cmd>"));
        }

        // Mandatory sandbox: redirect host exec into Docker isolation.
        if crate::susi_core::mac_policy::MacPolicy::global().mandatory_sandbox()
            && !crate::susi_core::mac_policy::MacPolicy::global().is_permitted(
                "susi",
                crate::susi_core::mac_policy::actions::PROCESS_EXEC,
                "*",
            )
        {
            gawd_hooks::audit_action("sandbox_exec", clean, workspace)?;
            return Self::shared_runtime()?
                .block_on(async {
                    // Requires the standalone `susi-sandbox` service (`127.0.0.1:18083`);
                    // bollard is isolated there — IPC fails clearly if the service is down.
                    crate::susi_sandbox::manager::SandboxManager::execute_in_docker(clean).await
                })
                .map_err(|e| {
                    EaiError::process(format!(
                        "[PRIVACY] mandatory sandbox: Docker execution failed: {e}. \
                     Start `susi-sandbox` (SUSI_SANDBOX_PORT) and ensure Docker is running."
                    ))
                });
        }

        gawd_hooks::audit_action("exec_command", clean, workspace)?;

        let task_handle = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task_scoped("exec_command", clean, arg.field_str("cancel_scope"));

        let args = susi_paths::shlex::split(clean)
            .ok_or_else(|| EaiError::protocol("Invalid shell syntax"))?;
        if args.is_empty() {
            task_handle.mark_failed("Command cannot be empty");
            return Err(EaiError::protocol("Command cannot be empty"));
        }
        if let Err(e) = confine_exec_argv(workspace, &args) {
            task_handle.mark_failed(&e.to_string());
            return Err(e);
        }

        println!("- [Substrate Operation] Executing: {}", clean);
        let _ = std::io::stdout().flush();
        task_handle.report_progress();

        let mut cmd = Command::new(&args[0]);
        cmd.args(&args[1..])
            .current_dir(workspace)
            // Never hand the daemon's stdin to an agent-issued command.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        susi_paths::console::apply_non_interactive_env(&mut cmd);
        susi_paths::console::without_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| {
            task_handle.mark_failed(&format!("Exec spawn failed: {}", e));
            EaiError::process(format!("Exec failed: {e}"))
        })?;

        // Drain both pipes concurrently: reading only after exit deadlocks
        // any command that writes more than a pipe buffer (~64 KiB) — the
        // child blocks on write and never exits.
        let mut stdout = Some(drain_exec_pipe(child.stdout.take(), task_handle.clone()));
        let mut stderr = Some(drain_exec_pipe(child.stderr.take(), task_handle.clone()));

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let exec_timeout = std::env::var("SUSI_EXEC_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|&s| s > 0)
            .map_or_else(
                || std::time::Duration::from_secs(300),
                std::time::Duration::from_secs,
            );
        let deadline = std::time::Instant::now() + exec_timeout;

        loop {
            task_handle.check_pause();
            if task_handle.is_cancelled() {
                let _ = child.kill();
                // Reap it: an unwaited child lingers as a zombie.
                let _ = child.wait();
                task_handle.mark_failed("Task cancelled or stalled");
                return Err(EaiError::process(
                    "Execution killed due to stall or cancel request".to_string(),
                ));
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                task_handle.mark_failed("Command timed out");
                return Err(EaiError::process(format!(
                    "Command timed out after {}s",
                    exec_timeout.as_secs()
                )));
            }

            let status = match child.try_wait() {
                Ok(status) => status,
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    task_handle.mark_failed(&format!("wait failed: {e}"));
                    return Err(EaiError::process(format!("Exec wait failed: {e}")));
                }
            };
            if let Some(status) = status {
                if let Some(reader) = stdout.take() {
                    stdout_buf = reader.join().unwrap_or_default();
                }
                if let Some(reader) = stderr.take() {
                    stderr_buf = reader.join().unwrap_or_default();
                }

                let stdout_str = String::from_utf8_lossy(&stdout_buf).to_string();
                let stderr_str = String::from_utf8_lossy(&stderr_buf).to_string();

                if !status.success() {
                    let err_msg = if stderr_str.is_empty() {
                        "Command failed with non-zero exit status".to_string()
                    } else {
                        stderr_str
                    };
                    task_handle.mark_failed(&err_msg);
                    return Err(EaiError::process(err_msg));
                } else {
                    task_handle.mark_completed(&stdout_str);
                    return Ok(stdout_str);
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    pub fn os_services(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let action = arg.field_str("action").unwrap_or("status");
        let name = arg.field_str("name").unwrap_or("");
        let audit = format!("{action} {name}").trim().to_string();
        gawd_hooks::audit_action("os_services", &audit, workspace)?;

        match action {
            "status" | "list" => {
                let statuses = crate::susi_core::service_table::status();
                serde_json::to_string_pretty(&statuses)
                    .map_err(|e| EaiError::protocol(e.to_string()))
            }
            "restart" => {
                if name.is_empty() {
                    return Err(EaiError::protocol(
                        "Usage: os_services {action: \"restart\", name: <service>}",
                    ));
                }
                let Some(svc) = crate::susi_core::service_table::leaf_service(name) else {
                    return Err(EaiError::protocol(format!("unknown service `{name}`")));
                };
                let table = crate::susi_core::service_table::load();
                let Some(rec) = table.iter().find(|r| r.name == svc.name) else {
                    return Err(EaiError::process(format!(
                        "{name} is not supervised by the daemon"
                    )));
                };
                // External rows are observability records, not children —
                // the agent surface must never signal a process the daemon
                // did not spawn.
                if rec.external {
                    return Err(EaiError::authorization(format!(
                        "{name} is bound by an external process the daemon does \
                         not supervise — restart is refused"
                    )));
                }
                let ok = Command::new("kill")
                    .args(["-TERM", &rec.pid.to_string()])
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok {
                    Ok(format!(
                        "sent SIGTERM to {} (pid {}); the daemon supervisor will respawn it",
                        svc.name, rec.pid
                    ))
                } else {
                    Err(EaiError::process(format!(
                        "failed to SIGTERM {} (pid {})",
                        svc.name, rec.pid
                    )))
                }
            }
            other => Err(EaiError::protocol(format!(
                "unknown action `{other}` (status|restart)"
            ))),
        }
    }

    pub fn os_ps(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let limit = arg.field_u64("limit").unwrap_or(50).min(500) as usize;
        let filter = arg.field_str("filter").unwrap_or("");
        gawd_hooks::audit_action("os_ps", filter, workspace)?;
        os_ps_proc(limit, filter)
    }

    pub fn os_sysinfo(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("os_sysinfo", "", workspace)?;
        os_sysinfo_proc()
    }

    pub fn os_kill(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let pid =
            arg.field_u64("pid")
                .ok_or_else(|| EaiError::protocol("Usage: os_kill {pid: <n>}"))? as u32;
        let signal = arg.field_str("signal").unwrap_or("term");
        let sig_name = match signal {
            "term" | "TERM" | "15" => "TERM",
            "kill" | "KILL" | "9" => "KILL",
            other => {
                return Err(EaiError::protocol(format!(
                    "signal `{other}` not permitted (term|kill)"
                )));
            }
        };
        let audit = format!("{sig_name} {pid}");
        gawd_hooks::audit_action("os_kill", &audit, workspace)?;

        // Fails closed: only pids the daemon itself supervises may be
        // signalled through this tool — an arbitrary-pid kill would be a
        // host-destruction primitive, not an OS-layer capability. External
        // rows (ports bound by processes the daemon did not spawn) are in
        // the table for observability but are explicitly NOT killable.
        let supervised = crate::susi_core::service_table::load();
        if !supervised.iter().any(|r| r.pid == pid && !r.external) {
            return Err(EaiError::authorization(format!(
                "pid {pid} is not a supervised leaf service — os_kill only \
                 reaches pids the daemon spawned"
            )));
        }
        let ok = Command::new("kill")
            .args([format!("-{sig_name}"), pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            Ok(format!("sent SIG{sig_name} to pid {pid}"))
        } else {
            Err(EaiError::process(format!("kill -{sig_name} {pid} failed")))
        }
    }

    pub fn os_logs(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let name = arg
            .field_str("name")
            .ok_or_else(|| EaiError::protocol("Usage: os_logs {name: <service>}"))?;
        let lines = arg.field_u64("lines").unwrap_or(50).min(500) as usize;
        gawd_hooks::audit_action("os_logs", name, workspace)?;
        // Only registered leaf services — a free-form name would read
        // arbitrary logs under substrate_home.
        let Some(svc) = crate::susi_core::service_table::leaf_service(name) else {
            return Err(EaiError::protocol(format!(
                "unknown service `{name}` (expected a leaf service name)"
            )));
        };
        let path = susi_paths::SusiDirs::substrate_home()
            .join("logs")
            .join(format!("{}.log", svc.name));
        // Tail-bounded read: a governed tool must not block on arbitrarily
        // large logs — cap at the last 512 KiB and drop the partial head line.
        const TAIL_BYTES: u64 = 512 * 1024;
        use std::io::{Read, Seek, SeekFrom};
        let mut file = std::fs::File::open(&path).map_err(|_| {
            EaiError::filesystem(format!(
                "no log at {} — service may never have been supervised",
                path.display()
            ))
        })?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        let truncated = size > TAIL_BYTES;
        if truncated {
            file.seek(SeekFrom::End(-(TAIL_BYTES as i64)))
                .map_err(|e| EaiError::filesystem(format!("seek log: {e}")))?;
        }
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)
            .map_err(|e| EaiError::filesystem(format!("read log: {e}")))?;
        let text = String::from_utf8_lossy(&buf);
        let all: Vec<&str> = text.lines().collect();
        let start = all.len().saturating_sub(lines).max(usize::from(truncated));
        let mut out = all[start..].join("\n");
        if truncated {
            out.insert_str(0, "… (log truncated to last 512 KiB)\n");
        }
        Ok(out)
    }
}

/// Drain one child pipe on its own thread, keeping at most 8 MiB. Output is
/// the command's only observable progress: each chunk read reports progress,
/// so the idle-lease watchdog (Mandate 32) sees a silent, hung command as
/// idle instead of a wait loop reporting progress on its behalf.
pub(crate) fn drain_exec_pipe(
    pipe: Option<impl std::io::Read + Send + 'static>,
    task: std::sync::Arc<crate::susi_core::task_manager::TaskHandle>,
) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        const CAP: usize = 8 * 1024 * 1024;
        let mut kept = Vec::new();
        let Some(mut pipe) = pipe else {
            return kept;
        };
        let mut chunk = [0u8; 8192];
        while let Ok(n) = pipe.read(&mut chunk) {
            if n == 0 {
                break;
            }
            task.report_progress();
            let room = CAP.saturating_sub(kept.len());
            kept.extend_from_slice(chunk.get(..n.min(room)).unwrap_or_default());
        }
        kept
    })
}
