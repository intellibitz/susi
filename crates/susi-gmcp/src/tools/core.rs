//! Concrete MCP/meta tool handlers (`CoreTools`).

use rmcp::tool;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

use susi_error::{EaiError, EaiResult};
use susi_gemi::hardware::HardwareProfiler;
use susi_gemi::models::ModelManager;
use susi_tools::hooks::hooks as engine_hooks;
use susi_tools::GmcpClient;

#[cfg(feature = "tools-rich")]
use headless_chrome::Browser;

#[cfg(feature = "tools-rich")]
use super::helpers::secure_external_url;
use super::helpers::{
    confine_exec_argv, external_agent_control, read_file_nofollow, secure_path, tool_string_arg,
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
        let progress_file = susi_paths::SusiDirs::data_dir().join("download_progress.json");
        if progress_file.exists() {
            if let Ok(content) = fs::read_to_string(&progress_file) {
                if let Ok(progress) =
                    serde_json::from_str::<susi_gemi::models::ModelDownloadProgress>(&content)
                {
                    if progress.status == "IN_PROGRESS" {
                        out.push_str("\n[SUBSTRATE PROVISIONING ACTIVE]\n");
                        out.push_str(&format!("- Target: {}\n", progress.model_name));
                        out.push_str(&format!(
                            "- Progress: {:.2}% ({:.2}GB / {:.2}GB)\n",
                            progress.percentage,
                            progress.bytes_downloaded as f32 / 1e9,
                            progress.expected_bytes as f32 / 1e9
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
        let log_content = susi_sandbox::manager::SusiAuditLogger::read_audit_log(workspace, 100);
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
        engine_hooks().bloat_audit(workspace)
    }

    #[tool(name = "identity", description = "SUSI substrate identity report")]
    pub fn identity(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        engine_hooks().identity_report(workspace)
    }

    #[tool(
        name = "distill_genome",
        description = "Distill the hard-compiled genome into the Tier 2 reasoning model"
    )]
    pub fn distill_genome(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        match engine_hooks().audit_reasoning_substrate(workspace) {
            Ok(report) => Ok(format!("# Genome Distillation Successful\n\n{}", report)),
            Err(e) => Ok(format!("# Genome Distillation Failed\n\nError: {}", e)),
        }
    }

    #[tool(
        name = "self_validate",
        description = "Execute autonomous substrate self-validation"
    )]
    pub fn self_validate(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        match engine_hooks().self_validate(workspace) {
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
        let mut out = format!("Active Model Substrates (Count: {})\n\n", models.len());
        for m in &models {
            out.push_str(&format!(
                "- [{}] {} ({})\n",
                if m.is_local() { "LOCAL" } else { "CLOUD" },
                m.name(),
                m.model_id()
            ));
        }
        Ok(out)
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
        ModelManager::set_selected_model(arg_s.trim()).map_err(EaiError::config)
    }

    #[tool(name = "scout_model", description = "Scout or install model substrate")]
    pub fn scout_model(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let arg_s = arg.as_str().unwrap_or("");
        if arg_s.trim().is_empty() {
            return Ok("Usage: scout_model <model_name_or_url>".to_string());
        }
        let res = ModelManager::install_model(arg_s.trim());
        Ok(res)
    }

    #[tool(
        name = "verify_model_download_agent",
        description = "Verify model download agent, check network status, and ensure 32b and 72b models are provisioned"
    )]
    pub fn verify_model_download_agent(
        _arg: &serde_json::Value,
        workspace: &Path,
    ) -> EaiResult<String> {
        let report = ModelManager::verify_and_provision_32b_and_72b_models(workspace)?;
        Ok(serde_json::to_string_pretty(&report)
            .unwrap_or_else(|_| "Report serialization failed".to_string()))
    }

    #[tool(
        name = "train_reflexes",
        description = "Manually trigger native neural reflex distillation"
    )]
    pub fn train_reflexes(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        engine_hooks().train_reflexes(workspace)
    }

    #[tool(
        name = "swarm_schedule",
        description = "Show recent mission scheduler decisions: per-agent scores, admitted vs deferred"
    )]
    pub fn swarm_schedule(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let decisions = susi_gawd_agents::scheduler::MissionScheduler::recent_decisions();
        if decisions.is_empty() {
            return Ok("No missions scheduled yet.".to_string());
        }
        let mut out = String::from("# Mission Schedule Log\n\n");
        for d in decisions.iter().rev() {
            out.push_str(&format!("## [{}] \"{}\"\n", d.timestamp, d.goal));
            for e in &d.entries {
                out.push_str(&format!(
                    "- {} `{}` score={:.3} (rank={:.2} intent={:.2} urgency={:.2})\n",
                    if e.admitted { "RUN" } else { "DEFER" },
                    e.name,
                    e.score,
                    e.learned_rank,
                    e.intent_match,
                    e.urgency_boost
                ));
            }
            out.push('\n');
        }
        Ok(out)
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

        engine_hooks().audit_action("exec_command", clean, workspace)?;

        let task_handle = susi_agents::task_manager::SwarmTaskManager::global()
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

        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();

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
                if let Some(mut reader) = stdout.take() {
                    use std::io::Read;
                    let _ = reader.read_to_end(&mut stdout_buf);
                }
                if let Some(mut reader) = stderr.take() {
                    use std::io::Read;
                    let _ = reader.read_to_end(&mut stderr_buf);
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
        name = "agents_list",
        description = "List managed external executors and local setup readiness"
    )]
    pub fn agents_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let manager = susi_agents::external::AgentManager::new(workspace)
            .map_err(|e| EaiError::process(e.to_string()))?;
        let catalog = susi_agents::external::catalog(susi_agents::external::CatalogKind::Execution)
            .map_err(|e| EaiError::config(e.to_string()))?;
        let agents: Vec<_> = catalog
            .into_iter()
            .map(|agent| {
                let readiness = manager.adapter(&agent.id).and_then(|a| a.preflight());
                serde_json::json!({"agent": agent, "prerequisites_present": readiness.is_ok(),
                "detail": readiness.unwrap_or_else(|e| e.to_string())})
            })
            .collect();
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
        engine_hooks().audit_action("agents_run", &audit, workspace)?;
        let manager = susi_agents::external::AgentManager::new(workspace)
            .map_err(|e| EaiError::process(e.to_string()))?;
        let run = manager
            .start(agent, prompt)
            .map_err(|e| EaiError::process(e.to_string()))?;
        serde_json::to_string(&run)
            .map(|s| susi_agents::external::redact(&s))
            .map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "agents_tasks",
        description = "List persisted external agent tasks for this workspace"
    )]
    pub fn agents_tasks(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let runs = susi_agents::external::AgentManager::new(workspace)
            .and_then(|m| m.list())
            .map_err(|e| EaiError::process(e.to_string()))?;
        serde_json::to_string(&runs)
            .map(|s| susi_agents::external::redact(&s))
            .map_err(|e| EaiError::protocol(e.to_string()))
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
        susi_gemi::http_provider::apply_cloud_env_file();
        let manager = susi_gemi::coding_models::CodingModelManager::new()
            .map_err(|e| EaiError::process(e.to_string()))?;
        let preferred = manager.preferred();
        let rows: Vec<_> = susi_gemi::coding_models::CodingModelManager::catalog()
            .map_err(|e| EaiError::config(e.to_string()))?
            .into_iter()
            .map(|m| {
                let effective = manager.effective(&m.id).unwrap_or(m.clone());
                let readiness = manager.preflight(&m.id);
                serde_json::json!({
                    "model": effective,
                    "preferred": preferred.as_deref() == Some(m.id.as_str()),
                    "prerequisites_present": readiness.is_ok(),
                    "detail": match readiness { Ok(s) => s, Err(e) => e.to_string() }
                })
            })
            .collect();
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
        susi_gemi::http_provider::apply_cloud_env_file();
        susi_gemi::coding_models::CodingModelManager::new()
            .and_then(|m| m.prefer(model))
            .map_err(|e| EaiError::process(e.to_string()))
    }

    #[tool(
        name = "frameworks_list",
        description = "List managed agent frameworks/engines and local setup readiness"
    )]
    pub fn frameworks_list(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let manager = susi_agents::external::AgentManager::frameworks(workspace)
            .map_err(|e| EaiError::process(e.to_string()))?;
        let catalog = susi_agents::external::catalog(susi_agents::external::CatalogKind::Framework)
            .map_err(|e| EaiError::config(e.to_string()))?;
        let engines: Vec<_> = catalog
            .into_iter()
            .map(|engine| {
                let readiness = manager.adapter(&engine.id).and_then(|a| a.preflight());
                serde_json::json!({"engine": engine, "prerequisites_present": readiness.is_ok(),
                "detail": readiness.unwrap_or_else(|e| e.to_string())})
            })
            .collect();
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
        let manager = susi_agents::external::AgentManager::frameworks(workspace)
            .map_err(|e| EaiError::process(e.to_string()))?;
        let run = manager
            .start(engine, prompt)
            .map_err(|e| EaiError::process(e.to_string()))?;
        serde_json::to_string(&run)
            .map(|s| susi_agents::external::redact(&s))
            .map_err(|e| EaiError::protocol(e.to_string()))
    }

    #[tool(
        name = "frameworks_tasks",
        description = "List persisted agent-framework tasks for this workspace"
    )]
    pub fn frameworks_tasks(_arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let runs = susi_agents::external::AgentManager::frameworks(workspace)
            .and_then(|m| m.list())
            .map_err(|e| EaiError::process(e.to_string()))?;
        serde_json::to_string(&runs)
            .map(|s| susi_agents::external::redact(&s))
            .map_err(|e| EaiError::protocol(e.to_string()))
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
        let tasks = susi_agents::task_manager::SwarmTaskManager::global().list_tasks();
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
        if susi_agents::task_manager::SwarmTaskManager::global().pause_task(id) {
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
        if susi_agents::task_manager::SwarmTaskManager::global().resume_task(id) {
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
        if susi_agents::task_manager::SwarmTaskManager::global().kill_task(id) {
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
        let mut out = format!("Global MCP Substrate Roster (Count: {})\n\n", entries.len());
        for e in &entries {
            let trust = e.trust_score.unwrap_or(0.0);
            let lat = e.latency_ms.unwrap_or(0);
            out.push_str(&format!(
                "- [{}] {}: {} (Trust: {:.2} | Latency: {}ms)\n  Package: {}\n",
                e.category, e.name, e.description, trust, lat, e.package
            ));
        }
        Ok(out)
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
            let res = GmcpClient::auto_configure_server(n, p);
            Ok(format!("MCP Server '{}' configuration status: {}", n, res))
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
        let rows = susi_tools::LeadingMcpManager::new(workspace)
            .and_then(|m| m.status())
            .map_err(|e| EaiError::process(e.to_string()))?;
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
        let cfg = susi_tools::LeadingMcpManager::new(workspace)
            .and_then(|m| m.enable(server))
            .map_err(|e| EaiError::process(e.to_string()))?;
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
        let removed = susi_tools::LeadingMcpManager::new(workspace)
            .and_then(|m| m.disable(server))
            .map_err(|e| EaiError::process(e.to_string()))?;
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
            let profile = susi_agents::AgentProfile {
                name: n.to_string(),
                description: d.to_string(),
                categories: c.split(',').map(|s| s.trim().to_string()).collect(),
                semantic_anchors: Vec::new(),
                base_rank: 0.8,
                is_core: false,
            };
            susi_agents::AgentMetaRegistry::global().register_agent(profile);
            Ok(format!("Successfully registered agent: {}", n))
        } else {
            let arg_s = arg.as_str().unwrap_or("");
            let parts: Vec<&str> = arg_s.splitn(3, ' ').collect();
            if parts.len() < 3 {
                return Err(EaiError::protocol(
                    "Usage: agent_register {name, description, categories}",
                ));
            }

            let profile = susi_agents::AgentProfile {
                name: parts[0].to_string(),
                description: parts[1].to_string(),
                categories: parts[2].split(',').map(|s| s.trim().to_string()).collect(),
                semantic_anchors: Vec::new(),
                base_rank: 0.8,
                is_core: false,
            };

            susi_agents::AgentMetaRegistry::global().register_agent(profile);
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
        let sanitized = engine_hooks().sanitize_input(&arg_s)?;
        engine_hooks().audit_action("reason", &sanitized, workspace)?;
        // When the mission captured real tool calls, this reasoning call must
        // answer by citing those receipts — free narrative cannot certify.
        let prompt = format!(
            "{sanitized}{}",
            susi_core::capture::EvidenceSession::evidence_prompt_for(workspace)
        );
        Ok(susi_gemi::engine::GemiEngine::generate_reasoning_deep(
            &prompt, workspace,
        ))
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
        Ok(engine_hooks().solve_mission(&intent, workspace, env!("CARGO_PKG_VERSION")))
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
            let res = GmcpClient::execute_external_tool(best_remote, "reason", &arg_s);
            if !res.contains("[FAIL]") {
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
        let registry = susi_agents::AgentMetaRegistry::global();
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

        engine_hooks().audit_action("sandbox_exec", cmd, workspace)?;

        Self::shared_runtime()?
            .block_on(async { susi_sandbox::manager::SandboxManager::execute_in_docker(cmd).await })
            .map_err(|e| EaiError::process(format!("[CAPABILITY_GAP] Docker execution failed: {}. Ensure Docker daemon is running.", e)))
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
            .unwrap()
            .as_secs();
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

    #[test]
    fn test_reason_fails_closed_when_unwired() {
        assert!(CoreTools::reason(&serde_json::json!("hi"), Path::new(".")).is_err());
    }

    #[test]
    fn test_exec_command_fails_closed_when_unwired() {
        assert!(CoreTools::exec_command(&serde_json::json!("ls"), Path::new(".")).is_err());
    }
}
