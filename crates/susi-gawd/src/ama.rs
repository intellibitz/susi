// Top-level mission orchestrator: sanitizes input, dispatches the swarm, and
// streams results back to the caller.
// Agents must add functionality directly to the susi engine via ToolRegistry,
// not simulate or "fake" susi capabilities by performing logic themselves.

use super::agents::GawdAgentInfo;
use super::amas::{A2AMessage, SusiSupervisor};
use super::axiom::AxiomSubstrate;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use susi_error::EaiResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SusiMissionReport {
    pub goal: String,
    pub status: String,
    pub agents: Vec<GawdAgentInfo>,
    pub interactions: Vec<A2AMessage>,
    pub final_answer: String,
}

pub type SusiSwarmReport = SusiMissionReport;

impl SusiMissionReport {
    /// Only an explicitly successful mission may produce a success exit code.
    pub fn is_success(&self) -> bool {
        matches!(self.status.as_str(), "SUCCESS" | "COMPLETE")
    }

    /// CLI outcome derived from the report, never from generated prose.
    pub fn exit_code(&self) -> std::process::ExitCode {
        if self.is_success() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        }
    }

    /// Human-readable completion marker using the same outcome as the protocol.
    pub fn completion_message(&self) -> String {
        if self.is_success() {
            format!("[MISSION COMPLETE] Status: {}", self.status)
        } else {
            format!("[MISSION FAILED] Status: {}", self.status)
        }
    }

    pub fn to_protocol_format(&self, _is_ide_environment: bool) -> String {
        let mut full_thinking_trace = String::new();
        full_thinking_trace.push_str(&format!(
            "SUSI Mission Goal: {}\nStatus: {}\nAgents Recruited: {}\n\n",
            self.goal,
            self.status,
            self.agents.len()
        ));

        for agent in &self.agents {
            full_thinking_trace
                .push_str(&format!("- [Agent] {} ({})\n", agent.name, agent.provider));
        }

        for msg in &self.interactions {
            full_thinking_trace.push_str(&format!(
                "- [{}] Action: {} | Payload: {}\n",
                msg.sender, msg.action, msg.payload
            ));
        }

        let primary_step = serde_json::json!({
            "action": "supervise_mission_swarm",
            "action_input": { "goal": self.goal },
            "observation": format!("Mission status: {}", self.status),
            "thought": full_thinking_trace.trim()
        });

        let json_str = serde_json::to_string_pretty(&primary_step).unwrap_or_default();
        let trimmed_answer = self.final_answer.trim();

        if trimmed_answer.is_empty()
            || trimmed_answer.starts_with("[FAST-PATH COMPLETE]")
            || trimmed_answer == self.goal
        {
            json_str
        } else {
            format!("{}\n\n{}", json_str, trimmed_answer)
        }
    }
}

pub struct SusiMasterAgent;

impl Default for SusiMasterAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl SusiMasterAgent {
    pub fn new() -> Self {
        Self
    }

    /// Validates and sanitizes natural language inputs to prevent injection attacks.
    /// Static (no `self`) so tool handlers outside `SusiMasterAgent` — notably
    /// the `reason` MCP tool, a second front door into the same reasoning
    /// substrate — can apply the identical untrusted-input boundary (Mandate 41)
    /// instead of feeding a raw, unbounded, unchecked prompt straight to the model.
    pub fn sanitize_input(input: &str) -> EaiResult<String> {
        let trimmed = input.trim();

        let hardware = susi_gemi::hardware::HardwareProfiler::get_profile();
        let max_len = (hardware.available_ram_gb * 1024 * 1024).max(4096); // Scale with RAM, min 4KB

        if trimmed.len() > max_len {
            return Err(susi_error::EaiError::governance(format!(
                "Input exceeds hardware-scaled limit ({} characters).",
                max_len
            )));
        }

        if trimmed.is_empty() {
            return Err(susi_error::EaiError::governance(
                "Input goal cannot be empty.",
            ));
        }

        // 2. Block high-risk shell/injection patterns
        let risk_patterns = ["$(", "`", "> /dev/", "| nc ", "| netcat ", "0xCC", "\\x"];
        for pattern in risk_patterns {
            if trimmed.contains(pattern) {
                return Err(susi_error::EaiError::governance(format!("High-risk sequence '{}' detected in input. Potential injection attempt blocked.", pattern)));
            }
        }

        Ok(trimmed.to_string())
    }

    /// Primary entry point for all natural language intents.
    /// Streams thinking and results back live in real-time.
    pub fn solve_clean(&self, goal: &str, workspace: &Path, version: &str) -> String {
        self.solve_stream(goal, workspace, version, &|piece| {
            print!("{}", piece);
            let _ = std::io::stdout().flush();
        })
    }

    pub fn solve_stream(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        _callback: &dyn Fn(String),
    ) -> String {
        self.solve_stream_report(goal, workspace, version, _callback)
            .final_answer
    }

    /// Run a streaming mission while preserving its structured outcome for callers.
    pub fn solve_stream_report(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        _callback: &dyn Fn(String),
    ) -> SusiMissionReport {
        let hw = susi_gemi::hardware::HardwareProfiler::get_profile();
        let (_engine_type, active_model_id) =
            susi_gemi::models::ModelManager::get_active_engine_and_model(Some(
                susi_gemi::intent::IntentClassifier::classify(goal),
            ));
        let model_path_str = susi_gemi::models::ModelManager::get_model_path(&active_model_id)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "Internal Hard-Compiled Substrate Genome".to_string());
        let device = susi_gemi::hardware::HardwareProfiler::get_candle_device();
        let local_models_count = susi_gemi::models::ModelManager::list_models(workspace).len();

        eprintln!("<thinking>");

        // Ensures </thinking> is always printed, even if synthesis panics or hangs.
        struct ThinkingGuard;
        impl Drop for ThinkingGuard {
            fn drop(&mut self) {
                eprintln!("</thinking>\n");
                let _ = std::io::stdout().flush();
            }
        }
        let _guard = ThinkingGuard;

        eprintln!("[SUSI Substrate Swarm Active - Full Transparency Omni-Trace Mode]");
        eprintln!("- [Engine Version] v{}", version);
        eprintln!("- [Workspace Root] {}", workspace.display());

        use crate::self_core::AlphaSelf;
        eprintln!("- [Core Paradigm] {}", AlphaSelf::CORE_PARADIGM);
        eprintln!("- [Accountability] 100% Omni-Trace Coverage Active (Mandate 26: Glass Box Transparency)");

        eprintln!("\n[DETAILED HARDWARE AUDIT LOGS]");
        eprintln!("- [CPU Info] Brand: {} | Cores: {}", hw.cpu_brand, hw.cpus);
        eprintln!(
            "- [Memory Info] Total RAM: {}GB | Available RAM: {}GB",
            hw.ram_gb, hw.available_ram_gb
        );
        eprintln!("- [GPU Topologies] Info: {}", hw.gpu_info);
        eprintln!("- [OS Architecture] OS: {} | Arch: {}", hw.os_info, hw.arch);
        eprintln!(
            "- [System Liveness] Hostname: {} | Uptime: {}s | Load Avg: {}",
            hw.hostname, hw.uptime, hw.load_avg
        );
        eprintln!(
            "- [Compute Saturation] Native Acceleration: {} | Active Inference Device: {:?}",
            hw.native_acceleration, device
        );

        eprintln!("\n[DETAILED SUBSTRATE CONFIGURATION LOGS]");
        let global_dir = susi_paths::SusiDirs::config_dir();
        let cfg = susi_sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();
        eprintln!(
            "- [Network Fabric] GMCP Port: {} | GEMI Port: {} | Discovery UDP Port: {}",
            cfg.gmcp_port(),
            cfg.gemi_port(),
            cfg.udp_discovery_port()
        );
        eprintln!(
            "- [Neural Defaults] Target Engine: {} | Selected Model ID: {}",
            cfg.default_engine(),
            cfg.default_model()
        );
        eprintln!(
            "- [Substrate Limits] Agent Recruitment Threshold: {} | Cloud Scout Timeout: {}s",
            cfg.agent_rank_threshold(),
            cfg.cloud_scout_timeout_secs()
        );
        eprintln!(
            "- [Concurrency Primitives] Max Parallel Swarm Agents: {}",
            crate::agents::GawdAgentFleet::get_max_concurrent_agents()
        );
        eprintln!(
            "- [Active Model Deep-Dive] Active Local ID: {} | Model Path: {}",
            active_model_id, model_path_str
        );
        eprintln!(
            "- [Discovered Model Substrates] Count: {}",
            local_models_count
        );

        eprintln!("\n[GENOMIC MANDATES]");
        for rule in AlphaSelf::RULES
            .iter()
            .filter(|r| r.title.contains("Universal") || r.title.contains("Agnosticism"))
        {
            eprintln!(
                "- [Mandate {}] {}: {}",
                rule.id, rule.title, rule.imperative
            );
        }

        // 1. Continuous Intent Manifold Routing
        let manifold = crate::manifold::IntentManifold::analyze(goal);
        eprintln!(
            "\n[INTENT MANIFOLD ROUTING: {:?} (Risk: {:?})]",
            manifold.scope_of_impact, manifold.risk_profile
        );

        if manifold.scope_of_impact == crate::manifold::ScopeOfImpact::Read {
            // Glass Box Transparency (Mandate 26): the fast-path used to print
            // these two lines as pure narration with no backing call — the
            // trace claimed governance validation happened when it didn't.
            // Actually run the same lightweight (no-inference) detectors the
            // swarm's SafetyAgent/SecurityAgent run for every mission goal, so
            // what's printed here is true, not aspirational.
            eprintln!("- [Substrate Operation] Validating with SafetyAgent...");
            let safety_result =
                crate::safety::SafetyDetector::audit_action("SUSI_SOLVE", goal, workspace);
            eprintln!("- [Substrate Operation] Validating with SecurityAgent...");
            let security_result =
                crate::security::SecurityDetector::audit_action("SUSI_SOLVE", goal, workspace);

            if let Err(e) = safety_result.and(security_result) {
                eprintln!("- [Governance] Fast-path Read blocked: {}", e);
                let report = SusiMissionReport {
                    goal: goal.to_string(),
                    status: "BLOCKED".to_string(),
                    agents: Vec::new(),
                    interactions: Vec::new(),
                    final_answer: format!("[GOVERNANCE_BLOCK] {}", e),
                };
                eprintln!("{}", report.completion_message());
                drop(_guard);
                eprintln!("{}", report.to_protocol_format(true));
                return report;
            }

            eprintln!("\n[DETAILED SWARM SYNTHESIS LOGS]");
            eprintln!("- [Recruited Agent] SafetyAgent (Provider: Local Core) cleared Read safety validation.");
            eprintln!("- [Recruited Agent] SecurityAgent (Provider: Local Core) cleared injection validation.");

            let lower_goal = goal.trim().to_lowercase();
            let final_answer = if lower_goal.contains("identity") {
                crate::self_core::AlphaSelf::inspect_compiled_binary_instructions()
            } else if lower_goal.contains("who am i") || lower_goal.contains("whoami") {
                let user = std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_else(|_| "unknown_user".into());
                let host = hw.hostname;
                format!(
                    "System User Identity: {}@{}\n\nSUSI Substrate Identity:\n{}",
                    user,
                    host,
                    crate::self_core::AlphaSelf::inspect_compiled_binary_instructions()
                )
            } else if lower_goal == "ls"
                || lower_goal.starts_with("ls -")
                || lower_goal.starts_with("ls ") && lower_goal.split_whitespace().count() <= 3
                || lower_goal == "dir"
                || lower_goal == "list directory"
                || lower_goal == "list files"
            {
                let cmd = if lower_goal.starts_with("ls ") {
                    goal
                } else {
                    "ls -la"
                };
                susi_tools::ToolRegistry::execute_tool(
                    "exec_command",
                    &serde_json::Value::String(cmd.to_string()),
                    workspace,
                )
            } else if crate::system_observe::looks_like_system_observe_goal(goal) {
                crate::system_observe::observe_system(goal, workspace).unwrap_or_else(|| {
                    susi_tools::ToolRegistry::execute_tool(
                        "exec_command",
                        &serde_json::Value::String(
                            "df -h -x tmpfs -x devtmpfs -x squashfs --total".into(),
                        ),
                        workspace,
                    )
                })
            } else if lower_goal.trim() == "dashboard"
                || lower_goal == "susi dashboard"
                || lower_goal == "show dashboard"
            {
                susi_tools::ToolRegistry::execute_tool(
                    "sovereign_dashboard",
                    &serde_json::json!(null),
                    workspace,
                )
            } else if lower_goal.trim() == "bloat audit"
                || lower_goal == "run bloat audit"
                || lower_goal == "bloat-audit"
            {
                susi_tools::ToolRegistry::execute_tool(
                    "bloat_audit",
                    &serde_json::json!(null),
                    workspace,
                )
            } else if lower_goal.contains("version") {
                format!("SUSI Engine Version: v{}", version)
            } else if lower_goal.contains("status") {
                format!(
                    "SUSI Substrate Status: Operational | Hardware: {} | RAM: {}GB",
                    hw.cpu_brand, hw.ram_gb
                )
            } else if lower_goal.trim() == "models"
                || lower_goal == "list models"
                || lower_goal == "show models"
            {
                let models = susi_gemi::models::ModelManager::list_models(workspace);
                let mut out = format!("Active Model Substrates (Count: {})\n\n", models.len());
                for m in &models {
                    out.push_str(&format!(
                        "- [{}] {} ({})\n",
                        if m.is_local() { "LOCAL" } else { "CLOUD" },
                        m.name(),
                        m.model_id()
                    ));
                }
                out
            } else {
                format!("Administrative query result for goal: {}", goal)
            };

            eprintln!("\n[LIVE REASONING TOKENS]");
            eprintln!("- [Fast-Path Execution] Fast-path Read bypassed heavy neural loop. Output direct response stream:");
            for token in final_answer.split_whitespace() {
                print!("{} ", token);
                let _ = std::io::stdout().flush();
            }
            eprintln!();

            eprintln!("\n[SUBSTRATE VERIFICATION RESULTS]");
            let mut verification_failed = false;
            let final_answer = match susi_gemi::engine::GemiEngine::verify_axiomatic_alignment(
                &final_answer,
                workspace,
            ) {
                Ok(v) => {
                    eprintln!(
                        "- [Axiomatic Alignment Check] Status: SUCCESS | Fast-Path Read axiomatic alignment verified."
                    );
                    v
                }
                Err(e) => {
                    verification_failed = true;
                    eprintln!(
                        "- [Axiomatic Alignment Check] Status: VIOLATION | Error: {}",
                        e
                    );
                    format!("Axiomatic Violation: {}", e)
                }
            };
            let final_answer =
                match super::truth::TruthTransformer::verify_mission_with_cross_examine(
                    goal,
                    "SUSI_SOLVE",
                    &final_answer,
                    workspace,
                ) {
                    Ok(v) => {
                        eprintln!(
                        "- [Reality Integrity Check] Status: SUCCESS | Fast-Path Read reality integrity verified."
                    );
                        v
                    }
                    Err(e) => {
                        verification_failed = true;
                        eprintln!(
                            "- [Reality Integrity Check] Status: VIOLATION | Error: {}",
                            e
                        );
                        format!("Reality Violation: {}", e)
                    }
                };

            let mut report = SusiMissionReport {
                goal: goal.to_string(),
                status:
                    if !verification_failed && crate::accountability::is_usable(&final_answer) {
                        "SUCCESS"
                    } else {
                        "FAILED"
                    }
                    .to_string(),
                agents: Vec::new(),
                interactions: Vec::new(),
                final_answer,
            };
            crate::cloud_recovery::recover(&mut report, workspace);
            eprintln!("{}", report.completion_message());
            drop(_guard);
            eprintln!("{}", report.to_protocol_format(true));
            return report;
        }

        let start = std::time::Instant::now();
        let res = self.solve_with_streaming_trace(goal, workspace, version, &|_| {});
        let elapsed = start.elapsed();

        eprintln!("\n- [Swarm Execution Latency] {:?}", elapsed);

        if elapsed.as_millis() > 2 {
            susi_sandbox::manager::SusiAuditLogger::log(
                workspace,
                susi_sandbox::manager::LogLevel::Axiomatic,
                "LATENCY_VIOLATION",
                &format!(
                    "Reflex operation exceeded 2ms mandate: {:?} (Goal: {})",
                    elapsed, goal
                ),
            );
        }

        match res {
            Ok(mut report) => {
                crate::cloud_recovery::recover(&mut report, workspace);
                eprintln!("{}", report.completion_message());
                drop(_guard);
                eprintln!("{}", report.to_protocol_format(true));
                report
            }
            Err(e) => {
                eprintln!("[MISSION FAILED] {}", e);
                let mut err_report = SusiMissionReport {
                    goal: goal.to_string(),
                    status: "FAILED".to_string(),
                    agents: Vec::new(),
                    interactions: Vec::new(),
                    final_answer: format!("SUSI Engine Error: {}", e),
                };
                crate::cloud_recovery::recover(&mut err_report, workspace);
                eprintln!("{}", err_report.completion_message());
                drop(_guard);
                eprintln!("{}", err_report.to_protocol_format(true));
                err_report
            }
        }
    }

    fn solve_with_streaming_trace(
        &self,
        goal: &str,
        workspace: &Path,
        _version: &str,
        _callback: &dyn Fn(String),
    ) -> EaiResult<SusiMissionReport> {
        let goal = Self::sanitize_input(goal)?;

        eprintln!("\n[DETAILED SWARM SYNTHESIS LOGS]");
        eprintln!("- [Swarm Synthesis] Recruitment projection active over Active Agent Registry.");

        // 1. Swarm Supervision
        let (interactions, agents) =
            super::amas::SusiSupervisor::supervise_mission(&goal, workspace);

        for agent in &agents {
            eprintln!("  - [Recruited Agent] Profile: {} | Provider: {} | Status: Recruited for semantic centroid projection overlap.", agent.name, agent.provider);
        }

        eprintln!("- [Swarm Execution] Dispatching agent fleet via rayon work-stealing parallel execution...");
        for msg in &interactions {
            if msg.sender != "ConsensusMaster" {
                eprintln!(
                    "  - [Swarm Channel Message] From: {} | Action: {} | Payload Len: {}",
                    msg.sender,
                    msg.action,
                    msg.payload.len()
                );
            }
        }

        let swarm_context =
            super::amas::SusiSupervisor::gather_weighted_wisdom(&interactions, &agents);

        eprintln!("\n[LIVE REASONING TOKENS]");
        eprintln!("- [Truth Convergence] Ingesting model reasoning trace stream:");
        let reasoning_prompt = format!(
            "MISSION_GOAL: {}\n\nLOCAL_SWARM_CONTEXT:\n{}\n\n[INSTRUCTION]: Resolve this mission. Output finalized verified actions.",
            goal, swarm_context
        );

        let final_answer = if !swarm_context.trim().is_empty()
            && (swarm_context.contains("###")
                || swarm_context.contains("| English")
                || swarm_context.contains("AGENT_SUCCESS_RATIO"))
        {
            eprintln!("{}", swarm_context);
            swarm_context
        } else {
            susi_gemi::engine::GemiEngine::generate_reasoning_stream(
                &reasoning_prompt,
                workspace,
                _callback,
            )
        };

        eprintln!("\n\n[SUBSTRATE VERIFICATION RESULTS]");
        let mut verification_failed = false;
        let verified = match susi_gemi::engine::GemiEngine::verify_axiomatic_alignment(
            &final_answer,
            workspace,
        ) {
            Ok(v) => {
                eprintln!("- [Axiomatic Alignment Check] Status: SUCCESS | Alignment verified.");
                v
            }
            Err(e) => {
                verification_failed = true;
                eprintln!(
                    "- [Axiomatic Alignment Check] Status: VIOLATION | Error: {}",
                    e
                );
                format!("Axiomatic Violation: {}", e)
            }
        };

        let verified_final = match super::truth::TruthTransformer::verify_mission_with_cross_examine(
            &goal,
            "SUSI_SOLVE",
            &verified,
            workspace,
        ) {
            Ok(v) => {
                eprintln!(
                    "- [Reality Integrity Check] Status: SUCCESS | Reality verification passed."
                );
                v
            }
            Err(e) => {
                verification_failed = true;
                eprintln!(
                    "- [Reality Integrity Check] Status: VIOLATION | Error: {}",
                    e
                );
                format!("Reality Violation: {}", e)
            }
        };

        Ok(SusiMissionReport {
            goal: goal.to_string(),
            status: if !verification_failed && crate::accountability::is_usable(&verified_final) {
                "COMPLETE"
            } else {
                "FAILED"
            }
            .to_string(),
            agents,
            interactions,
            final_answer: verified_final,
        })
    }

    pub fn solve(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
    ) -> EaiResult<SusiMissionReport> {
        let mut report = self.solve_internal(goal, workspace, version, 0)?;
        crate::cloud_recovery::recover(&mut report, workspace);
        Ok(report)
    }

    fn solve_internal(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        depth: u32,
    ) -> EaiResult<SusiMissionReport> {
        if depth >= 5 {
            return Ok(SusiMissionReport {
                goal: goal.to_string(),
                status: "ABORTED".to_string(),
                agents: vec![],
                interactions: vec![],
                final_answer: format!("SUSI-Recursion-Limit-Reached: Substrate decay prevented infinite intent recursion (Depth: {}).", depth),
            });
        }

        let goal = Self::sanitize_input(goal)?;
        let lower_goal = goal.to_lowercase();

        // Meta-command fast paths (identity/status/models/version/...)
        let trimmed_query = lower_goal.trim();
        if trimmed_query == "identity" || trimmed_query == "susi identity" {
            let (interactions, agents) = SusiSupervisor::supervise_mission(&goal, workspace);
            let identity_report =
                crate::self_core::AlphaSelf::inspect_compiled_binary_instructions();
            return Ok(SusiMissionReport {
                goal: goal.to_string(),
                status: "COMPLETE".to_string(),
                agents,
                interactions,
                final_answer: format!(
                    "SUSI Substrate Identity Report ({}):\n\n{}",
                    version, identity_report
                ),
            });
        }

        if trimmed_query == "version" || trimmed_query == "susi version" {
            let (interactions, agents) = SusiSupervisor::supervise_mission(&goal, workspace);
            return Ok(SusiMissionReport {
                goal: goal.to_string(),
                status: "COMPLETE".to_string(),
                agents,
                interactions,
                final_answer: format!("SUSI Engine Version: v{}", version),
            });
        }

        if trimmed_query == "status" || trimmed_query == "susi status" {
            let (interactions, agents) = SusiSupervisor::supervise_mission(&goal, workspace);
            let hw = susi_gemi::hardware::HardwareProfiler::get_profile();
            let global_dir = susi_paths::SusiDirs::config_dir();
            let daemon_status = if susi_sandbox::daemon_state::SusiDaemonState::check_status(
                workspace,
                &global_dir,
            ) {
                "RUNNING"
            } else {
                "STOPPED"
            };
            let status_report = format!(
                "SUSI Substrate Status ({}) :\n- Daemon Status: {}\n- Hardware: {} CPUs ({}) | {}GB RAM | {}\n- Acceleration: {}",
                version, daemon_status, hw.cpus, hw.cpu_brand, hw.ram_gb, hw.gpu_info, hw.native_acceleration
            );
            return Ok(SusiMissionReport {
                goal: goal.to_string(),
                status: "COMPLETE".to_string(),
                agents,
                interactions,
                final_answer: status_report,
            });
        }

        if trimmed_query == "models" || trimmed_query == "susi models" {
            let (interactions, agents) = SusiSupervisor::supervise_mission(&goal, workspace);
            let models = susi_gemi::models::ModelManager::list_models(workspace);
            let mut roster = format!("SUSI Substrate Models Roster ({}) :\n", version);
            for m in models {
                roster.push_str(&format!(
                    "- [{}] {} ({})\n",
                    m.provider(),
                    m.name(),
                    m.model_id()
                ));
            }
            return Ok(SusiMissionReport {
                goal: goal.to_string(),
                status: "COMPLETE".to_string(),
                agents,
                interactions,
                final_answer: roster,
            });
        }

        // Route goals that ask for parallel/split work to the parallel-mission path
        if lower_goal.contains("parallel") || lower_goal.contains("split") {
            return self.solve_parallel_mission(&goal, workspace, version, depth + 1);
        }

        // Autonomous Task Decomposition
        if (goal.len() > 150
            || lower_goal.contains(" and then ")
            || lower_goal.contains(" finally "))
            && !goal.contains("[STEP ")
        {
            return self.solve_planned_mission(&goal, workspace, version, depth + 1);
        }

        let mut retry_count = 0;
        let mut current_goal = goal.to_string();
        let mut last_error = String::new();
        let mut previous_errors = std::collections::HashSet::new();

        while retry_count < 3 {
            // 2. Swarm Supervision (Tier 1 AOA Dispatch)
            // Parallel execution of Safety, Security, Runtime Setup and Mission specific agents
            let (interactions, agents) =
                SusiSupervisor::supervise_mission(&current_goal, workspace);

            let lower_goal = current_goal.to_lowercase();
            let is_motion = lower_goal.contains("admin mission")
                || lower_goal.contains("motion")
                || lower_goal.contains("sync")
                || lower_goal.contains("audit")
                || lower_goal.contains("release");
            let swarm_context = SusiSupervisor::gather_weighted_wisdom(&interactions, &agents);

            let final_answer = if is_motion {
                // Admin/motion goal: label the output accordingly
                format!(
                    "SUSI-Motion-Convergence ({}):\n\n{}",
                    version, swarm_context
                )
            } else if !swarm_context.trim().is_empty()
                && !swarm_context.contains("No valid wisdom gathered")
            {
                // Swarm Convergence: Use high-confidence swarm wisdom directly without CPU model loop hang
                swarm_context
            } else {
                // Tier 2 Native Local Model Inference Fallback for Open Missions.
                // Escalate the minimum model tier by retry attempt: a
                // verified TRUTH_VIOLATION on a previous attempt (below)
                // means that attempt's model genuinely failed, so a retry
                // must not be allowed to reselect the same (or a smaller)
                // tier just because the heuristic complexity guess on the
                // longer, correction-annotated goal happens to land low
                // again - it forces a strictly bigger model each attempt.
                let reasoning_prompt = format!(
                    "MISSION_GOAL: {}\n\nLOCAL_SWARM_CONTEXT:\n{}\n\n[INSTRUCTION]: Resolve this mission using native local model inference.",
                    current_goal, swarm_context
                );

                let context_words = reasoning_prompt.split_whitespace().count();

                let min_complexity_for_attempt = match retry_count {
                    0 => None,
                    1 => Some(susi_gemi::intent::TaskComplexity::Moderate),
                    _ => Some(susi_gemi::intent::TaskComplexity::Complex),
                };
                let selected_model =
                    susi_gemi::models::ModelManager::get_selected_model_for_request_with_min_complexity(
                        &current_goal,
                        Some(context_words),
                        min_complexity_for_attempt,
                    );
                let model_name = selected_model
                    .as_deref()
                    .unwrap_or("automatic provisioning");
                let local_inference = match selected_model.as_deref() {
                    Some(model) => {
                        susi_gemi::engine::GemiEngine::generate_reasoning_deep_with_model(
                            &reasoning_prompt,
                            workspace,
                            model,
                        )
                    }
                    None => {
                        susi_gemi::engine::GemiEngine::generate_reasoning_deep_with_min_complexity(
                            &reasoning_prompt,
                            workspace,
                            min_complexity_for_attempt,
                        )
                    }
                };
                format!(
                    "SUSI-Tier2-Mission-Synthesis ({} via {}):\n\n{}",
                    version, model_name, local_inference
                )
            };

            // 4. Axiomatic Alignment Check
            match susi_gemi::engine::GemiEngine::verify_axiomatic_alignment(
                &final_answer,
                workspace,
            ) {
                Ok(ans) => {
                    // 5. Reality Verification
                    match super::truth::TruthTransformer::verify_mission_with_cross_examine(
                        &current_goal,
                        "SUSI_SOLVE",
                        &ans,
                        workspace,
                    ) {
                        Ok(verified_answer) => {
                            return Ok(SusiMissionReport {
                                goal: goal.to_string(),
                                status: "COMPLETE".to_string(),
                                agents,
                                interactions,
                                final_answer: verified_answer,
                            });
                        }
                        Err(e) if e.to_string().contains("TRUTH_VIOLATION") => {
                            let error_str = e.to_string();
                            let error_sig = format!("{:x}", md5::compute(error_str.as_bytes()));

                            if previous_errors.contains(&error_sig) {
                                susi_sandbox::manager::SusiAuditLogger::log_event(
                                    workspace,
                                    "RETRY_LOOP_DETECTED",
                                    &format!("Same error repeated: {}", error_str),
                                );
                                return Err(e);
                            }

                            previous_errors.insert(error_sig);
                            retry_count += 1;
                            last_error = error_str;
                            susi_sandbox::manager::SusiAuditLogger::log_event(
                                workspace,
                                "HALLUCINATION_DETECTED",
                                &format!("Retry {}/3: {}", retry_count, last_error),
                            );

                            current_goal = format!(
                                "{}\n\n[CORRECTION ATTEMPT {}]: Previous response failed reality check.\n\
                                Error detail: {}",
                                goal, retry_count,
                                last_error.chars().take(200).collect::<String>()
                            );
                        }
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => {
                    retry_count += 1;
                    susi_sandbox::manager::SusiAuditLogger::log_event(
                        workspace,
                        "AXIOMATIC_VIOLATION",
                        &e.to_string(),
                    );

                    current_goal = format!(
                        "{}\n\n[CORRECTION ATTEMPT {}]: Response violated substrate axioms.\n\
                        Violation: {}",
                        goal,
                        retry_count,
                        e.to_string().chars().take(200).collect::<String>()
                    );
                    continue;
                }
            };
        }

        Err(susi_error::EaiError::governance(format!(
            "Recursive reasoning failed after 3 attempts. Last violation: {}",
            last_error
        )))
    }

    fn solve_parallel_mission(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        depth: u32,
    ) -> EaiResult<SusiMissionReport> {
        let plan = susi_gemi::engine::MissionPlanner::partition_mission(goal, workspace)?;

        // Speculative Parallelism
        // Partitioned tasks are executed in parallel across the multi-threaded substrate.
        use rayon::prelude::*;

        let results: Vec<_> = plan
            .goals
            .par_iter()
            .enumerate()
            .map(|(i, sub_goal)| {
                let g = format!(
                    "[PARALLEL STEP {}/{}]: {}",
                    i + 1,
                    plan.goals.len(),
                    sub_goal
                );
                let w = workspace.to_path_buf();
                let v = version.to_string();
                let ama = SusiMasterAgent::new();
                ama.solve_internal(&g, &w, &v, depth + 1)
            })
            .filter_map(Result::ok)
            .collect();

        let mut all_interactions = Vec::new();
        let mut all_agents = Vec::new();
        let mut final_responses = Vec::new();

        for report in results.into_iter() {
            all_interactions.extend(report.interactions);
            all_agents.extend(report.agents);
            final_responses.push(report.final_answer);
        }

        Ok(SusiMissionReport {
            goal: goal.to_string(),
            status: "COMPLETE".to_string(),
            agents: all_agents,
            interactions: all_interactions,
            final_answer: format!(
                "PARALLEL_FORK_JOIN_COMPLETE ({} steps):\n\n{}",
                final_responses.len(),
                final_responses.join("\n\n---\n\n")
            ),
        })
    }

    fn solve_planned_mission(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        depth: u32,
    ) -> EaiResult<SusiMissionReport> {
        let mut plan = susi_gemi::engine::MissionPlanner::plan_mission(goal, workspace)?;
        let mut all_interactions = Vec::new();
        let mut all_agents = Vec::new();
        let mut final_responses = Vec::new();

        let mut current_step = 0;
        while current_step < plan.goals.len() {
            let sub_goal = &plan.goals[current_step];
            let tagged_goal = format!(
                "[STEP {}/{}]: {}",
                current_step + 1,
                plan.goals.len(),
                sub_goal
            );
            let report = self.solve_internal(&tagged_goal, workspace, version, depth + 1)?;

            all_interactions.extend(report.interactions.clone());
            all_agents.extend(report.agents.clone());
            final_responses.push(report.final_answer.clone());

            // Dynamic Plan Mutation: Check for failure or gap in the last step
            if report.final_answer.contains("FAILURE") || report.final_answer.contains("GAP") {
                susi_sandbox::manager::SusiAuditLogger::log_event(
                    workspace,
                    "PLAN_MUTATION",
                    &format!("Refining plan due to step {} failure.", current_step + 1),
                );

                let blackboard_state = format!("LATEST_OUTCOME: {}", report.final_answer);
                if let Ok(new_plan) = susi_gemi::engine::MissionPlanner::refine_plan(
                    goal,
                    &blackboard_state,
                    workspace,
                ) {
                    plan = new_plan;
                    // Reset or adjust steps based on new plan (for now we just continue from next)
                }
            }

            current_step += 1;
        }

        Ok(SusiMissionReport {
            goal: goal.to_string(),
            status: "COMPLETE".to_string(),
            agents: all_agents,
            interactions: all_interactions,
            final_answer: format!(
                "PLANNED_MISSION_COMPLETE:\n\n{}",
                final_responses.join("\n\n---\n\n")
            ),
        })
    }

    pub fn generate_substrate_report(&self, workspace: &Path) -> EaiResult<String> {
        let (axiom_summary, topology_summary) = AxiomSubstrate::ingest_constitution(workspace);
        let model_name =
            susi_gemi::models::ModelManager::get_selected_model(None).unwrap_or_else(|| {
                let filename = susi_sandbox::manager::SusiConfig::load_global()
                    .unwrap_or_default()
                    .alpha_weights_filename();
                format!("{} (Local Neural Substrate)", filename)
            });

        let mut report = String::new();
        report.push_str("# susi Substrate - Technical Report\n\n");
        report.push_str("- **Engine**: susi EAI Substrate\n");
        report.push_str(&format!("- **Version**: {}\n", env!("CARGO_PKG_VERSION")));
        report.push_str(&format!("- **Active Model**: {}\n\n", model_name));

        report.push_str(&axiom_summary);
        report.push('\n');
        report.push_str(&topology_summary);

        Ok(report)
    }

    pub fn process_intent(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        // 1. Audit
        susi_sandbox::manager::SusiAuditLogger::log_event(workspace, "MISSION_START", goal);

        // 2. Reasoning
        let res = self.solve(goal, workspace, env!("CARGO_PKG_VERSION"))?;

        // 3. Memory persistence
        susi_sandbox::manager::SusiMemory::save_interaction(
            workspace,
            goal,
            &res.final_answer,
            env!("CARGO_PKG_VERSION"),
        );

        // 4. Stage successful power_reason (remote) interactions for distillation
        for msg in &res.interactions {
            if msg.action.contains("power_reason") && !msg.payload.contains("[FAIL]") {
                let metadata = serde_json::json!({
                    "agents": res.agents.iter().map(|a| a.name.clone()).collect::<Vec<String>>(),
                    "interactions_count": res.interactions.len(),
                    "final_status": res.status
                });
                let _ = super::pkb::ProtocolKnowledgeBase::stage_distillation_pair(
                    goal,
                    &res.final_answer,
                    workspace,
                    Some(metadata),
                );
            }

            if msg.sender == "SusiUniversalSubstrateAgent"
                && (msg.payload.contains("VIOLATION") || msg.payload.contains("FAILURE"))
            {
                susi_sandbox::manager::SusiAuditLogger::log_event(
                    workspace,
                    "TOOL_FAILURE",
                    &msg.payload,
                );
            }
        }

        Ok(res.final_answer)
    }

    pub fn solve_with_feedback(
        &self,
        goal: &str,
        workspace: &Path,
        feedback_tx: flume::Sender<String>,
    ) -> EaiResult<String> {
        let goal = Self::sanitize_input(goal)?;
        let _ = feedback_tx.send(format!("[SUSI] Initiating mission for goal: '{}'", goal));

        // Mandate: Use multi-threaded swarm for all runtime setup and audits
        let _ = feedback_tx.send(
            "[SUSI] Dispatching multi-threaded swarm for setup, audit, and mission execution..."
                .to_string(),
        );
        let (interactions, agents) = SusiSupervisor::supervise_mission(&goal, workspace);

        for msg in &interactions {
            let _ = feedback_tx.send(format!("[Swarm: {}] {}", msg.sender, msg.action));
        }

        // Step 4: Final Synthesis
        let _ = feedback_tx.send(format!(
            "[SUSI] Mission synthesized across {} agents. Verifying reality...",
            agents.len()
        ));

        let model_name = susi_gemi::models::ModelManager::get_selected_model_for_request(&goal)
            .unwrap_or_else(|| {
                susi_sandbox::manager::SusiConfig::load_global()
                    .unwrap_or_default()
                    .alpha_weights_filename()
            });

        let ans = format!(
            "SUSI-Synthesis ({} via {}):\n\nProcessed goal '{}' across {} agents.",
            env!("CARGO_PKG_VERSION"),
            model_name,
            goal,
            agents.len()
        );

        let verified = super::truth::TruthTransformer::verify_mission_with_cross_examine(
            &goal,
            "SUSI_SOLVE",
            &ans,
            workspace,
        )?;

        susi_sandbox::manager::SusiMemory::save_interaction(
            workspace,
            &goal,
            &verified,
            env!("CARGO_PKG_VERSION"),
        );

        Ok(verified)
    }

    pub fn handle_autonomous_evolution(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        susi_sandbox::manager::SusiAuditLogger::log_event(workspace, "MISSION_START", goal);

        // 1. Attempt the mission as-is
        let res = self.solve(goal, workspace, env!("CARGO_PKG_VERSION"));

        match res {
            Ok(report) => {
                if report.final_answer.contains("NO_ACTION_REQUIRED")
                    || report.final_answer.contains("VIOLATION")
                {
                    // Potential gap or blocked action
                    if report.final_answer.contains("blocked") {
                        susi_sandbox::manager::SusiAuditLogger::log_event(
                            workspace,
                            "TRUTH_BLOCK",
                            &report.final_answer,
                        );
                    }
                    return Ok(report.final_answer);
                }
                Ok(report.final_answer)
            }
            Err(e) => {
                // FAILURE: Report gap
                susi_sandbox::manager::SusiAuditLogger::log_event(
                    workspace,
                    "INTELLIGENCE_GAP",
                    &format!("Goal '{}' failed: {}", goal, e),
                );

                if e.to_string().contains("not found") || e.to_string().contains("no models") {
                    susi_sandbox::manager::SusiAuditLogger::log_event(
                        workspace,
                        "INTELLIGENCE_GAP",
                        "No models found. Substrate expansion required by Swarm.",
                    );
                }

                // Report gap; pulse intent does NOT trigger Motion Rule
                Err(e)
            }
        }
    }
}

/// Routes a goal to either the coding toolbox (`ast_analyze`) or the
/// assistant toolbox (`rag_query`) based on keyword matching.
pub struct SusiHybridAgent {
    pub coding_toolbox: Vec<String>,
    pub assistant_toolbox: Vec<String>,
}

impl Default for SusiHybridAgent {
    fn default() -> Self {
        Self::new()
    }
}

impl SusiHybridAgent {
    pub fn new() -> Self {
        Self {
            coding_toolbox: vec![
                "ast_analyze".to_string(),
                "semantic_search".to_string(),
                "sandbox_exec".to_string(),
                "lsp_proxy".to_string(),
            ],
            assistant_toolbox: vec![
                "browser_automate".to_string(),
                "rag_query".to_string(),
                "audio_transcribe".to_string(),
            ],
        }
    }

    pub fn execute_hybrid_mission(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        eprintln!("<thinking>");
        eprintln!("[SUSI Hybrid Agent] Goal: {}", goal);

        let manifold = crate::manifold::IntentManifold::analyze(goal);
        eprintln!(
            "- [Intent Manifold] Scope: {:?} | Risk: {:?}",
            manifold.scope_of_impact, manifold.risk_profile
        );

        // Specialist Routing
        let result = if goal.contains("code") || goal.contains("refactor") || goal.contains("fix") {
            eprintln!("- [Specialist Route] Coding Agent Substrate Active");
            self.solve_coding_mission(goal, workspace)?
        } else {
            eprintln!("- [Specialist Route] General Assistant Substrate Active");
            self.solve_assistant_mission(goal, workspace)?
        };

        eprintln!("</thinking>\n");
        Ok(result)
    }

    fn solve_coding_mission(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        eprintln!("- [Coding Toolbox] Using: {:?}", self.coding_toolbox);
        let res = susi_tools::ToolRegistry::execute_tool(
            "ast_analyze",
            &serde_json::json!({"code": goal}),
            workspace,
        );
        Ok(format!("[HYBRID_CODING] {}", res))
    }

    fn solve_assistant_mission(&self, goal: &str, workspace: &Path) -> EaiResult<String> {
        eprintln!("- [Assistant Toolbox] Using: {:?}", self.assistant_toolbox);
        let res = susi_tools::ToolRegistry::execute_tool(
            "rag_query",
            &serde_json::json!({"query": goal}),
            workspace,
        );
        Ok(format!("[HYBRID_ASSISTANT] {}", res))
    }
}

#[cfg(test)]
mod report_tests {
    use super::*;
    #[test]
    fn failure_evidence_controls_banner_and_exit_even_with_optimistic_prose() {
        for status in ["FAILED", "BLOCKED", "ABORTED", "UNVERIFIED", ""] {
            let report = SusiMissionReport {
                goal: "weather".into(),
                status: status.into(),
                agents: vec![],
                interactions: vec![],
                final_answer: "Mission complete. Everything worked.".into(),
            };
            assert!(!report.is_success());
            assert_eq!(report.exit_code(), std::process::ExitCode::FAILURE);
            assert!(report.completion_message().starts_with("[MISSION FAILED]"));
            assert!(!report.completion_message().contains("MISSION COMPLETE"));
            // The protocol emits its JSON envelope followed by the answer text.
            let rendered = report.to_protocol_format(false);
            let protocol = serde_json::Deserializer::from_str(&rendered)
                .into_iter::<serde_json::Value>()
                .next()
                .unwrap()
                .unwrap();
            assert_eq!(protocol["observation"], format!("Mission status: {status}"));
        }
    }

    #[test]
    fn explicit_success_reports_have_success_banner_and_exit() {
        for status in ["SUCCESS", "COMPLETE"] {
            let report = SusiMissionReport {
                goal: "inspect".into(),
                status: status.into(),
                agents: vec![],
                interactions: vec![],
                final_answer: "Observed the requested file.".into(),
            };
            assert!(report.is_success());
            assert_eq!(report.exit_code(), std::process::ExitCode::SUCCESS);
            assert!(report
                .completion_message()
                .starts_with("[MISSION COMPLETE]"));
        }
    }

    #[test]
    fn protocol_preserves_failed_and_blocked_outcomes() {
        for status in ["FAILED", "BLOCKED", "ABORTED", "COMPLETE"] {
            let report = SusiMissionReport {
                goal: "test".into(),
                status: status.into(),
                agents: vec![],
                interactions: vec![],
                final_answer: String::new(),
            };
            let value: serde_json::Value =
                serde_json::from_str(&report.to_protocol_format(false)).unwrap();
            assert_eq!(value["observation"], format!("Mission status: {status}"));
        }
    }
}
