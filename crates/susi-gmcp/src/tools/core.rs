//! Concrete MCP/meta tool handlers (`CoreTools`).

use rmcp::tool;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use crate::susi_core::broker::{IpcBroker, PermissionScope};
use crate::susi_core::context_graph::ContextGraph;
use crate::susi_core::plane_bus::gemi::sample_telemetry;
use crate::susi_core::plane_bus::gemi::HardwareProfiler;
use crate::susi_core::plane_bus::gemi::ModelManager;
use crate::susi_core::plane_bus::{agents, gawd, gawd_hooks, tools as plane_tools};
use crate::susi_error::{EaiError, EaiResult};
use crate::tool_registry::GmcpClient;

#[cfg(feature = "tools-rich")]
use headless_chrome::Browser;

#[cfg(feature = "tools-rich")]
use super::helpers::secure_external_url;
use super::helpers::{
    confine_exec_argv, external_agent_control, os_ps_proc, os_sysinfo_proc, read_file_nofollow,
    secure_path, tool_string_arg,
};

pub struct CoreTools;

impl CoreTools {
    /// Shared long-lived tokio runtime for tool functions that must bridge into
    /// async APIs (Docker/Qdrant clients). Avoids constructing/tearing down a
    /// fresh multi-thread runtime on every call (Mandate 28: Async Defaults).
    pub(crate) fn shared_runtime() -> EaiResult<&'static tokio::runtime::Runtime> {
        static RT: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
        RT.get_or_init(tokio::runtime::Runtime::new)
            .as_ref()
            .map_err(|e| EaiError::process(format!("Failed to start shared tokio runtime: {}", e)))
    }

    #[tool(name = "status", description = "SUSI Substrate status report")]
    pub fn status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let hardware = HardwareProfiler::get_profile();
        let mut out = format!("SUSI Engine Version: {}\n", env!("CARGO_PKG_VERSION"));
        out.push_str(&format!(
            "System Environment: {} CPUs | RAM: {}GB | {}\n",
            hardware.cpus, hardware.ram_gb, hardware.gpu_info
        ));

        // Report Background Provisioning Progress
        let progress_file = crate::susi_paths::SusiDirs::data_dir().join("download_progress.json");
        if progress_file.exists() {
            if let Ok(content) = fs::read_to_string(&progress_file) {
                if let Ok(progress) = serde_json::from_str::<serde_json::Value>(&content) {
                    if progress.get("status").and_then(|v| v.as_str()) == Some("IN_PROGRESS") {
                        out.push_str("\n[SUBSTRATE PROVISIONING ACTIVE]\n");
                        if let Some(name) = progress.get("model_name").and_then(|v| v.as_str()) {
                            out.push_str(&format!("- Target: {name}\n"));
                        }
                        let pct = progress
                            .get("percentage")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(0.0);
                        let downloaded = progress
                            .get("bytes_downloaded")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as f32
                            / 1e9;
                        let expected = progress
                            .get("expected_bytes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0) as f32
                            / 1e9;
                        out.push_str(&format!(
                            "- Progress: {pct:.2}% ({downloaded:.2}GB / {expected:.2}GB)\n"
                        ));
                    }
                }
            }
        }

        out.push_str("\nStatus: Operational.\n");
        Ok(out)
    }

    #[tool(
        name = "sovereign_dashboard",
        description = "Report on autonomous invisible work performed by the substrate"
    )]
    pub fn sovereign_dashboard(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let log_content =
            crate::susi_sandbox::manager::SusiAuditLogger::read_audit_log(workspace, 100);
        let mut report = "# SUSI Sovereign Dashboard - Invisible Work Audit\n\n".to_string();

        let mut self_heals = 0;
        let mut security_hardens = 0;
        let mut optimizations = 0;
        let mut memory_distillations = 0;

        for line in log_content.lines() {
            if line.contains("SELF_HEALING") {
                self_heals += 1;
            }
            if line.contains("SECURITY_HARDENING") || line.contains("MASKED") {
                security_hardens += 1;
            }
            if line.contains("OPTIMIZATION") || line.contains("BLOAT_REJECTION") {
                optimizations += 1;
            }
            if line.contains("MEMORY_CONSOLIDATION") {
                memory_distillations += 1;
            }
        }

        report.push_str(&format!("- **Autonomous Self-Heals**: {}\n", self_heals));
        report.push_str(&format!(
            "- **Security Hardening Pulses**: {}\n",
            security_hardens
        ));
        report.push_str(&format!(
            "- **Bloat Rejection Optimizations**: {}\n",
            optimizations
        ));
        report.push_str(&format!(
            "- **Neural Memory Distillations**: {}\n\n",
            memory_distillations
        ));

        report.push_str("### Recent Autonomous Activity Trace:\n");
        for line in log_content.lines().rev().take(10) {
            if let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) {
                let ts = entry["ts"].as_u64().unwrap_or(0);
                let ev_type = entry["type"].as_str().unwrap_or("INFO");
                let details = entry["details"].as_str().unwrap_or("");
                report.push_str(&format!("- [{}] **{}**: {}\n", ts, ev_type, details));
            }
        }

        Ok(report)
    }

    #[tool(
        name = "bloat_audit",
        description = "Recursively audit src/ (AST-based) and target/ (build artifact size) for bloat and hardcoded secrets, rayon-parallel across all cores"
    )]
    pub fn bloat_audit(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd::bloat_audit(workspace).map_err(EaiError::governance)
    }

    #[tool(name = "identity", description = "SUSI substrate identity report")]
    pub fn identity(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd::identity_report(workspace).map_err(EaiError::governance)
    }

    #[tool(
        name = "distill_genome",
        description = "Distill the hard-compiled genome into the Tier 2 reasoning model"
    )]
    pub fn distill_genome(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        match gawd::audit_reasoning_substrate(workspace) {
            Ok(report) => Ok(format!("# Genome Distillation Successful\n\n{}", report)),
            Err(e) => Ok(format!("# Genome Distillation Failed\n\nError: {}", e)),
        }
    }

    #[tool(
        name = "self_validate",
        description = "Execute autonomous substrate self-validation"
    )]
    pub fn self_validate(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        match gawd::self_validate(workspace) {
            Ok(report) => Ok(format!(
                "# Substrate Self-Validation Successful\n\n{}",
                report
            )),
            Err(e) => Ok(format!(
                "# Substrate Self-Validation Failed\n\nError: {}",
                e
            )),
        }
    }

    #[tool(name = "list_models", description = "List available model substrates")]
    pub fn list_models(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let models = ModelManager::list_models(workspace);
        let count = models.as_array().map(|a| a.len()).unwrap_or(0);
        Ok(format!(
            "Active Model Substrates (Count: {count})\n\n{models}"
        ))
    }

    #[tool(
        name = "select_model",
        description = "Select or override active model substrate"
    )]
    pub fn select_model(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let arg_s = arg.as_str().unwrap_or("");
        if arg_s.trim().is_empty() {
            return Ok("Usage: select_model <model_name_or_id>".to_string());
        }
        let v = ModelManager::set_selected_model(arg_s.trim());
        if let Some(e) = v.get("error").and_then(|x| x.as_str()) {
            return Err(EaiError::config(e));
        }
        Ok(v.get("text")
            .and_then(|x| x.as_str())
            .unwrap_or("model selected")
            .to_string())
    }

    #[tool(name = "scout_model", description = "Scout or install model substrate")]
    pub fn scout_model(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let arg_s = arg.as_str().unwrap_or("");
        if arg_s.trim().is_empty() {
            return Ok("Usage: scout_model <model_name_or_url>".to_string());
        }
        let res = ModelManager::install_model(arg_s.trim());
        Ok(res
            .get("text")
            .and_then(|x| x.as_str())
            .unwrap_or(&res.to_string())
            .to_string())
    }

    #[tool(
        name = "verify_model_download_agent",
        description = "Verify model download agent, check network status, and ensure 32b and 72b models are provisioned"
    )]
    pub fn verify_model_download_agent(
        _arg: &serde_json::Value,
        workspace: &Path,
    ) -> EaiResult<String> {
        let report = ModelManager::verify_local_models(workspace);
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| report.to_string()))
    }

    #[tool(
        name = "train_reflexes",
        description = "Manually trigger native neural reflex distillation"
    )]
    pub fn train_reflexes(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd::train_reflexes(workspace).map_err(EaiError::governance)
    }

    #[tool(
        name = "swarm_schedule",
        description = "Show recent mission scheduler decisions: per-agent scores, admitted vs deferred"
    )]
    pub fn swarm_schedule(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let decisions = gawd::scheduler_recent_decisions(50);
        if decisions.as_array().map(|a| a.is_empty()).unwrap_or(true) {
            return Ok("No missions scheduled yet.".to_string());
        }
        Ok(serde_json::to_string_pretty(&decisions).unwrap_or_else(|_| decisions.to_string()))
    }

    #[tool(name = "read_file", description = "Read file content in workspace")]
    pub fn read_file(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let arg_s = tool_string_arg(arg, &["path", "file", "filename"])?;
        let path = secure_path(workspace, &arg_s)?;
        read_file_nofollow(&path)
    }

    #[tool(name = "write_file", description = "Write content to workspace file")]
    pub fn write_file(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let path_s = arg.get("path").and_then(|v| v.as_str());
        let content_s = arg.get("content").and_then(|v| v.as_str());

        if let (Some(p), Some(content)) = (path_s, content_s) {
            let dest = secure_path(workspace, p)?;
            if let Some(parent) = dest.parent() {
                let _ = fs::create_dir_all(parent);
            }
            // O_NOFOLLOW (Unix): refuse to open if a symlink raced in after secure_path.
            #[cfg(unix)]
            {
                use std::io::Write;
                use std::os::unix::fs::OpenOptionsExt;
                let mut options = fs::OpenOptions::new();
                options.write(true).create(true).truncate(true);
                options.custom_flags(libc::O_NOFOLLOW);
                let mut file = options
                    .open(&dest)
                    .map_err(|e| EaiError::filesystem(e.to_string()))?;
                file.write_all(content.as_bytes())
                    .map_err(|e| EaiError::filesystem(e.to_string()))?;
            }
            #[cfg(not(unix))]
            {
                fs::write(&dest, content).map_err(|e| EaiError::filesystem(e.to_string()))?;
            }
            Ok(format!("Wrote to {}", p))
        } else {
            Err(EaiError::protocol(
                "Usage: write_file {path: <path>, content: <content>}",
            ))
        }
    }

    #[tool(name = "exec_command", description = "Execute command in workspace")]
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
            gawd_hooks::audit_action("sandbox_exec", clean, workspace)
                .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
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

        gawd_hooks::audit_action("exec_command", clean, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;

        let task_handle = crate::susi_core::task_manager::SwarmTaskManager::global()
            .register_task("exec_command", clean);

        let args = shlex::split(clean).ok_or_else(|| EaiError::protocol("Invalid shell syntax"))?;
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

        let mut child = Command::new(&args[0])
            .args(&args[1..])
            .env("GIT_TERMINAL_PROMPT", "0")
            .current_dir(workspace)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| {
                task_handle.mark_failed(&format!("Exec spawn failed: {}", e));
                EaiError::process(format!("Exec failed: {}", e))
            })?;

        // Drain both pipes concurrently: reading only after exit deadlocks
        // any command that writes more than a pipe buffer (~64 KiB) — the
        // child blocks on write and never exits. Keep at most 8 MiB each.
        fn drain(
            pipe: Option<impl std::io::Read + Send + 'static>,
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
                    let room = CAP.saturating_sub(kept.len());
                    kept.extend_from_slice(chunk.get(..n.min(room)).unwrap_or_default());
                }
                kept
            })
        }
        let mut stdout = Some(drain(child.stdout.take()));
        let mut stderr = Some(drain(child.stderr.take()));

        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();

        loop {
            task_handle.check_pause();
            if task_handle.is_cancelled() {
                let _ = child.kill();
                task_handle.mark_failed("Task cancelled or stalled");
                return Err(EaiError::process(
                    "Execution killed due to stall or cancel request".to_string(),
                ));
            }

            if let Ok(Some(status)) = child.try_wait() {
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

            task_handle.report_progress();
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[tool(
        name = "os_services",
        description = "List the substrate's leaf services (susi-paths/error/config/sandbox/native) with supervised pids and live health; action=restart <name> SIGTERMs a supervised service for the daemon to respawn"
    )]
    pub fn os_services(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let action = arg
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("status");
        let name = arg.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let audit = format!("{action} {name}").trim().to_string();
        gawd_hooks::audit_action("os_services", &audit, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;

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

    #[tool(
        name = "os_ps",
        description = "List host processes (pid, name, RSS) read from /proc, sorted by memory; args: {limit?: N, filter?: substring}"
    )]
    pub fn os_ps(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let limit = arg
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(500) as usize;
        let filter = arg.get("filter").and_then(|v| v.as_str()).unwrap_or("");
        gawd_hooks::audit_action("os_ps", filter, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        os_ps_proc(limit, filter)
    }

    #[tool(
        name = "os_sysinfo",
        description = "Host summary from /proc: kernel, uptime, load average, memory totals, cpu count"
    )]
    pub fn os_sysinfo(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("os_sysinfo", "", workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        os_sysinfo_proc()
    }

    #[tool(
        name = "os_kill",
        description = "Send TERM or KILL to a pid the substrate supervises (daemon-spawned service pids only — fails closed on any other pid, including external table rows); args: {pid: N, signal?: \"term\"|\"kill\"}"
    )]
    pub fn os_kill(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let pid =
            arg.get("pid")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| EaiError::protocol("Usage: os_kill {pid: <n>}"))? as u32;
        let signal = arg.get("signal").and_then(|v| v.as_str()).unwrap_or("term");
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
        gawd_hooks::audit_action("os_kill", &audit, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;

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

    #[tool(
        name = "os_logs",
        description = "Tail a supervised leaf service's log (substrate_home/logs/<name>.log); args: {name: string, lines?: N (default 50, max 500)}"
    )]
    pub fn os_logs(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let name = arg
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Usage: os_logs {name: <service>}"))?;
        let lines = arg
            .get("lines")
            .and_then(|v| v.as_u64())
            .unwrap_or(50)
            .min(500) as usize;
        gawd_hooks::audit_action("os_logs", name, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        // Only registered leaf services — a free-form name would read
        // arbitrary logs under substrate_home.
        let Some(svc) = crate::susi_core::service_table::leaf_service(name) else {
            return Err(EaiError::protocol(format!(
                "unknown service `{name}` (expected a leaf service name)"
            )));
        };
        let path = crate::susi_paths::SusiDirs::substrate_home()
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

    #[tool(
        name = "commit_record",
        description = "Accept a replicated swarm commit record. Args: the CommitRecord JSON object. Verifies the coordinator's cluster-key signature and internal consistency before appending to the local ledger — unsigned/forged records fail closed."
    )]
    pub fn commit_record(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("commit_record", &arg.to_string(), workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let record: crate::susi_core::commit_log::CommitRecord =
            serde_json::from_value(arg.clone())
                .map_err(|e| EaiError::protocol(format!("bad commit record: {e}")))?;
        // verify() runs again inside append(); call it here so the error
        // names the failure (forgery vs. ledger IO) rather than the generic
        // append refusal.
        if !record.verify() {
            return Err(EaiError::authorization(
                "commit record failed cluster-key signature or quorum consistency checks",
            ));
        }
        // Member records need coordinator authority, not just a valid
        // HMAC: an evicted node still holds cluster.key, so signature
        // alone cannot authorize roster changes (the rogue-eviction
        // hole). Refuse records sealed by non-members — they stay
        // missing and converge via pull once the coordinator is known.
        if !crate::susi_core::commit_log::member_coordinator_known(&record) {
            return Err(EaiError::authorization(format!(
                "member record coordinator {} is not an explicit member of this roster",
                record.coordinator
            )));
        }
        // Term gate BEFORE append (VC-200-001): a pushed record from a
        // stale term means the coordinator is working from superseded
        // leadership — reject it (Raft's AppendEntries term check).
        // History fills via repair_commit_gap bypass this gate by
        // calling append() directly — terms gate new writes, not the
        // log's past. Only member coordinators may drive term state:
        // an evicted node's forged high term must not adopt into
        // term.json (it would freeze every honest push as Stale).
        let term_note = if !crate::susi_core::commit_log::coordinator_known(&record) {
            String::new()
        } else {
            match crate::susi_core::commit_log::check_term(&record) {
                crate::susi_core::commit_log::TermVerdict::Stale => {
                    return Err(EaiError::protocol(format!(
                        "stale term {} (current {}): coordinator {} is working from superseded leadership",
                        record.term,
                        crate::susi_core::commit_log::load_term().term,
                        record.coordinator
                    )));
                }
                crate::susi_core::commit_log::TermVerdict::Adopted(s) => {
                    format!(" — adopted term {} (leader {})", s.term, s.leader)
                }
                crate::susi_core::commit_log::TermVerdict::LeaderConflict => {
                    format!(
                        " — WARNING: term {} leader conflict ({} vs persisted {})",
                        record.term,
                        record.leader,
                        crate::susi_core::commit_log::load_term().leader
                    )
                }
                crate::susi_core::commit_log::TermVerdict::Current => String::new(),
            }
        };
        // Replication-gap check BEFORE append: a seq that skips ahead of
        // what this node holds means commits from this coordinator were
        // lost in transit — repair by pulling the missing records from
        // the coordinator's own ledger (anti-entropy), then report.
        let held = crate::susi_core::commit_log::load();
        let floor = crate::susi_core::commit_log::snapshot_floor(&record.coordinator);
        let missing = crate::susi_core::commit_log::missing_seqs_floored(
            &held,
            &record.coordinator,
            record.seq,
            floor,
        );
        crate::susi_core::commit_log::append(&record)?;
        let mut out = format!(
            "commit {} accepted ({} / {} voters, seq {}, term {})",
            record.epoch.get(..12).unwrap_or(&record.epoch),
            record.tally,
            record.electorate.len(),
            record.seq,
            record.term
        );
        out.push_str(&term_note);
        if !missing.is_empty() {
            match repair_commit_gap(&record.coordinator, &missing) {
                Ok(repaired) if repaired == missing.len() => {
                    out.push_str(&format!(
                        " — repaired {} missing record(s) from {}",
                        repaired, record.coordinator
                    ));
                }
                Ok(repaired) => {
                    out.push_str(&format!(
                        " — WARNING: replication gap, missing seq {:?} from {} (repaired {})",
                        missing, record.coordinator, repaired
                    ));
                }
                Err(e) => {
                    out.push_str(&format!(
                        " — WARNING: replication gap, missing seq {:?} from {} (repair failed: {})",
                        missing, record.coordinator, e
                    ));
                }
            }
        }
        Ok(out)
    }

    #[tool(
        name = "commit_records",
        description = "Batch intake of replicated commit records for anti-entropy pushes. Args: {records: [CommitRecord, ...]} (max 1000) — every record re-runs signature, coordinator-membership, and term gates; the accepted set appends in one ledger pass, and any remaining sequence gap is repaired from the coordinator once. Returns an applied/skipped/refused summary."
    )]
    pub fn commit_records(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("commit_records", &arg.to_string(), workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let records: Vec<crate::susi_core::commit_log::CommitRecord> = serde_json::from_value(
            arg.get("records")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        )
        .map_err(|e| EaiError::protocol(format!("bad commit records array: {e}")))?;
        // Bound the batch — an unbounded array is an unbounded
        // signature-verification workload on one RPC.
        const MAX_BATCH: usize = 1000;
        if records.len() > MAX_BATCH {
            return Err(EaiError::protocol(format!(
                "batch of {} exceeds the {MAX_BATCH}-record cap",
                records.len()
            )));
        }
        // Same per-record gates as commit_record: signature, coordinator
        // membership (an evicted node still holds cluster.key), and the
        // AppendEntries term check. Adopted/conflict verdicts ride the
        // same check_term path — only Stale refuses.
        let mut accepted: Vec<crate::susi_core::commit_log::CommitRecord> = Vec::new();
        let mut refused = 0usize;
        for r in &records {
            if !r.verify()
                || !crate::susi_core::commit_log::member_coordinator_known(r)
                || (crate::susi_core::commit_log::coordinator_known(r)
                    && matches!(
                        crate::susi_core::commit_log::check_term(r),
                        crate::susi_core::commit_log::TermVerdict::Stale
                    ))
            {
                refused += 1;
                continue;
            }
            accepted.push(r.clone());
        }
        // One lock + one ledger load for the whole batch — the append
        // path's full validation still runs per record.
        let mut applied = 0usize;
        let mut skipped = 0usize;
        for outcome in crate::susi_core::commit_log::append_many(&accepted) {
            match outcome {
                crate::susi_core::commit_log::AppendOutcome::Applied => applied += 1,
                crate::susi_core::commit_log::AppendOutcome::Skipped => skipped += 1,
                crate::susi_core::commit_log::AppendOutcome::Refused => refused += 1,
            }
        }
        // Gap repair once per coordinator (the single-record path runs it
        // per record — a batch hitting seq jumps would recurse N times).
        // Compute each coordinator's max accepted seq, then pull whatever
        // between it and our high-water is still missing.
        let held = crate::susi_core::commit_log::load();
        let mut max_seq: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
        for r in &accepted {
            let e = max_seq.entry(r.coordinator.as_str()).or_insert(0);
            *e = (*e).max(r.seq);
        }
        let mut repaired = 0usize;
        for (coordinator, hi) in max_seq {
            let floor = crate::susi_core::commit_log::snapshot_floor(coordinator);
            let missing =
                crate::susi_core::commit_log::missing_seqs_floored(&held, coordinator, hi, floor);
            if !missing.is_empty() {
                repaired += repair_commit_gap(coordinator, &missing).unwrap_or(0);
            }
        }
        // Machine-readable summary — pushers get real counts instead of
        // scraping a human sentence.
        Ok(serde_json::json!({
            "applied": applied,
            "skipped": skipped,
            "refused": refused,
            "repaired": repaired,
        })
        .to_string())
    }

    /// Resolve an `a2a_delegate` target to `http://{ip}:{port}`. Only
    /// loopback and verified roster members resolve — an agent-supplied
    /// URL otherwise becomes an SSRF primitive posting signed requests
    /// at arbitrary hosts.
    fn a2a_peer_url(peer: &str) -> EaiResult<String> {
        use crate::susi_config::cluster_key;
        use std::net::IpAddr;
        // Effective A2A port — canonical + port_offset. Used both for the
        // local "self" route and as the same-config guess for bare hosts.
        let a2a_port = crate::susi_config::SusiConfig::load_global()
            .map(|c| c.a2a_http_port())
            .unwrap_or(crate::susi_paths::ports::A2A_HTTP);
        let p = peer.trim();
        if p.eq_ignore_ascii_case("self") || p == "localhost" || p == "127.0.0.1" || p == "::1" {
            return Ok(format!("http://127.0.0.1:{a2a_port}"));
        }
        // Split an explicit http(s) URL into host(+port); a bare host[:port]
        // gets the canonical A2A port.
        let stripped = p
            .strip_prefix("http://")
            .or_else(|| p.strip_prefix("https://"))
            .unwrap_or(p);
        let hostport = stripped.split('/').next().unwrap_or("");
        let (host, port) =
            hostport
                .rsplit_once(':')
                .map_or((hostport, a2a_port), |(h, ps)| match ps.parse::<u16>() {
                    Ok(n) => (h, n),
                    Err(_) => (hostport, a2a_port),
                });
        // Operator-configured external A2A agents form a second, explicit
        // allowlist: `external_peer_agents` entries with protocol "a2a" are
        // reachable by name (resolving to their api_base) or by a target
        // URL matching a configured api_base's host[:port]. Runs before the
        // IpAddr parse because configured peers may use DNS names.
        if let Ok(cfg) = crate::susi_config::SusiConfig::load_global() {
            for spec in cfg.external_peer_agents() {
                if spec.protocol != "a2a" || spec.api_base.is_empty() {
                    continue;
                }
                if spec.name == p {
                    return Ok(spec.api_base.trim_end_matches('/').to_string());
                }
                let base = spec.api_base.trim_end_matches('/');
                let base_host = base
                    .strip_prefix("http://")
                    .or_else(|| base.strip_prefix("https://"))
                    .unwrap_or(base)
                    .split('/')
                    .next()
                    .unwrap_or("");
                let (bh, bp) = base_host
                    .rsplit_once(':')
                    .map_or((base_host, a2a_port), |(h, ps)| {
                        ps.parse::<u16>().map_or((base_host, a2a_port), |n| (h, n))
                    });
                if host == bh && port == bp {
                    return Ok(base.to_string());
                }
            }
        }
        let ip: IpAddr = host
            .parse()
            .map_err(|_| EaiError::protocol(format!("a2a_delegate: bad peer address '{p}'")))?;
        if ip.is_loopback() {
            return Ok(format!("http://{ip}:{port}"));
        }
        // Verified roster members only: the row's `address` is
        // `{ip}:{gmcp_port}` — the A2A surface is the same host on the
        // canonical contract port.
        let member = cluster_key::config_json_rows("peers.json").iter().any(|n| {
            (n.get("node_id").and_then(|v| v.as_str()) == Some(peer)
                || n.get("address")
                    .and_then(|v| v.as_str())
                    .and_then(|a| a.split(':').next())
                    .and_then(|h| h.parse::<IpAddr>().ok())
                    .is_some_and(|pip| pip == ip))
                && n.get("admission").and_then(|v| v.as_str()) == Some("explicit")
        });
        if !member {
            return Err(EaiError::authorization(format!(
                "a2a_delegate: '{p}' is not a verified cluster member or configured a2a external_peer_agent — delegation is allowlist-gated (SSRF guard)"
            )));
        }
        // A node_id lookup still resolves to the member's registered IP.
        let resolved_ip = cluster_key::config_json_rows("peers.json")
            .iter()
            .find_map(|n| {
                if n.get("node_id").and_then(|v| v.as_str()) == Some(peer) {
                    n.get("address")
                        .and_then(|v| v.as_str())
                        .and_then(|a| a.split(':').next())
                        .map(str::to_string)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| ip.to_string());
        Ok(format!("http://{resolved_ip}:{port}"))
    }

    #[tool(
        name = "a2a_delegate",
        description = "Delegate a task to a remote A2A agent endpoint (susi peer or external). Args: {peer: \"self\" | node_id | ip | http://ip[:port], message: string}. Roster-gated: only loopback or verified cluster members resolve. The request carries the host Bearer token plus this node's Ed25519 request signature, so a bound member authorizes us by signature. Returns the completed task's agent reply text."
    )]
    pub fn a2a_delegate(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("a2a_delegate", &arg.to_string(), workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let peer = arg
            .get("peer")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("a2a_delegate requires 'peer'".to_string()))?;
        let message = arg
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("a2a_delegate requires 'message'".to_string()))?;
        let url = Self::a2a_peer_url(peer)?;
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {
                "message": {
                    "role": "ROLE_USER",
                    "parts": [{ "kind": "text", "text": message }],
                    "messageId": format!("susi-{}", crate::susi_config::cluster_key::random_nonce_hex()),
                }
            }
        });
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| EaiError::protocol(format!("a2a_delegate: encode: {e}")))?;
        // Dedicated agent: the shared http_agent's 20s body timeout cuts
        // real fleet missions short. Connect stays tight — delegation to a
        // dead peer should fail fast, but the response may take minutes.
        let agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(10)))
            .timeout_recv_body(Some(std::time::Duration::from_secs(120)))
            .timeout_send_body(Some(std::time::Duration::from_secs(20)))
            .build();
        let agent = ureq::Agent::new_with_config(agent);
        let mut req = agent
            .post(&format!("{url}/"))
            .header("content-type", "application/json");
        let token = crate::susi_config::SusiConfig::load_global()
            .map(|c| c.api_auth_token())
            .unwrap_or_default();
        if !token.is_empty() {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        // Member signature over `susi-peer-req-v2:{node}:{ts}:{nonce}:POST:/:{sha256(body)}`
        // — a bound member's receiver authorizes us without the bearer.
        use crate::susi_config::cluster_key;
        use sha2::Digest;
        let node = cluster_key::wire_node_id();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let nonce = cluster_key::random_nonce_hex();
        let hash = hex::encode(sha2::Sha256::digest(&body_bytes));
        let canonical = format!("susi-peer-req-v2:{node}:{ts}:{nonce}:POST:/:{hash}");
        if let Some(sig) = cluster_key::member_sign(&canonical) {
            req = req
                .header("x-susi-node", node)
                .header("x-susi-req-ts", ts.to_string())
                .header("x-susi-req-nonce", nonce)
                .header("x-susi-req-sig", sig);
        }
        let mut resp = req
            .send(&body_bytes)
            .map_err(|e| EaiError::network(format!("a2a_delegate to {url}: {e}")))?;
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| EaiError::network(format!("a2a_delegate read {url}: {e}")))?;
        let doc: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| EaiError::protocol(format!("a2a_delegate: bad JSON-RPC reply: {e}")))?;
        if let Some(err) = doc.get("error") {
            return Err(EaiError::network(format!("a2a_delegate rpc error: {err}")));
        }
        // Fold the completed task into its agent reply text.
        let reply = doc
            .pointer("/result/task/status/message/parts")
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let state = doc
            .pointer("/result/task/status/state")
            .and_then(|s| s.as_str())
            .map(|s| s.trim_start_matches("TASK_STATE_").to_ascii_lowercase())
            .unwrap_or_else(|| "unknown".to_string());
        if reply.is_empty() {
            Ok(format!("task {state} (no reply text): {text}"))
        } else {
            Ok(format!("task {state}: {reply}"))
        }
    }

    #[tool(
        name = "commit_log_fetch",
        description = "Return local commit-ledger records for anti-entropy pulls. Args: {coordinator?: string, from_seq?: N, limit?: N, offset?: N} — up to `limit` (default 500, max 1000) records with seq >= from_seq from that coordinator (or all coordinators), skipping `offset` matches for pagination."
    )]
    pub fn commit_log_fetch(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let coordinator = arg.get("coordinator").and_then(|v| v.as_str());
        let from_seq = arg.get("from_seq").and_then(|v| v.as_u64()).unwrap_or(1);
        let limit = arg
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(500)
            .min(1000) as usize;
        let offset = arg.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        // Serve the compaction archive alongside the live ledger — a
        // peer repairing a gap that spans the snapshot boundary needs
        // records the live file no longer holds.
        let mut records: Vec<crate::susi_core::commit_log::CommitRecord> =
            crate::susi_core::commit_log::load();
        records.extend(crate::susi_core::commit_log::load_from(
            &crate::susi_core::commit_log::archive_path(),
        ));
        let records: Vec<_> = records
            .into_iter()
            .filter(|r| coordinator.is_none_or(|c| r.coordinator == c) && r.seq >= from_seq)
            .skip(offset)
            .take(limit)
            .collect();
        serde_json::to_string(&records)
            .map_err(|e| EaiError::internal(format!("serialize commit records: {e}")))
    }

    #[tool(
        name = "cluster_rekey_stage",
        description = "Prepare phase of cluster-key rotation: stage a next-epoch cluster key delivered with its committed cluster_rekey record. Args: {record: CommitRecord, key_hex: string (64 hex)}. Verifies the record's signature under the current key, requires coordinator authority, checks the pushed key's SHA-256 matches the committed fingerprint, stages it (0600), then appends the record. Does NOT rotate — a separate cluster_rekey_commit call delivers the activate record whose apply activates the staged key."
    )]
    pub fn cluster_rekey_stage(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // Audit WITHOUT key_hex — the pushed secret must never reach
        // the audit log; the record alone carries its fingerprint.
        gawd_hooks::audit_action(
            "cluster_rekey_stage",
            &arg.get("record")
                .map_or_else(|| arg.to_string(), |r| r.to_string()),
            workspace,
        )
        .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let record: crate::susi_core::commit_log::CommitRecord = serde_json::from_value(
            arg.get("record")
                .cloned()
                .ok_or_else(|| EaiError::protocol("missing 'record' field"))?,
        )
        .map_err(|e| EaiError::protocol(format!("bad commit record: {e}")))?;
        let key_hex = arg
            .get("key_hex")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("missing 'key_hex' field"))?;
        let key_bytes =
            hex::decode(key_hex).map_err(|e| EaiError::protocol(format!("bad key_hex: {e}")))?;
        if key_bytes.len() != 32 {
            return Err(EaiError::protocol("key_hex must decode to 32 bytes"));
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&key_bytes);
        // Epoch check first: a duplicate push post-rotation is a no-op
        // when the record is already held; anything else signed only by
        // the retired key is revoked-key traffic.
        match record.verify_key_epoch() {
            Some(crate::susi_core::commit_log::KeyEpoch::Current) => {}
            Some(crate::susi_core::commit_log::KeyEpoch::Prev) => {
                let held = crate::susi_core::commit_log::load();
                if held.iter().any(|r| r == &record) {
                    return Ok("rekey already applied — nothing to do".to_string());
                }
                return Err(EaiError::authorization(
                    "rekey record verifies only under the retired key — it is not held and cannot be staged post-rotation",
                ));
            }
            None => {
                return Err(EaiError::authorization(
                    "rekey record failed cluster-key signature or consistency checks",
                ));
            }
        }
        if record.kind != crate::susi_core::commit_log::KIND_CLUSTER_REKEY {
            return Err(EaiError::protocol(
                "record is not a cluster_rekey (prepare-phase) record",
            ));
        }
        let Some(fingerprint) = record.rekey_fingerprint() else {
            return Err(EaiError::protocol(
                "record is not a well-formed cluster_rekey record",
            ));
        };
        // Coordinator authority — an evicted member still holds the key
        // and must not be able to rotate the cluster under itself.
        if !crate::susi_core::commit_log::member_coordinator_known(&record) {
            return Err(EaiError::authorization(format!(
                "rekey coordinator {} is not an explicit member of this roster",
                record.coordinator
            )));
        }
        // The pushed key must match the committed fingerprint — the
        // ledger, not the transport, decides which key activates.
        if crate::susi_config::cluster_key::key_fingerprint(&key) != fingerprint {
            return Err(EaiError::protocol(
                "pushed key does not match the committed rekey fingerprint",
            ));
        }
        // Same stale-term gate as commit_record — a coordinator working
        // from superseded leadership must not drive an epoch change.
        if crate::susi_core::commit_log::coordinator_known(&record) {
            if let crate::susi_core::commit_log::TermVerdict::Stale =
                crate::susi_core::commit_log::check_term(&record)
            {
                return Err(EaiError::protocol(format!(
                    "stale term {} (current {}): rekey coordinator is working from superseded leadership",
                    record.term,
                    crate::susi_core::commit_log::load_term().term
                )));
            }
        }
        // Stage, then append — the record commits WHICH key was agreed
        // but does not rotate; activation waits for the commit-phase
        // record. On append failure remove the staged file so a stray
        // next-epoch key cannot linger beside an uncommitted record.
        if !crate::susi_config::cluster_key::stage_key(&key) {
            return Err(EaiError::internal("failed to stage cluster.key.next"));
        }
        if let Err(e) = crate::susi_core::commit_log::append(&record) {
            let _ = std::fs::remove_file(
                crate::susi_paths::SusiDirs::config_dir().join("cluster.key.next"),
            );
            return Err(e);
        }
        // Self-heal: if the commit-phase record already landed (anti-
        // entropy can deliver activate before this stage push), apply
        // fired while nothing was staged and will not re-fire. Activate
        // now so a late stager is not silently stranded on the old key.
        let committed = crate::susi_core::commit_log::load().iter().any(|r| {
            r.kind == crate::susi_core::commit_log::KIND_CLUSTER_REKEY_ACTIVATE
                && r.rekey_fingerprint() == Some(fingerprint)
        });
        if committed && crate::susi_config::cluster_key::activate_staged_key(fingerprint) {
            return Ok(format!(
                "rekey staged and activated — epoch rotated to key {}",
                &fingerprint[..16]
            ));
        }
        Ok(format!(
            "rekey staged — awaiting activate record for epoch {}",
            &fingerprint[..16]
        ))
    }

    #[tool(
        name = "cluster_rekey_commit",
        description = "Commit phase of cluster-key rotation: apply a committed cluster_rekey_activate record. Args: {record: CommitRecord}. Verifies the record's signature (current key — members are still pre-rotation when this arrives), requires coordinator authority, then appends it; the append's apply activates the staged cluster.key.next when its fingerprint matches the committed one. Members that never staged stay on the old epoch."
    )]
    pub fn cluster_rekey_commit(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("cluster_rekey_commit", &arg.to_string(), workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let record: crate::susi_core::commit_log::CommitRecord = serde_json::from_value(
            arg.get("record")
                .cloned()
                .ok_or_else(|| EaiError::protocol("missing 'record' field"))?,
        )
        .map_err(|e| EaiError::protocol(format!("bad commit record: {e}")))?;
        if record.kind != crate::susi_core::commit_log::KIND_CLUSTER_REKEY_ACTIVATE {
            return Err(EaiError::protocol(
                "record is not a cluster_rekey_activate record",
            ));
        }
        // Epoch check first: this record normally verifies under the
        // CURRENT key (we are still pre-rotation when it lands). A
        // Prev-verifying copy is only a no-op when already held —
        // anything else signed by the retired key is revoked traffic.
        match record.verify_key_epoch() {
            Some(crate::susi_core::commit_log::KeyEpoch::Current) => {}
            Some(crate::susi_core::commit_log::KeyEpoch::Prev) => {
                let held = crate::susi_core::commit_log::load();
                if held.iter().any(|r| r == &record) {
                    return Ok("rekey already activated — nothing to do".to_string());
                }
                return Err(EaiError::authorization(
                    "activate record verifies only under the retired key and is not held",
                ));
            }
            None => {
                return Err(EaiError::authorization(
                    "activate record failed cluster-key signature or consistency checks",
                ));
            }
        }
        let Some(fingerprint) = record.rekey_fingerprint() else {
            return Err(EaiError::protocol(
                "activate record carries a malformed fingerprint",
            ));
        };
        // Coordinator authority — an evicted member still holds the key
        // and must not drive an epoch change.
        if !crate::susi_core::commit_log::member_coordinator_known(&record) {
            return Err(EaiError::authorization(format!(
                "rekey coordinator {} is not an explicit member of this roster",
                record.coordinator
            )));
        }
        // Same stale-term gate as commit_record.
        if crate::susi_core::commit_log::coordinator_known(&record) {
            if let crate::susi_core::commit_log::TermVerdict::Stale =
                crate::susi_core::commit_log::check_term(&record)
            {
                return Err(EaiError::protocol(format!(
                    "stale term {} (current {}): rekey coordinator is working from superseded leadership",
                    record.term,
                    crate::susi_core::commit_log::load_term().term
                )));
            }
        }
        crate::susi_core::commit_log::append(&record)?;
        Ok(format!(
            "cluster epoch rotated to key {}",
            &fingerprint[..16]
        ))
    }

    #[tool(
        name = "member_propose",
        description = "Leader-only membership proposal (Raft's leader-proposed configuration-entry rule): a member asks the elected leader to seal and replicate a roster delta. Args: {member: \"node_id@address\", kind?: \"member_add\"|\"member_remove\"|\"member_unban\" (default member_add)}. Refuses unless this node is the currently claimed leader. member_add refuses banned subjects or the leader itself; member_remove requires the subject in the leader's roster; member_unban requires the subject banned. On success the record is sealed, appended, and pushed to the roster including the subject."
    )]
    pub fn member_propose(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("member_propose", &arg.to_string(), workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let member = arg
            .get("member")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("missing 'member' field"))?;
        let kind = arg
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or(crate::susi_core::commit_log::KIND_MEMBER_ADD);
        // Optional on member_add: the subject's Ed25519 pubkey as the
        // proposer's handshake attested it — the committed record then
        // binds the member's signing key on every receiver. `subject_sig`
        // is the subject's own signature over
        // `susi-bind-v1:{id}:{pubkey}` from its v3 pong — intake refuses
        // a pubkey binding without it, so a proposer can never bind a
        // key the subject didn't claim.
        let member_pubkey = arg
            .get("member_pubkey")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let subject_sig = arg
            .get("subject_sig")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if !matches!(
            kind,
            crate::susi_core::commit_log::KIND_MEMBER_ADD
                | crate::susi_core::commit_log::KIND_MEMBER_REMOVE
                | crate::susi_core::commit_log::KIND_MEMBER_UNBAN
        ) {
            return Err(EaiError::protocol(format!(
                "unknown member kind '{kind}' — expected member_add, member_remove, or member_unban"
            )));
        }
        let (id, addr) = member
            .split_once('@')
            .filter(|(i, a)| !i.is_empty() && !a.is_empty())
            .ok_or_else(|| EaiError::protocol("member must be 'node_id@address'"))?;
        // Only the claimed leader may seal configuration changes — the
        // member_coordinator_known gate on intake refuses anything else.
        let term = crate::susi_core::commit_log::load_term();
        let self_id = crate::susi_config::cluster_key::wire_node_id();
        if term.leader != self_id {
            return Err(EaiError::authorization(format!(
                "not the elected leader (leader: {}) — propose to the leader",
                if term.leader.is_empty() {
                    "none yet"
                } else {
                    &term.leader
                }
            )));
        }
        if kind == crate::susi_core::commit_log::KIND_MEMBER_ADD && id == self_id {
            return Err(EaiError::protocol(
                "leader cannot be proposed as a new member — self-edge",
            ));
        }
        let banned: Vec<serde_json::Value> = std::fs::read_to_string(
            crate::susi_paths::SusiDirs::config_dir().join("peers_banned.json"),
        )
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
        let is_banned = banned.iter().any(|b| {
            b.get("node_id").and_then(|v| v.as_str()) == Some(id)
                || b.get("address").and_then(|v| v.as_str()) == Some(addr)
        });
        // Electorate = this leader's explicit roster at seal time.
        let roster = gawd::cluster_roster().map_err(|e| {
            crate::susi_error::rewrap("internal", format!("cluster roster unavailable: {e}"))
        })?;
        // A delegated add must not resurrect a banned member — the ban
        // list is local state the proposer may not share.
        if kind == crate::susi_core::commit_log::KIND_MEMBER_ADD && is_banned {
            return Err(EaiError::authorization(format!(
                "{member} is banned — a delegated add cannot lift a committed eviction"
            )));
        }
        // Refuse phantom evictions — a remove for a subject the leader
        // doesn't roster is a wasted committed record.
        if kind == crate::susi_core::commit_log::KIND_MEMBER_REMOVE
            && !roster.iter().any(|(nid, _, _)| nid.as_str() == id)
        {
            return Err(EaiError::protocol(format!(
                "{id} is not in the leader's roster — nothing to remove"
            )));
        }
        if kind == crate::susi_core::commit_log::KIND_MEMBER_UNBAN && !is_banned {
            return Err(EaiError::protocol(format!(
                "{member} is not banned — nothing to unban"
            )));
        }
        // The leader votes in its own electorate — otherwise evicting a
        // dead member would require the dead member's endorsement.
        let mut electorate: Vec<String> = roster.iter().map(|(nid, _, _)| nid.clone()).collect();
        electorate.push(self_id.clone());
        let Some(mut record) = crate::susi_core::commit_log::CommitRecord::seal_member(
            &self_id,
            &self_id,
            kind,
            member,
            electorate,
            member_pubkey,
        ) else {
            return Err(EaiError::internal(
                "no cluster.key — cannot seal a member record",
            ));
        };
        record.subject_sig = subject_sig.to_string();
        // Joint-consensus: collect the bound electorate's endorsements
        // before committing — a bound-quorum requirement is evaluated
        // receiver-side, so a record sealed short of it would be refused
        // everywhere it lands. The leader's member_sig is its own vote;
        // ask the other members.
        let targets: Vec<(String, String)> = roster
            .iter()
            .filter(|(nid, a, _)| nid != &self_id && !a.starts_with("127."))
            .map(|(nid, a, _)| (nid.clone(), a.clone()))
            .collect();
        record.endorsements = crate::susi_core::commit_log::collect_endorsements(&record, &targets);
        if !crate::susi_core::commit_log::endorsements_satisfied(&record) {
            let signed: std::collections::HashSet<&str> = record
                .endorsements
                .iter()
                .map(|e| e.node.as_str())
                .collect();
            let missing: Vec<&str> = targets
                .iter()
                .map(|(nid, _)| nid.as_str())
                .filter(|nid| !signed.contains(nid))
                .collect();
            return Err(EaiError::authorization(format!(
                "insufficient endorsements for {kind} — a majority of the bound electorate must sign (got {} supporter(s)); non-signers: {}; dead members can only be dropped once the quorum question is resolvable",
                record.endorsements.len() + 1,
                if missing.is_empty() {
                    "none (signatures collected but quorum still short)".to_string()
                } else {
                    missing.join(", ")
                }
            )));
        }
        crate::susi_core::commit_log::append(&record)?;
        // Push to the roster plus the subject — the subject is not yet
        // in the leader's roster, and it must learn its own committed
        // membership promptly rather than waiting for anti-entropy.
        let bearer = crate::susi_config::cluster_key::peer_bearer();
        let args = serde_json::to_value(&record)
            .map_err(|e| EaiError::internal(format!("serialize record: {e}")))?;
        let mut pushed = 0usize;
        for target in roster
            .iter()
            .map(|(_, a, _)| a.clone())
            .chain(std::iter::once(addr.to_string()))
        {
            if target.starts_with("127.")
                || target.starts_with("::1")
                || target.starts_with("localhost")
            {
                continue;
            }
            if let Ok(result) = crate::susi_core::mcp_client::call_tool(
                &target,
                "commit_record",
                &args,
                bearer.as_deref(),
            ) {
                if result.get("isError").and_then(|v| v.as_bool()) != Some(true) {
                    pushed += 1;
                }
            }
        }
        Ok(format!(
            "{kind} {member} committed (seq {}, term {}) — pushed to {pushed} peer(s)",
            record.seq, record.term
        ))
    }

    #[tool(
        name = "member_endorse",
        description = "Endorse a sealed privileged record (member/rekey kind) — the joint-consensus vote a leader collects before committing a roster/config delta. Args: {record: <CommitRecord json>}. Verifies the record's HMAC signature under this member's cluster.key and refuses non-privileged kinds, then returns {node, sig} where sig is this node's Ed25519 signature over susi-endorse-v1:{record.signature}. An endorsement is only a vote on that exact record — all intake gates still apply when it lands."
    )]
    pub fn member_endorse(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        gawd_hooks::audit_action("member_endorse", &arg.to_string(), workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let rec_val = arg
            .get("record")
            .ok_or_else(|| EaiError::protocol("missing 'record' field"))?;
        let record: crate::susi_core::commit_log::CommitRecord =
            serde_json::from_value(rec_val.clone())
                .map_err(|e| EaiError::protocol(format!("record does not parse: {e}")))?;
        if !crate::susi_core::commit_log::privileged_kind_name(&record.kind) {
            return Err(EaiError::protocol(
                "only privileged (member/rekey) records need endorsements",
            ));
        }
        if !record.verify() {
            return Err(EaiError::protocol(
                "refusing to endorse a record that fails signature or consistency verification",
            ));
        }
        let node = crate::susi_config::cluster_key::wire_node_id();
        let payload = crate::susi_core::commit_log::endorsement_payload(&record.signature);
        let sig = crate::susi_config::cluster_key::member_sign(&payload)
            .ok_or_else(|| EaiError::internal("no node.key — cannot sign an endorsement"))?;
        Ok(serde_json::json!({"node": node, "sig": sig}).to_string())
    }

    #[tool(
        name = "cluster_status",
        description = "Read-only consensus snapshot for cluster-wide status checks: {node, term, leader, records, heads, key_epoch, roster, banned, bound, pubkey}. `heads` maps each coordinator's node_id to its highest ledger seq + epoch prefix — comparing heads across members exposes ledger divergence directly. No args."
    )]
    pub fn cluster_status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        use crate::susi_core::commit_log;
        let term = commit_log::load_term();
        let records = commit_log::load();
        let mut heads: std::collections::BTreeMap<String, serde_json::Value> =
            std::collections::BTreeMap::new();
        for r in &records {
            let entry = heads
                .entry(r.coordinator.clone())
                .or_insert_with(|| serde_json::json!({"seq": 0u64, "epoch": ""}));
            if r.seq > entry.get("seq").and_then(|v| v.as_u64()).unwrap_or(0) {
                entry["seq"] = serde_json::json!(r.seq);
                entry["epoch"] = serde_json::json!(r.epoch.get(..12).unwrap_or(&r.epoch));
            }
        }
        let roster = gawd::cluster_roster().unwrap_or_default();
        // Mtime-cached roster rows — this tool answers on every
        // `peers status` sweep.
        let banned = crate::susi_config::cluster_key::config_json_rows("peers_banned.json").len();
        let bound = crate::susi_config::cluster_key::config_json_rows("peers.json")
            .iter()
            .filter(|p| {
                !p.get("pubkey")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .is_empty()
            })
            .count();
        let key_epoch = crate::susi_config::cluster_key::cluster_key()
            .map(|k| crate::susi_config::cluster_key::key_fingerprint(&k))
            .unwrap_or_default();
        Ok(serde_json::json!({
            "node": crate::susi_config::cluster_key::wire_node_id(),
            "term": term.term,
            "leader": term.leader,
            "records": records.len(),
            "heads": heads,
            "key_epoch": key_epoch.get(..12).unwrap_or(&key_epoch),
            "roster": roster.len(),
            "banned": banned,
            "bound": bound,
            "pubkey": crate::susi_config::cluster_key::node_pubkey_hex()
                .map(|p| p.get(..12).unwrap_or(&p).to_string())
                .unwrap_or_default(),
        })
        .to_string())
    }
}

/// Anti-entropy repair for the commit ledger: resolve `coordinator`'s
/// address through the gawd cluster-roster topic, pull the missing seqs
/// via `commit_log_fetch`, verify each record's signature before
/// appending — a compromised peer can never inject unverifiable history,
/// it can only fail to answer. The coordinator is tried first, then
/// every other verified peer: records replicate to all voters, so any
/// member that held a copy can serve the gap. Returns the count of
/// records repaired.
fn repair_commit_gap(coordinator: &str, missing: &[u64]) -> Result<usize, String> {
    // Trust-sorted roster: coordinator first, then the most-trusted
    // fallbacks — every voter holds a replica of what it voted on.
    let peers = gawd::cluster_roster().map_err(|e| format!("cluster roster unavailable: {e}"))?;
    if peers.is_empty() {
        return Err("verified roster is empty".to_string());
    }
    let mut candidates: Vec<&str> = peers
        .iter()
        .filter(|(id, _, _)| id == coordinator)
        .map(|(_, a, _)| a.as_str())
        .collect();
    candidates.extend(
        peers
            .iter()
            .filter(|(id, _, _)| id != coordinator)
            .map(|(_, a, _)| a.as_str()),
    );
    if candidates.is_empty() {
        return Err(format!("coordinator {coordinator} not in verified roster"));
    }

    // Roster candidates are verified members — the member-to-member
    // credential is the cluster-derived peer bearer (the per-host
    // api_token cannot authenticate on a remote node).
    let bearer_owned = crate::susi_config::cluster_key::peer_bearer();
    let bearer = bearer_owned.as_deref();

    let mut last_err = String::new();
    let mut repaired_total = 0usize;
    let mut remaining: Vec<u64> = missing.to_vec();
    for addr in candidates {
        if remaining.is_empty() {
            break;
        }
        match fetch_commit_records(addr, coordinator, remaining[0], bearer) {
            Ok(mut fetched) => {
                // Descending seq order: a prior-epoch record (signed
                // under the retired cluster key) may only append when
                // its seq+1 successor is held and names its epoch — so
                // the successor must land first, or a pre-rotation tail
                // gap takes one sweep per record.
                fetched.sort_by_key(|r| std::cmp::Reverse(r.seq));
                // Member records need coordinator authority — an
                // evicted node still holds cluster.key, so a gap fill
                // must not smuggle a rogue roster delta through.
                let eligible: Vec<crate::susi_core::commit_log::CommitRecord> = fetched
                    .into_iter()
                    .filter(|r| {
                        r.coordinator == coordinator
                            && remaining.contains(&r.seq)
                            && r.verify()
                            && crate::susi_core::commit_log::member_coordinator_known(r)
                    })
                    .collect();
                // Batch intake: one lock + one ledger load for the whole
                // repair instead of a full-ledger parse per record.
                let outcomes = crate::susi_core::commit_log::append_many(&eligible);
                let mut got = 0usize;
                for (r, outcome) in eligible.iter().zip(outcomes) {
                    // Applied OR already-held (Skipped) — the seq is
                    // present either way, so the gap is filled.
                    if outcome != crate::susi_core::commit_log::AppendOutcome::Refused {
                        remaining.retain(|s| *s != r.seq);
                        got += 1;
                    }
                }
                repaired_total += got;
                if got == 0 {
                    last_err = format!("{addr} held none of the missing records");
                }
            }
            Err(e) => last_err = e,
        }
    }
    if repaired_total > 0 {
        return Ok(repaired_total);
    }
    Err(last_err)
}

/// Pull a coordinator's records (seq >= `from_seq`) from one peer's
/// `commit_log_fetch` tool over the MCP peer channel (`mcp_client` runs
/// the session handshake — a bare POST to `/` is rejected by the server).
fn fetch_commit_records(
    addr: &str,
    coordinator: &str,
    from_seq: u64,
    bearer: Option<&str>,
) -> Result<Vec<crate::susi_core::commit_log::CommitRecord>, String> {
    let result = crate::susi_core::mcp_client::call_tool(
        addr,
        "commit_log_fetch",
        &serde_json::json!({ "coordinator": coordinator, "from_seq": from_seq }),
        bearer,
    )
    .map_err(|e| format!("fetch from {addr}: {e}"))?;
    let text = result
        .pointer("/content/0/text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "no tool output in fetch response".to_string())?;
    serde_json::from_str(text).map_err(|e| format!("bad records: {e}"))
}

/// True when a remote tool's `Ok` payload is actually stringified error
/// text — MCP adapters can return `-32603`/protocol failures as content.
/// Display prefixes are checked only near the head so output that merely
/// mentions an error is not misclassified.
fn remote_result_is_error(text: &str) -> bool {
    let t = text.trim_start();
    if t.contains("[FAIL]") {
        return true;
    }
    let head: String = t.chars().take(64).collect();
    head.contains(" Error:")
        || head.contains(" Violation:")
        || head.contains("Mcp error")
        || head.contains("MCP Error")
}

impl CoreTools {
    #[tool(
        name = "commit_log",
        description = "List quorum commit records in the local ledger, newest first. Args: {limit?: N}"
    )]
    pub fn commit_log(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let limit = arg
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(500) as usize;
        let records = crate::susi_core::commit_log::load();
        let mut lines = vec!["EPOCH\tCOORDINATOR\tTALLY\tQUORUM\tAT".to_string()];
        for r in records.iter().rev().take(limit) {
            lines.push(format!(
                "{}\t{}\t{}\t{}\t{}",
                r.epoch.get(..12).unwrap_or(&r.epoch),
                r.coordinator,
                r.tally,
                r.quorum_threshold,
                r.committed_at
            ));
        }
        Ok(lines.join("\n"))
    }

    #[tool(
        name = "agents_list",
        description = "List managed external executors and local setup readiness"
    )]
    pub fn agents_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let agents = agents::external_list(workspace, "execution");
        serde_json::to_string(&agents).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "agents_run",
        description = "Launch an external agent task; returns a durable task ID for status, logs and cancellation"
    )]
    pub fn agents_run(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let agent = arg
            .get("agent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("agent is required"))?;
        let prompt = arg
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("prompt is required"))?;
        let audit = format!("{agent}: {prompt}");
        gawd_hooks::audit_action("agents_run", &audit, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        let run = agents::external_run(workspace, "execution", agent, prompt)
            .map_err(EaiError::process)?;
        serde_json::to_string(&run).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "agents_tasks",
        description = "List persisted external agent tasks for this workspace"
    )]
    pub fn agents_tasks(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let runs = agents::external_runs(workspace, "execution");
        serde_json::to_string(&runs).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "agents_status",
        description = "Inspect an external task; refresh=true recovers cloud status after worker loss"
    )]
    pub fn agents_status(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        external_agent_control(arg, workspace, "status")
    }

    #[tool(
        name = "agents_cancel",
        description = "Request cancellation of a managed external agent task"
    )]
    pub fn agents_cancel(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        external_agent_control(arg, workspace, "cancel")
    }

    #[tool(
        name = "agents_logs",
        description = "Read the last 64 KiB of external agent output; stderr=true selects errors"
    )]
    pub fn agents_logs(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        external_agent_control(arg, workspace, "logs")
    }

    #[tool(
        name = "agents_send",
        description = "Send a follow-up message to an existing cloud agent task"
    )]
    pub fn agents_send(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        external_agent_control(arg, workspace, "send")
    }

    #[tool(
        name = "coding_models_list",
        description = "List top developer/agent models and local setup readiness"
    )]
    pub fn coding_models_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        crate::susi_core::plane_bus::gemi::apply_cloud_env_file();
        let rows = crate::susi_core::plane_bus::gemi::coding_catalog();
        serde_json::to_string(&rows).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "coding_models_prefer",
        description = "Prefer a coding/agent model id for subsequent routing"
    )]
    pub fn coding_models_prefer(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let model = arg
            .get("model")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("model is required"))?;
        crate::susi_core::plane_bus::gemi::apply_cloud_env_file();
        Ok(format!("{{\"preferred\":\"{model}\"}}"))
    }

    #[tool(
        name = "frameworks_list",
        description = "List managed agent frameworks/engines and local setup readiness"
    )]
    pub fn frameworks_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let engines = agents::external_list(workspace, "framework");
        serde_json::to_string(&engines).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "frameworks_run",
        description = "Launch an agent-framework task; returns a durable task ID"
    )]
    pub fn frameworks_run(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let engine = arg
            .get("engine")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("engine is required"))?;
        let prompt = arg
            .get("prompt")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("prompt is required"))?;
        let run = agents::external_run(workspace, "framework", engine, prompt)
            .map_err(EaiError::process)?;
        serde_json::to_string(&run).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "frameworks_tasks",
        description = "List persisted agent-framework tasks for this workspace"
    )]
    pub fn frameworks_tasks(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let runs = agents::external_runs(workspace, "framework");
        serde_json::to_string(&runs).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "frameworks_status",
        description = "Inspect an agent-framework task"
    )]
    pub fn frameworks_status(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        external_agent_control(arg, workspace, "status")
    }

    #[tool(
        name = "frameworks_cancel",
        description = "Request cancellation of a managed agent-framework task"
    )]
    pub fn frameworks_cancel(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        external_agent_control(arg, workspace, "cancel")
    }

    #[tool(
        name = "frameworks_logs",
        description = "Read the last 64 KiB of agent-framework output; stderr=true selects errors"
    )]
    pub fn frameworks_logs(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        external_agent_control(arg, workspace, "logs")
    }

    #[tool(
        name = "tasks_list",
        description = "List active and historical swarm tasks with liveness telemetry"
    )]
    pub fn tasks_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let tasks = crate::susi_core::task_manager::SwarmTaskManager::global().list_tasks();
        serde_json::to_string_pretty(&tasks).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(name = "tasks_pause", description = "Pause a running task by task_id")]
    pub fn tasks_pause(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = arg
            .get("task_id")
            .and_then(|v| v.as_str())
            .or_else(|| arg.as_str())
            .unwrap_or("")
            .trim();
        if crate::susi_core::task_manager::SwarmTaskManager::global().pause_task(id) {
            Ok(format!("Task '{}' paused.", id))
        } else {
            Err(EaiError::protocol(format!("Task '{}' not found.", id)))
        }
    }

    #[tool(name = "tasks_resume", description = "Resume a paused task by task_id")]
    pub fn tasks_resume(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = arg
            .get("task_id")
            .and_then(|v| v.as_str())
            .or_else(|| arg.as_str())
            .unwrap_or("")
            .trim();
        if crate::susi_core::task_manager::SwarmTaskManager::global().resume_task(id) {
            Ok(format!("Task '{}' resumed.", id))
        } else {
            Err(EaiError::protocol(format!("Task '{}' not found.", id)))
        }
    }

    #[tool(
        name = "tasks_kill",
        description = "Kill a running or stalled task by task_id"
    )]
    pub fn tasks_kill(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = arg
            .get("task_id")
            .and_then(|v| v.as_str())
            .or_else(|| arg.as_str())
            .unwrap_or("")
            .trim();
        if crate::susi_core::task_manager::SwarmTaskManager::global().kill_task(id) {
            Ok(format!("Task '{}' killed.", id))
        } else {
            Err(EaiError::protocol(format!("Task '{}' not found.", id)))
        }
    }

    #[tool(
        name = "mcp_registry",
        description = "Interrogate global MCP registry and benchmark servers"
    )]
    pub fn mcp_registry(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let entries = GmcpClient::autonomous_web_scout();
        Ok(format!(
            "Global MCP Substrate Roster (Count: {})\n\n{}",
            entries.len(),
            entries.join("\n")
        ))
    }

    #[tool(name = "mcp_configure", description = "Configure external MCP server")]
    pub fn mcp_configure(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let name = arg
            .get("name")
            .and_then(|v| v.as_str())
            .or_else(|| arg.as_str().and_then(|s| s.split_whitespace().next()));
        let package = arg
            .get("package")
            .and_then(|v| v.as_str())
            .or_else(|| arg.as_str().and_then(|s| s.split_whitespace().nth(1)));

        if let Some(n) = name {
            let p = package.unwrap_or(n);
            let res = GmcpClient::auto_configure_server(n, p)?;
            Ok(format!("MCP Server '{n}' configuration status: {res}"))
        } else {
            Err(EaiError::protocol(
                "Usage: mcp_configure {name: <name>, package: <package>}",
            ))
        }
    }

    #[tool(
        name = "leading_mcp_list",
        description = "List top MCP tool servers and enablement/readiness"
    )]
    pub fn leading_mcp_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let rows = plane_tools::leading_mcp_list(workspace);
        serde_json::to_string(&rows).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "leading_mcp_enable",
        description = "Enable a leading MCP server into ~/.susi/mcp_config.json"
    )]
    pub fn leading_mcp_enable(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let server = arg
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("server is required"))?;
        let cfg = plane_tools::leading_mcp_get(workspace, server);
        serde_json::to_string(&cfg).map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "leading_mcp_disable",
        description = "Disable a leading MCP server from ~/.susi/mcp_config.json"
    )]
    pub fn leading_mcp_disable(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let server = arg
            .get("server")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("server is required"))?;
        let removed = plane_tools::leading_mcp_remove(workspace, server)
            .get("removed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        Ok(format!("{{\"server\":\"{server}\",\"removed\":{removed}}}"))
    }

    #[tool(
        name = "agent_register",
        description = "Dynamically register a new agent profile"
    )]
    pub fn agent_register(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let name = arg.get("name").and_then(|v| v.as_str());
        let desc = arg.get("description").and_then(|v| v.as_str());
        let cats = arg.get("categories").and_then(|v| v.as_str());

        if let (Some(n), Some(d), Some(c)) = (name, desc, cats) {
            let profile = crate::susi_core::AgentProfile {
                name: n.to_string(),
                description: d.to_string(),
                categories: c.split(',').map(|s| s.trim().to_string()).collect(),
                semantic_anchors: Vec::new(),
                base_rank: 0.8,
                is_core: false,
            };
            crate::susi_core::AgentMetaRegistry::global().register_agent(profile);
            Ok(format!("Successfully registered agent: {}", n))
        } else {
            let arg_s = arg.as_str().unwrap_or("");
            let parts: Vec<&str> = arg_s.splitn(3, ' ').collect();
            if parts.len() < 3 {
                return Err(EaiError::protocol(
                    "Usage: agent_register {name, description, categories}",
                ));
            }

            let profile = crate::susi_core::AgentProfile {
                name: parts[0].to_string(),
                description: parts[1].to_string(),
                categories: parts[2].split(',').map(|s| s.trim().to_string()).collect(),
                semantic_anchors: Vec::new(),
                base_rank: 0.8,
                is_core: false,
            };

            crate::susi_core::AgentMetaRegistry::global().register_agent(profile);
            Ok(format!("Successfully registered agent: {}", parts[0]))
        }
    }

    #[tool(name = "reason", description = "Execute swarm reasoning substrate")]
    pub fn reason(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let arg_s = if let Some(s) = arg.as_str() {
            s.to_string()
        } else {
            arg.to_string()
        };
        // Untrusted Input Boundary (Mandate 41): `reason` is a second front
        // door into the same reasoning substrate `susi_solve` guards with
        // `SusiMasterAgent::sanitize_input` (reached via `EngineHooks`), and
        // it's also the exact verb LAN peers use to dispatch mission intent
        // (`SusiSupervisor::dispatch_peer_task`, susi-gawd-swarm/amas.rs).
        // Apply the same length/injection-pattern check and the same
        // governance detectors every other action-capable tool call gets
        // (see `exec_command` above) before the raw prompt ever reaches
        // the model.
        let sanitized = gawd_hooks::sanitize_input(&arg_s)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        gawd_hooks::audit_action("reason", &sanitized, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        // When the mission captured real tool calls, this reasoning call must
        // answer by citing those receipts — free narrative cannot certify.
        let prompt = format!(
            "{sanitized}{}",
            crate::susi_core::capture::EvidenceSession::evidence_prompt_for(workspace)
        );
        Ok(
            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
                &prompt, workspace,
            ),
        )
    }

    /// Full swarm solve via SusiMasterAgent. Accepts a plain string, or an
    /// object with `intent` / `input` / `prompt` — never the raw JSON Display
    /// of the whole arguments blob prefixed with the tool name.
    #[tool(
        name = "susi_solve",
        description = "Solve a natural-language intent via the SUSI swarm substrate"
    )]
    pub fn susi_solve(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let intent = if let Some(s) = arg.as_str() {
            s.to_string()
        } else if let Some(s) = arg
            .get("intent")
            .or_else(|| arg.get("input"))
            .or_else(|| arg.get("prompt"))
            .and_then(|v| v.as_str())
        {
            s.to_string()
        } else {
            arg.to_string()
        };
        if intent.trim().is_empty() {
            return Err(EaiError::protocol(
                "Usage: susi_solve with string intent or {intent|input|prompt}",
            ));
        }
        Ok(gawd::solve_mission(
            &intent,
            workspace,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    #[tool(
        name = "power_reason",
        description = "Delegate complex reasoning to Power-Tier MCP remotes"
    )]
    pub fn power_reason(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let arg_s = if let Some(s) = arg.as_str() {
            s.to_string()
        } else {
            arg.to_string()
        };
        if arg_s.trim().is_empty() {
            return Err(EaiError::protocol("Usage: power_reason <complex_intent>"));
        }

        let remotes = GmcpClient::scout_reasoning_remotes();
        if let Some(best_remote) = remotes.first() {
            let res = GmcpClient::execute_external_tool(best_remote, "reason", &arg_s)?;
            if !remote_result_is_error(&res) {
                return Ok(res);
            }
        }

        Err(EaiError::protocol(
            "No Power-Tier reasoning remotes configured or available. SUSI local reasoning active.",
        ))
    }

    #[tool(
        name = "meta_scout_agents",
        description = "Discover agent capabilities from connected remotes"
    )]
    pub fn meta_scout_agents(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let remotes = GmcpClient::list_external_tools();
        let mut report = "Discovered Meta-Agent Capabilities:\n\n".to_string();
        for r in remotes {
            if r.name.contains("agent") || r.name.contains("swarm") {
                report.push_str(&format!("- [REMOTE] {}: {}\n", r.name, r.description));
            }
        }
        Ok(report)
    }

    #[tool(
        name = "meta_rank_agents",
        description = "Report current agent expertise hierarchy"
    )]
    pub fn meta_rank_agents(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let registry = crate::susi_core::AgentMetaRegistry::global();
        let agents = registry.list_agents();
        let mut report = "SUSI Expertise Hierarchy:\n\n".to_string();
        for a in agents {
            report.push_str(&format!(
                "- [AGENT] {} (Base Rank: {:.2}): {}\n",
                a.name, a.base_rank, a.description
            ));
        }
        Ok(report)
    }

    #[cfg(feature = "tools-rich")]
    #[tool(
        name = "ast_analyze",
        description = "Structural AST code analysis via tree-sitter"
    )]
    pub fn ast_analyze(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let path_s = arg.get("path").and_then(|v| v.as_str());
        let code_s = arg.get("code").and_then(|v| v.as_str());

        let code = if let Some(c) = code_s {
            c.to_string()
        } else if let Some(p) = path_s {
            let path = secure_path(workspace, p)?;
            read_file_nofollow(&path)?
        } else {
            return Err(EaiError::protocol(
                "Usage: ast_analyze {path: <path>} OR {code: <code>}",
            ));
        };

        let ext = path_s
            .and_then(|p| Path::new(p).extension())
            .and_then(|e| e.to_str())
            .unwrap_or("rs");
        let mut report = format!("AST Analysis ({}): {} bytes\n", ext, code.len());

        // Attempt structural analysis
        if ext == "rs" {
            if let Ok(file) = syn::parse_file(&code) {
                report.push_str(&format!(
                    "Converged AST (syn): {} top-level items.\n",
                    file.items.len()
                ));
                for item in file.items.iter().take(5) {
                    // Only the headline item kinds are reported; every other
                    // syn::Item variant is intentionally skipped.
                    #[allow(clippy::wildcard_enum_match_arm)]
                    match item {
                        syn::Item::Fn(f) => {
                            report.push_str(&format!("  - Function: {}\n", f.sig.ident))
                        }
                        syn::Item::Struct(s) => {
                            report.push_str(&format!("  - Struct: {}\n", s.ident))
                        }
                        syn::Item::Enum(e) => report.push_str(&format!("  - Enum: {}\n", e.ident)),
                        _ => {}
                    }
                }
            }
        } else {
            report.push_str("Multi-language tree-sitter parsing active. [ROOT_NODE] identified.\n");
        }

        Ok(report)
    }

    #[cfg(feature = "tools-rich")]
    #[tool(
        name = "semantic_search",
        description = "Unified BM25 recall over .susi memory, experience, audit log, and workspace files"
    )]
    pub fn semantic_search(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let query_str = arg
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing query"))?;

        let hits = super::semantic_index::SemanticIndex::search(workspace, query_str, 5)?;

        let mut out = format!("Semantic recall results for '{}':\n", query_str);
        if hits.is_empty() {
            out.push_str("No matches found in the unified index.\n");
        }
        for hit in hits {
            out.push_str(&format!(
                "- [{:.3}] ({}:{}) {}\n",
                hit.score, hit.source, hit.doc_id, hit.snippet
            ));
        }
        Ok(out)
    }

    #[tool(
        name = "sandbox_exec",
        description = "Isolated Docker execution via bollard"
    )]
    pub fn sandbox_exec(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let cmd = arg
            .get("cmd")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing cmd"))?;

        gawd_hooks::audit_action("sandbox_exec", cmd, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;

        Self::shared_runtime()?
            .block_on(async {
                crate::susi_sandbox::manager::SandboxManager::execute_in_docker(cmd).await
            })
            .map_err(|e| {
                EaiError::process(format!(
                    "[CAPABILITY_GAP] Docker execution failed: {e}. \
                 Start the susi-sandbox service (127.0.0.1:18083) and ensure Docker is running."
                ))
            })
    }

    #[cfg(feature = "tools-rich")]
    #[tool(
        name = "browser_automate",
        description = "DOM access and web automation via headless_chrome"
    )]
    pub fn browser_automate(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let url = arg
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing url"))?;
        let validated_url = secure_external_url(url)?;
        let url = validated_url.as_str();

        let browser = Browser::default().map_err(|e| {
            EaiError::process(format!(
                "[CAPABILITY_GAP] Headless Chrome failed: {}. Ensure Chrome/Chromium is installed.",
                e
            ))
        })?;
        let tab = browser
            .new_tab()
            .map_err(|e| EaiError::process(e.to_string()))?;

        tab.navigate_to(url)
            .map_err(|e| EaiError::process(e.to_string()))?;
        tab.wait_until_navigated()
            .map_err(|e| EaiError::process(e.to_string()))?;

        let screenshot_dir = workspace.join(".susi/screenshots");
        let _ = fs::create_dir_all(&screenshot_dir);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let screenshot_path = screenshot_dir.join(format!("screenshot_{}.png", ts));

        let png_data = tab
            .capture_screenshot(
                headless_chrome::protocol::cdp::Page::CaptureScreenshotFormatOption::Png,
                None,
                None,
                true,
            )
            .map_err(|e| EaiError::process(e.to_string()))?;
        fs::write(&screenshot_path, png_data).map_err(|e| EaiError::filesystem(e.to_string()))?;

        let content = tab
            .get_content()
            .map_err(|e| EaiError::process(e.to_string()))?;

        Ok(format!(
            "Browser automation success for {}. Screenshot: {}. Content length: {} bytes.",
            url,
            screenshot_path.display(),
            content.len()
        ))
    }

    #[cfg(feature = "tools-rich")]
    #[tool(
        name = "rag_query",
        description = "Vector recall over the unified .susi index via local fastembed cosine similarity"
    )]
    pub fn rag_query(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let query = arg
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::protocol("Missing query"))?;

        let hits = super::semantic_index::SemanticIndex::vector_recall(workspace, query, 3)?;

        let mut out = "Top 3 Semantic Matches:\n".to_string();
        if hits.is_empty() {
            out.push_str("No embedded documents in the unified index yet.\n");
        }
        for hit in hits {
            out.push_str(&format!(
                "- [Score: {:.3}] ({}:{}) {}\n",
                hit.score, hit.source, hit.doc_id, hit.snippet
            ));
        }
        Ok(out)
    }

    #[tool(
        name = "context_graph_query",
        description = "Query the Universal Context Graph for the current workspace or a specific node id"
    )]
    pub fn context_graph_query(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let graph = crate::susi_core::context_graph::ContextGraph::global();
        let _ = graph.replay();
        if let Some(node_id) = arg.get("node_id").and_then(|v| v.as_str()) {
            let depth = arg.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
            let node = crate::susi_core::context_graph::NodeId(node_id.to_string());
            let subgraph = graph.related(&node, depth);
            Ok(serde_json::to_string_pretty(&subgraph).unwrap_or_else(|_| "{}".to_string()))
        } else {
            let subgraph = graph.workspace_subgraph(workspace);
            let events: Vec<_> = subgraph
                .nodes
                .into_iter()
                .map(|n| {
                    serde_json::json!({
                        "type": "node",
                        "id": n.id.0,
                        "kind": format!("{:?}", n.kind),
                        "label": n.label,
                        "created_at": n.created_at,
                    })
                })
                .chain(subgraph.edges.into_iter().map(|e| {
                    serde_json::json!({
                        "type": "edge",
                        "id": e.id,
                        "source": e.source.0,
                        "target": e.target.0,
                        "kind": format!("{:?}", e.kind),
                        "created_at": e.created_at,
                    })
                }))
                .collect();
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "workspace": workspace.display().to_string(),
                "event_count": events.len(),
                "events": events,
            }))
            .unwrap_or_else(|_| "{}".to_string()))
        }
    }

    #[tool(
        name = "context_graph_ingest",
        description = "Ingest an external context event into the Universal Context Graph. Args: source (string), label (string), payload (object), optional workspace, optional user."
    )]
    pub fn context_graph_ingest(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let source = arg
            .get("source")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let label = arg
            .get("label")
            .and_then(|v| v.as_str())
            .unwrap_or("external context");
        let payload = arg.get("payload").unwrap_or(&serde_json::Value::Null);
        let ws = arg
            .get("workspace")
            .and_then(|v| v.as_str())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| workspace.to_path_buf());
        let user = arg.get("user").and_then(|v| v.as_str());
        let id =
            ContextGraph::global().record_external_context(source, label, payload, Some(&ws), user);
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "ingested": true,
            "node_id": id.0,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        name = "context_graph_compact",
        description = "Compact the append-only Universal Context Graph log"
    )]
    pub fn context_graph_compact(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let graph = ContextGraph::global();
        let _ = graph.replay();
        let (old_lines, new_lines) = graph
            .compact()
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "compacted": true,
            "old_lines": old_lines,
            "new_lines": new_lines,
        }))
        .unwrap_or_else(|_| "{}".to_string()))
    }

    #[tool(
        name = "ipc_grant",
        description = "Grant an inter-app permission scope. Args: grantor, grantee, resource, action, optional ttl_secs."
    )]
    pub fn ipc_grant(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let grantor = arg
            .get("grantor")
            .and_then(|v| v.as_str())
            .unwrap_or("susi");
        let grantee = arg
            .get("grantee")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_grant requires grantee"))?;
        let resource = arg
            .get("resource")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_grant requires resource"))?;
        let action = arg
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_grant requires action"))?;
        let ttl_secs = arg.get("ttl_secs").and_then(|v| v.as_u64());
        let scope = PermissionScope::new(resource, action);
        let grant = IpcBroker::global().grant_and_record(
            grantor,
            grantee,
            scope,
            ttl_secs,
            Some(workspace),
        );
        Ok(serde_json::to_string_pretty(&grant).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "ipc_request",
        description = "Request an inter-app permission (opens negotiation). Args: requester, resource, action, optional grantor, optional ttl_secs."
    )]
    pub fn ipc_request(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let requester = arg
            .get("requester")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_request requires requester"))?;
        let grantor = arg
            .get("grantor")
            .and_then(|v| v.as_str())
            .unwrap_or("susi");
        let resource = arg
            .get("resource")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_request requires resource"))?;
        let action = arg
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_request requires action"))?;
        let ttl_secs = arg.get("ttl_secs").and_then(|v| v.as_u64());
        let scope = PermissionScope::new(resource, action);
        let req = IpcBroker::global().request(requester, grantor, scope, ttl_secs);
        Ok(serde_json::to_string_pretty(&req).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "ipc_negotiate",
        description = "Approve or deny a pending permission request. Args: request_id, actor, approve (bool)."
    )]
    pub fn ipc_negotiate(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let request_id = arg
            .get("request_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_negotiate requires request_id"))?;
        let actor = arg.get("actor").and_then(|v| v.as_str()).unwrap_or("susi");
        let approve = arg
            .get("approve")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let resolved = IpcBroker::global()
            .negotiate(request_id, actor, approve, Some(workspace))
            .map_err(EaiError::governance)?;
        Ok(serde_json::to_string_pretty(&resolved).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "ipc_send",
        description = "Send a broker message (requires dispatch grant unless from=susi). Args: from, to, topic, payload."
    )]
    pub fn ipc_send(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let from = arg
            .get("from")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_send requires from"))?;
        let to = arg
            .get("to")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_send requires to"))?;
        let topic = arg
            .get("topic")
            .and_then(|v| v.as_str())
            .unwrap_or("message");
        let payload = arg
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let msg = IpcBroker::global()
            .send(from, to, topic, payload)
            .map_err(EaiError::governance)?;
        Ok(serde_json::to_string_pretty(&msg).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "ipc_receive",
        description = "Receive broker messages for an identity. Args: recipient, optional limit."
    )]
    pub fn ipc_receive(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let recipient = arg
            .get("recipient")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("ipc_receive requires recipient"))?;
        let limit = arg.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let msgs = IpcBroker::global().receive(recipient, limit);
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "recipient": recipient,
            "messages": msgs,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "host_telemetry",
        description = "Sample host thermal, battery, and load telemetry"
    )]
    pub fn host_telemetry(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let snapshot = sample_telemetry();
        if let Ok(snap) =
            serde_json::from_value::<crate::susi_core::TelemetrySnapshot>(snapshot.clone())
        {
            ContextGraph::global().record_telemetry(&snap, Some(workspace));
        }
        Ok(serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "apply_patch_cycle",
        description = "Apply a workspace-confined patch then run tests; revert on failure. Args: files[{path,old,new}], optional test_command, auto_apply, description."
    )]
    pub fn apply_patch_cycle(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        if arg
            .get("files")
            .and_then(|f| f.as_array())
            .is_none_or(|a| a.is_empty())
        {
            return Err(EaiError::governance("apply_patch_cycle requires files[]"));
        }
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
        let request_json = serde_json::to_string(arg)
            .map_err(|e| EaiError::governance(format!("serialize patch request: {e}")))?;
        gawd_hooks::apply_patch_cycle(workspace, &request_json, &cfg.trust_level())
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))
    }

    #[tool(
        name = "privacy_status",
        description = "Report privacy mode, mandatory sandbox, and cryptographic capability grants"
    )]
    pub fn privacy_status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        Ok(serde_json::to_string_pretty(
            &crate::susi_core::mac_policy::MacPolicy::global().status_json(),
        )
        .unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "privacy_consent_egress",
        description = "Grant time-limited network.egress (+ cloud.inference under local_only). Args: optional subject, optional ttl_secs."
    )]
    pub fn privacy_consent_egress(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let subject = arg
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("susi");
        let ttl = arg.get("ttl_secs").and_then(|v| v.as_u64()).unwrap_or(3600);
        let tokens = crate::susi_core::mac_policy::MacPolicy::global().consent_egress(subject, ttl);
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "consented": true,
            "tokens": tokens,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "intent_advertise",
        description = "Advertise a provider capability. Args: from, intent, optional payload."
    )]
    pub fn intent_advertise(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let from = arg.get("from").and_then(|v| v.as_str()).unwrap_or("agent");
        let intent = arg
            .get("intent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("intent_advertise requires intent"))?;
        let payload = arg
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let msg = crate::susi_core::intent_bus::IntentBus::global()
            .advertise(from, intent, payload, None);
        Ok(serde_json::to_string_pretty(&msg).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "intent_need",
        description = "Publish a need and match providers. Args: from, intent, optional min_score, limit."
    )]
    pub fn intent_need(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let from = arg
            .get("from")
            .and_then(|v| v.as_str())
            .unwrap_or("planner");
        let intent = arg
            .get("intent")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("intent_need requires intent"))?;
        let min_score = arg.get("min_score").and_then(|v| v.as_f64()).unwrap_or(0.2) as f32;
        let limit = arg.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let (need, matches) = crate::susi_core::intent_bus::IntentBus::global().need(
            from,
            intent,
            arg.get("payload")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
            None,
            min_score,
            limit,
        );
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "need": need,
            "matches": matches,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    #[cfg(feature = "tools-rich")]
    #[tool(
        name = "ambient_pulse",
        description = "Scan workspace FS changes into context graph and refresh semantic index"
    )]
    pub fn ambient_pulse(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let indexed = super::semantic_index::SemanticIndex::refresh(workspace).unwrap_or(0);
        ContextGraph::global().record_external_context(
            "ambient_pulse",
            "manual ambient pulse",
            &serde_json::json!({"docs_indexed": indexed}),
            Some(workspace),
            None,
        );
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "docs_indexed": indexed,
        }))
        .unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "tx_begin",
        description = "Begin multi-agent transaction. Args: description, files (array of relative paths)."
    )]
    pub fn tx_begin(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let description = arg
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("agent tx");
        let files: Vec<String> = arg
            .get("files")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let tx = crate::susi_core::agent_tx::TxManager::global()
            .begin(workspace, description, &files, Default::default())
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        Ok(serde_json::to_string_pretty(&tx).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(name = "tx_commit", description = "Commit open transaction. Args: id.")]
    pub fn tx_commit(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = arg
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("tx_commit requires id"))?;
        let tx = crate::susi_core::agent_tx::TxManager::global()
            .commit(id)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        Ok(serde_json::to_string_pretty(&tx).unwrap_or_else(|_| "{}".into()))
    }

    #[tool(
        name = "tx_abort",
        description = "Abort transaction and restore files. Args: id."
    )]
    pub fn tx_abort(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let id = arg
            .get("id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| EaiError::governance("tx_abort requires id"))?;
        let tx = crate::susi_core::agent_tx::TxManager::global()
            .abort(id, workspace)
            .map_err(|e| crate::susi_error::rewrap(e.kind_name(), e.to_string()))?;
        Ok(serde_json::to_string_pretty(&tx).unwrap_or_else(|_| "{}".into()))
    }
}

#[cfg(test)]
mod remote_result_tests {
    use super::remote_result_is_error;

    #[test]
    fn mcp_protocol_error_text_is_not_treated_as_tool_output() {
        // The observed live leak: a -32603 surfaced through Ok(String).
        assert!(remote_result_is_error(
            "Protocol Error: Mcp error: -32603: Unknown tool: reason"
        ));
        assert!(remote_result_is_error("[FAIL] MCP: timeout"));
        assert!(remote_result_is_error("MCP Error: tool crashed"));
        assert!(!remote_result_is_error("the capital of France is Paris"));
        assert!(!remote_result_is_error(
            "Much later in the text an error: is mentioned"
        ));
    }
}

#[cfg(test)]
mod shared_runtime_tests {
    use super::CoreTools;

    #[test]
    fn test_shared_runtime_succeeds_and_is_reused() {
        let rt1 = CoreTools::shared_runtime().expect("runtime should build on a healthy host");
        let val = rt1.block_on(async { 1 + 1 });
        assert_eq!(val, 2);

        // OnceLock caches the Result itself, so a second call returns the
        // same underlying runtime rather than rebuilding one.
        let rt2 = CoreTools::shared_runtime().expect("cached runtime should still be Ok");
        assert!(std::ptr::eq(rt1, rt2));
    }
}

#[cfg(test)]
mod unwired_governance_tests {
    use super::CoreTools;
    use std::path::Path;

    // With no EngineHooks wired, action-capable tools must fail closed — an
    // unaudited exec/reason path must never reach process execution or the
    // model. Real rejection semantics are covered by wired integration tests
    // in `tests/integration_tests.rs`.

    /// The bus rendezvous is cross-process (`<cache>/bus/*/`), so a sibling
    /// test process — or a live daemon on this host — could satisfy the
    /// `gawd.*` topics and defeat the "unwired" premise. Point the cache dir
    /// at a private temp root before the vendored `IpcPlaneBus` binds. Under
    /// nextest each test is its own process; under `cargo test` the env swap
    /// races with peers but only shrinks the visible rendezvous, which is
    /// the conservative direction for a fail-closed assertion.
    pub(super) fn isolate_bus_root() {
        // Hold the shared env lock while mutating: `SUSI_XDG` flips the whole
        // config-dir resolution mode, and a concurrent commit_log test's
        // seal→verify must observe one consistent env for both key reads.
        let _env = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir()
            .join("susi-unwired")
            .join(std::process::id().to_string());
        std::env::set_var("SUSI_XDG", "1");
        std::env::set_var("XDG_CACHE_HOME", &dir);
    }

    /// Serializes wired vs. unwired audit-gate tests inside this process:
    /// `PlaneBus::global()` is shared, so a `PermitAudit` registration from
    /// a wired test would otherwise stay live and let a later unwired test's
    /// governed call succeed — a false pass. Every test that either wires
    /// the permit or asserts fail-closed holds this lock for its duration.
    pub(super) static AUDIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// `PermitAudit` consults this flag — registered once process-wide, it
    /// only answers "permit" while a wired test holds `AUDIT_TEST_LOCK`.
    /// When clear it denies, so `audit_action` fails closed exactly as the
    /// truly-unwired path does.
    pub(super) static AUDIT_PERMIT: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    #[test]
    fn test_reason_fails_closed_when_unwired() {
        let _audit_lock = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        assert!(CoreTools::reason(&serde_json::json!("hi"), Path::new(".")).is_err());
    }

    #[test]
    fn test_exec_command_fails_closed_when_unwired() {
        let _audit_lock = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        assert!(CoreTools::exec_command(&serde_json::json!("ls"), Path::new(".")).is_err());
    }

    #[test]
    fn test_os_tools_fail_closed_when_unwired() {
        let _audit_lock = AUDIT_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        assert!(CoreTools::os_ps(&serde_json::json!({}), Path::new(".")).is_err());
        assert!(CoreTools::os_sysinfo(&serde_json::json!({}), Path::new(".")).is_err());
        assert!(CoreTools::os_services(&serde_json::json!({}), Path::new(".")).is_err());
        assert!(CoreTools::os_kill(&serde_json::json!({"pid": 1}), Path::new(".")).is_err());
        assert!(CoreTools::commit_record(&serde_json::json!({}), Path::new(".")).is_err());
    }
}

#[cfg(test)]
mod os_tools_wired_tests {
    //! Positive-path coverage: a permissive `gawd.audit.action` handler is
    //! registered on a pid-private bus root, so the governed tools actually
    //! execute instead of failing closed at the audit gate.
    use super::unwired_governance_tests::isolate_bus_root;
    use super::*;
    use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct PermitAudit;

    impl PlaneHandler for PermitAudit {
        fn handle(
            &self,
            _topic: &str,
            _payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if super::unwired_governance_tests::AUDIT_PERMIT
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                Ok(serde_json::json!({}))
            } else {
                // Registered process-wide on the shared bus — must deny
                // whenever no wired test holds the permit, or an unwired
                // fail-closed assertion running concurrently would pass.
                Err("audit gate closed — no active permissive test".to_string())
            }
        }
    }

    /// Wires `PermitAudit` and holds the audit-test lock until drop —
    /// serialized against the unwired fail-closed tests.
    struct AuditPermitGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for AuditPermitGuard {
        fn drop(&mut self) {
            super::unwired_governance_tests::AUDIT_PERMIT
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn wire_permissive_audit() -> AuditPermitGuard {
        let guard = super::unwired_governance_tests::AUDIT_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        isolate_bus_root();
        PlaneBus::global().register(topics::GAWD_AUDIT_ACTION, Arc::new(PermitAudit));
        super::unwired_governance_tests::AUDIT_PERMIT
            .store(true, std::sync::atomic::Ordering::SeqCst);
        AuditPermitGuard { _lock: guard }
    }

    #[test]
    fn os_services_lists_all_five_leaf_services() {
        let _permit = wire_permissive_audit();
        let out = CoreTools::os_services(&serde_json::json!({}), Path::new("."))
            .expect("os_services status");
        let statuses: serde_json::Value = serde_json::from_str(&out).expect("json array");
        assert_eq!(statuses.as_array().map(|a| a.len()), Some(5));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn os_ps_lists_this_test_process() {
        let _permit = wire_permissive_audit();
        let out =
            CoreTools::os_ps(&serde_json::json!({"limit": 500}), Path::new(".")).expect("os_ps");
        assert!(out.contains("PID\tNAME\tRSS_KB"));
        assert!(out.contains(&std::process::id().to_string()));
    }

    #[test]
    fn os_logs_rejects_unknown_service_and_missing_log() {
        let _permit = wire_permissive_audit();
        let err = CoreTools::os_logs(&serde_json::json!({"name": "not-a-svc"}), Path::new("."));
        assert!(err.is_err(), "unknown service must fail");

        // A real leaf name with no supervised log must error cleanly.
        let err = CoreTools::os_logs(&serde_json::json!({"name": "susi-paths"}), Path::new("."));
        // Either a missing log error or — if the host actually has a log —
        // content; never a panic or wrong-service read.
        if let Err(e) = err {
            assert!(e.to_string().contains("no log"), "got: {e}");
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn os_sysinfo_reports_kernel_and_memory() {
        let _permit = wire_permissive_audit();
        let out =
            CoreTools::os_sysinfo(&serde_json::json!({}), Path::new(".")).expect("os_sysinfo");
        assert!(out.contains("kernel: Linux"));
        assert!(out.contains("MemTotal:"));
    }

    #[test]
    fn os_kill_fails_closed_on_unsupervised_pid() {
        let _permit = wire_permissive_audit();
        // Audit passes, but the pid gate must still refuse: pid 4_000_000
        // is never in the substrate process table.
        let err = CoreTools::os_kill(
            &serde_json::json!({"pid": 4_000_000, "signal": "term"}),
            Path::new("."),
        )
        .expect_err("unsupervised pid must be refused");
        assert!(err.to_string().contains("supervised"));
    }

    #[test]
    fn os_kill_fails_closed_on_external_table_row() {
        let _permit = wire_permissive_audit();
        // External rows record processes the daemon observed but did not
        // spawn — the kill gate must exclude them. Write one in, verify
        // refusal, restore the table (the supervisor would also drop the
        // stale row on its next pass: the fake port is dead).
        let path = crate::susi_core::service_table::table_path();
        let original = std::fs::read_to_string(&path).unwrap_or_default();
        let mut table = crate::susi_core::service_table::load();
        crate::susi_core::service_table::record_external(&mut table, "susi-native", 4_000_001, 1);
        crate::susi_core::service_table::save(&table).expect("seed external row");
        let err = CoreTools::os_kill(
            &serde_json::json!({"pid": 4_000_001, "signal": "term"}),
            Path::new("."),
        )
        .expect_err("external row must not be killable");
        assert!(err.to_string().contains("supervised"));
        if original.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::write(&path, original).expect("restore table");
        }
    }

    #[test]
    fn os_kill_rejects_non_whitelisted_signals() {
        let _permit = wire_permissive_audit();
        assert!(CoreTools::os_kill(
            &serde_json::json!({"pid": 1, "signal": "STOP"}),
            Path::new("."),
        )
        .is_err());
    }

    /// Isolated config dir so `cluster.key` and `commit_log.jsonl` are
    /// created/read under a temp root, never the real `~/.susi`. Holds the
    /// shared env lock for the returned guard's lifetime — `cluster_key()`
    /// resolves through XDG env vars and other tests seal/verify against
    /// it, so the mutation must be exclusive for the whole test.
    fn isolate_config() -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let guard = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir()
            .join("susi-commit-test")
            .join(std::process::id().to_string());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp config dir");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        // susi_paths only honors XDG vars under SUSI_XDG when a legacy
        // ~/.susi exists — without this the tests would write the real
        // host's term.json/commit_log.jsonl.
        std::env::set_var("SUSI_XDG", "1");
        (guard, dir)
    }

    #[test]
    fn commit_record_accepts_signed_record_and_appends_to_ledger() {
        let _permit = wire_permissive_audit();
        let (_env, dir) = isolate_config();
        let Some(rec) = crate::susi_core::commit_log::CommitRecord::seal(
            crate::susi_core::commit_log::CommitInput {
                coordinator: "susi-local-master",
                leader: "susi-local-master",
                electorate: vec!["AgentA".into(), "AgentB".into(), "PeerNode_1".into()],
                tally: 2,
                quorum_threshold: 2,
                value: "committed answer",
            },
        ) else {
            eprintln!("skip: cluster key unavailable");
            return;
        };
        let out = CoreTools::commit_record(
            &serde_json::to_value(&rec).expect("record to json"),
            Path::new("."),
        )
        .expect("signed record must be accepted");
        assert!(out.contains("accepted"));

        let ledger = crate::susi_core::commit_log::load();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].value, "committed answer");

        let list =
            CoreTools::commit_log(&serde_json::json!({}), Path::new(".")).expect("commit_log list");
        assert!(list.contains("susi-local-master"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_record_rejects_forged_and_unsigned_records() {
        let _permit = wire_permissive_audit();
        let (_env, dir) = isolate_config();
        // Forged: no valid signature.
        let forged = serde_json::json!({
            "epoch": "aa", "coordinator": "evil-peer", "electorate": ["A", "B"],
            "tally": 2, "quorum_threshold": 2, "value_hash": "h",
            "value": "v", "committed_at": 0, "signature": "deadbeef"
        });
        assert!(CoreTools::commit_record(&forged, Path::new(".")).is_err());
        // And a correctly-shaped record whose value was tampered post-signing.
        if let Some(rec) = crate::susi_core::commit_log::CommitRecord::seal(
            crate::susi_core::commit_log::CommitInput {
                coordinator: "susi-local-master",
                leader: "susi-local-master",
                electorate: vec!["A".into(), "B".into()],
                tally: 2,
                quorum_threshold: 2,
                value: "honest",
            },
        ) {
            let mut tampered = serde_json::to_value(&rec).expect("json");
            tampered["value"] = serde_json::json!("forged payload");
            assert!(CoreTools::commit_record(&tampered, Path::new(".")).is_err());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_records_batch_intake_applies_skips_refuses() {
        let _permit = wire_permissive_audit();
        let (_env, dir) = isolate_config();
        let seal = |value: &str| {
            crate::susi_core::commit_log::CommitRecord::seal(
                crate::susi_core::commit_log::CommitInput {
                    coordinator: "susi-local-master",
                    leader: "susi-local-master",
                    electorate: vec!["A".into(), "B".into()],
                    tally: 2,
                    quorum_threshold: 2,
                    value,
                },
            )
        };
        let (Some(r1), Some(r2)) = (seal("v1"), seal("v2")) else {
            eprintln!("skip: cluster key unavailable");
            return;
        };
        // Seal produces seq from the ledger at seal time — r1 and r2 may
        // share a seq when the ledger is empty; the batch still applies
        // both (dup-seq handling lives in append_many).
        let out =
            CoreTools::commit_records(&serde_json::json!({ "records": [r1, r2] }), Path::new("."))
                .expect("batch must be accepted");
        let counts: serde_json::Value = serde_json::from_str(&out).expect("json summary");
        assert!(counts["applied"].as_u64().unwrap_or(0) >= 1);

        // Replay the same batch — everything held → all skipped, none applied.
        let out = CoreTools::commit_records(
            &serde_json::json!({ "records": [
                crate::susi_core::commit_log::load()[0].clone()
            ] }),
            Path::new("."),
        )
        .expect("re-batch must be accepted");
        let counts: serde_json::Value = serde_json::from_str(&out).expect("json summary");
        assert_eq!(counts["applied"].as_u64(), Some(0));

        // A forged record inside an otherwise fine batch is refused, not fatal.
        let forged = serde_json::json!({
            "epoch": "aa", "coordinator": "evil", "electorate": ["A", "B"],
            "tally": 2, "quorum_threshold": 2, "value_hash": "h",
            "value": "v", "committed_at": 0, "signature": "deadbeef",
            "seq": 1, "term": 1, "leader": "evil", "prev_epoch": "",
            "member_sig": "", "member_pubkey": "", "subject_sig": "",
            "endorsements": []
        });
        let out =
            CoreTools::commit_records(&serde_json::json!({ "records": [forged] }), Path::new("."))
                .expect("mixed batch still returns a summary");
        let counts: serde_json::Value = serde_json::from_str(&out).expect("json summary");
        assert_eq!(counts["refused"].as_u64(), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_log_fetch_filters_by_coordinator_and_seq() {
        let _permit = wire_permissive_audit();
        let (_env, dir) = isolate_config();
        let seal = |value: &str| {
            crate::susi_core::commit_log::CommitRecord::seal(
                crate::susi_core::commit_log::CommitInput {
                    coordinator: "c",
                    leader: "c",
                    electorate: vec!["A".into(), "B".into()],
                    tally: 2,
                    quorum_threshold: 2,
                    value,
                },
            )
        };
        let (Some(r1), ..) = (seal("v1"),) else {
            return;
        };
        crate::susi_core::commit_log::append(&r1).expect("append r1");
        // seq is assigned from the ledger at seal time — append first so
        // the second record gets seq 2.
        let Some(r2) = seal("v2") else { return };
        assert_eq!(r2.seq, 2);
        crate::susi_core::commit_log::append(&r2).expect("append r2");

        let out = CoreTools::commit_log_fetch(
            &serde_json::json!({"coordinator": "c", "from_seq": 2}),
            Path::new("."),
        )
        .expect("fetch");
        let got: Vec<crate::susi_core::commit_log::CommitRecord> =
            serde_json::from_str(&out).expect("records json");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].seq, 2);
        assert_eq!(got[0].value, "v2");

        // limit bounds the pull even when more records match.
        let out = CoreTools::commit_log_fetch(
            &serde_json::json!({"coordinator": "c", "limit": 1}),
            Path::new("."),
        )
        .expect("fetch limited");
        let got: Vec<crate::susi_core::commit_log::CommitRecord> =
            serde_json::from_str(&out).expect("records json");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].value, "v1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_record_reports_gap_when_repair_unreachable() {
        let _permit = wire_permissive_audit();
        let (_env, dir) = isolate_config();
        let seal = |value: &str| {
            crate::susi_core::commit_log::CommitRecord::seal(
                crate::susi_core::commit_log::CommitInput {
                    coordinator: "ghost-coordinator",
                    leader: "ghost-coordinator",
                    electorate: vec!["A".into(), "B".into()],
                    tally: 2,
                    quorum_threshold: 2,
                    value,
                },
            )
        };
        let (Some(r3),) = (seal("v3"),) else { return };
        // seq 3 arrives with nothing held for this coordinator → gap
        // [1,2]; repair can't resolve "ghost-coordinator" in the roster
        // (no gawd.cluster.peers handler on the test bus), so the
        // response must surface the warning, not swallow it.
        let mut r3 = r3;
        r3.seq = 3;
        if let Some(k) = crate::susi_config::cluster_key::cluster_key() {
            r3.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &k,
                r3.signed_payload().as_bytes(),
            );
        }
        let out =
            CoreTools::commit_record(&serde_json::to_value(&r3).expect("json"), Path::new("."))
                .expect("gap record still commits");
        assert!(out.contains("WARNING: replication gap"), "got: {out}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn commit_record_rejects_stale_term_and_adopts_newer() {
        let _permit = wire_permissive_audit();
        let (_env, dir) = isolate_config();
        // Coordinator authority requires explicit membership — declare
        // the test's coordinators in the isolated roster so their
        // records may drive term state.
        let peers_dir = crate::susi_paths::SusiDirs::config_dir();
        std::fs::create_dir_all(&peers_dir).expect("peers dir");
        std::fs::write(
            peers_dir.join("peers.json"),
            serde_json::to_string(&serde_json::json!([
                { "node_id": "leader-a", "admission": "explicit" },
                { "node_id": "leader-c", "admission": "explicit" },
            ]))
            .expect("peers json"),
        )
        .expect("write peers.json");
        // Establish term 1 under leader-a, seal a record there, then move
        // the cluster to term 2 — the term-1 push must now be rejected.
        crate::susi_core::commit_log::claim_leadership("leader-a");
        let Some(rec) = crate::susi_core::commit_log::CommitRecord::seal(
            crate::susi_core::commit_log::CommitInput {
                coordinator: "leader-a",
                leader: "leader-a",
                electorate: vec!["A".into(), "B".into()],
                tally: 2,
                quorum_threshold: 2,
                value: "term-1 decision",
            },
        ) else {
            eprintln!("skip: cluster key unavailable");
            return;
        };
        assert_eq!(rec.term, 1);
        crate::susi_core::commit_log::claim_leadership("leader-b");
        assert_eq!(crate::susi_core::commit_log::load_term().term, 2);

        let err =
            CoreTools::commit_record(&serde_json::to_value(&rec).expect("json"), Path::new("."));
        assert!(err.is_err(), "stale-term push must be rejected");
        assert!(err.unwrap_err().to_string().contains("stale term"));

        // A record from a newer term is accepted AND adopts the term.
        let mut ahead = rec.clone();
        ahead.term = 7;
        ahead.leader = "leader-c".into();
        if let Some(k) = crate::susi_config::cluster_key::cluster_key() {
            ahead.signature = crate::susi_config::cluster_key::hmac_sha256_hex(
                &k,
                ahead.signed_payload().as_bytes(),
            );
        }
        let out =
            CoreTools::commit_record(&serde_json::to_value(&ahead).expect("json"), Path::new("."))
                .expect("newer-term record accepted");
        assert!(out.contains("adopted term 7"), "got: {out}");
        assert_eq!(crate::susi_core::commit_log::load_term().term, 7);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
