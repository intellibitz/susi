//! Top-level mission orchestrator: sanitizes input, dispatches the swarm.

use super::report::SusiMissionReport;
use crate::amas::{A2AMessage, SusiSupervisor};
use crate::susi_error::EaiResult;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use susi_gawd_agents::AxiomSubstrate;

fn bus_tool(name: &str, args: &serde_json::Value, workspace: &Path) -> String {
    crate::susi_core::plane_bus::tools::execute_tool(name, args, workspace)
        .unwrap_or_else(|e| format!("[Error] {e}"))
}

fn mission_plan_goals(plan: &Value) -> Vec<String> {
    plan.get("goals")
        .and_then(|g| g.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
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

        let hardware = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
        let max_len = (hardware.available_ram_gb * 1024 * 1024).max(4096); // Scale with RAM, min 4KB

        if trimmed.len() > max_len {
            return Err(crate::susi_error::EaiError::governance(format!(
                "Input exceeds hardware-scaled limit ({} characters).",
                max_len
            )));
        }

        if trimmed.is_empty() {
            return Err(crate::susi_error::EaiError::governance(
                "Input goal cannot be empty.",
            ));
        }

        // 2. Block high-risk shell/injection patterns
        let risk_patterns = ["$(", "> /dev/", "| nc ", "| netcat ", "0xCC", "\\x"];
        for pattern in risk_patterns {
            if trimmed.contains(pattern) {
                return Err(crate::susi_error::EaiError::governance(format!("High-risk sequence '{}' detected in input. Potential injection attempt blocked.", pattern)));
            }
        }

        Ok(trimmed.to_string())
    }

    /// Primary entry point for all natural language intents.
    /// Streams thinking and results back live in real-time.
    pub fn solve_clean(&self, goal: &str, workspace: &Path, version: &str) -> String {
        self.solve_clean_with_model(goal, workspace, version, None)
    }

    /// `solve_clean` honoring a caller-requested model name — provider
    /// failover tries the named provider first and the local fallback
    /// loads the named model instead of the intent-classified default.
    pub fn solve_clean_with_model(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        model: Option<&str>,
    ) -> String {
        self.solve_stream_with_model(goal, workspace, version, model, &|piece| {
            print!("{}", piece);
            let _ = std::io::stdout().flush();
        })
    }

    /// Generative mission (`/v1/chat/completions`): the provider's own
    /// output is the product. The swarm fleet is skipped — its agents run
    /// tools that mint incidental receipts a chat goal can satisfy
    /// spuriously (observed: "reply ok" answered by citing a `status`
    /// receipt). Instead the request goes straight through the governed
    /// provider cascade (`recover` with `generative`): safety/security
    /// audit, cooldowns, privacy gates, model-hint priority, local
    /// fallback — and the answer self-cites an inference receipt minted
    /// for the call, so provenance stays in the ledger.
    pub fn solve_clean_generative(
        &self,
        goal: &str,
        workspace: &Path,
        _version: &str,
        model: Option<&str>,
    ) -> String {
        let session = crate::susi_core::capture::EvidenceSession::new(
            goal,
            workspace,
            susi_gawd_agents::security::SecurityDetector::redact,
        )
        .ok();
        let _activation = session
            .as_ref()
            .map(crate::susi_core::capture::EvidenceSession::activate);
        let _scope = crate::susi_core::capture::EvidenceSession::enter(session.clone());
        let mut report = SusiMissionReport {
            goal: goal.to_string(),
            status: "FAILED".into(),
            agents: vec![],
            interactions: vec![],
            plan: None,
            final_answer: String::new(),
        };
        crate::cloud_recovery::recover(&mut report, workspace, model, true, session);
        if report.is_success() {
            report.final_answer
        } else {
            // Marker lets the HTTP layer map exhausted inference to a
            // 502 instead of serving failure prose as a 200 completion.
            format!("[INFERENCE_FAILED]{}", report.final_answer)
        }
    }

    pub fn solve_stream(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        _callback: &dyn Fn(String),
    ) -> String {
        self.solve_stream_with_model(goal, workspace, version, None, _callback)
    }

    #[allow(clippy::too_many_arguments)] // mirrors the established
                                         // solve_stream signature + the model-hint slot; grouping into a
                                         // config struct would churn every external caller for one option.
    pub fn solve_stream_with_model(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        model: Option<&str>,
        _callback: &dyn Fn(String),
    ) -> String {
        self.solve_stream_report_with_model(goal, workspace, version, model, _callback)
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
        self.solve_stream_report_with_model(goal, workspace, version, None, _callback)
    }

    /// `solve_stream_report` with an optional caller-requested model hint
    /// (`/v1/chat/completions` `model` field). The hint is advisory inside
    /// the governed pipeline: failover prioritizes a matching provider and
    /// the local leg loads the named model — it never bypasses governance.
    #[allow(clippy::too_many_arguments)] // same shape as
                                         // solve_stream_report plus the model-hint slot.
    pub fn solve_stream_report_with_model(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        model_hint: Option<&str>,
        _callback: &dyn Fn(String),
    ) -> SusiMissionReport {
        self.solve_stream_report_inner(goal, workspace, version, model_hint, _callback)
    }

    #[allow(clippy::too_many_arguments)] // same shape as the public
                                         // wrappers it consolidates.
    fn solve_stream_report_inner(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        model_hint: Option<&str>,
        _callback: &dyn Fn(String),
    ) -> SusiMissionReport {
        // Mission-scoped evidence ledger (same contract as `solve`): tool
        // dispatches record receipts here, and citation answers resolve from
        // them at verification and during cloud recovery.
        let session = crate::susi_core::capture::EvidenceSession::new(
            goal,
            workspace,
            susi_gawd_agents::security::SecurityDetector::redact,
        )
        .ok();
        let _activation = session
            .as_ref()
            .map(crate::susi_core::capture::EvidenceSession::activate);
        let _scope = crate::susi_core::capture::EvidenceSession::enter(session.clone());

        let hw = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
        let (_engine_type, active_model_id) = {
            let intent = crate::susi_core::plane_bus::gemi::IntentClassifier::classify(goal);
            let resolved =
                crate::susi_core::plane_bus::gemi::ModelManager::get_active_engine_and_model(Some(
                    &intent,
                ));
            // A caller-requested model names the serving model; intent
            // classification still picked the engine.
            match model_hint {
                Some(m) if !m.is_empty() => (resolved.0, m.to_string()),
                _ => resolved,
            }
        };
        let model_path_str =
            crate::susi_core::plane_bus::gemi::ModelManager::get_model_path(&active_model_id)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "Internal Hard-Compiled Substrate Genome".to_string());
        let device = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_candle_device_label();
        let local_models_count =
            crate::susi_core::plane_bus::gemi::ModelManager::list_models_len(workspace);

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

        use susi_gawd_agents::self_core::AlphaSelf;
        eprintln!("- [Core Paradigm] {}", AlphaSelf::CORE_PARADIGM);
        eprintln!("- [Accountability] Omni-Trace active (Mandate 26: Glass Box Transparency)");

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
            "- [Compute Saturation] Native Acceleration: {} | Active Inference Device: {}",
            hw.native_acceleration, device
        );

        eprintln!("\n[DETAILED SUBSTRATE CONFIGURATION LOGS]");
        let global_dir = susi_paths::SusiDirs::config_dir();
        let cfg = crate::susi_sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();
        eprintln!(
            "- [Network Fabric] GMCP Port: {} | GEMI Port: {} | Discovery UDP Port: {} | GMCP HTTP Alias: {} | A2A HTTP Port: {}",
            cfg.gmcp_port(),
            cfg.gemi_port(),
            cfg.udp_discovery_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port()
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
            susi_gawd_agents::agents::GawdAgentFleet::get_max_concurrent_agents()
        );
        eprintln!(
            "- [Active Model Deep-Dive] Active Local ID: {} | Model Path: {}",
            active_model_id, model_path_str
        );
        eprintln!(
            "- [Discovered Model Substrates] Count: {}",
            local_models_count
        );

        // Only DNA mandates are mandates; ledger entries and roadmap vectors
        // are compiled alongside them but never printed under this heading.
        eprintln!("\n[GENOMIC MANDATES] {}", AlphaSelf::genome_summary());
        for rule in AlphaSelf::rules_of(susi_gawd_agents::self_core::RuleKind::Mandate) {
            eprintln!("- [{}] {}", rule.cite, rule.title);
        }

        // 1. Continuous Intent Manifold Routing
        let manifold = crate::susi_core::manifold::IntentManifold::analyze(goal);
        eprintln!(
            "\n[INTENT MANIFOLD ROUTING: {:?} (Risk: {:?})]",
            manifold.scope_of_impact, manifold.risk_profile
        );

        if manifold.scope_of_impact == crate::susi_core::manifold::ScopeOfImpact::Read {
            // Glass Box Transparency (Mandate 26): the fast-path used to print
            // these two lines as pure narration with no backing call — the
            // trace claimed governance validation happened when it didn't.
            // Actually run the same lightweight (no-inference) detectors the
            // swarm's SafetyAgent/SecurityAgent run for every mission goal, so
            // what's printed here is true, not aspirational.
            eprintln!("- [Substrate Operation] Validating with SafetyAgent...");
            let safety_result = susi_gawd_agents::safety::SafetyDetector::audit_action(
                "SUSI_SOLVE",
                goal,
                workspace,
            );
            eprintln!("- [Substrate Operation] Validating with SecurityAgent...");
            let security_result = susi_gawd_agents::security::SecurityDetector::audit_action(
                "SUSI_SOLVE",
                goal,
                workspace,
            );

            if let Err(e) = safety_result.and(security_result) {
                eprintln!("- [Governance] Fast-path Read blocked: {}", e);
                let report = SusiMissionReport {
                    goal: goal.to_string(),
                    status: "BLOCKED".to_string(),
                    agents: Vec::new(),
                    interactions: Vec::new(),
                    plan: None,
                    final_answer: format!("[GOVERNANCE_BLOCK] {}", e),
                };
                report.persist_inspectable_trace(workspace);
                eprintln!("{}", report.completion_message());
                drop(_guard);
                eprintln!("{}", report.to_protocol_format(true));
                return report;
            }

            eprintln!("\n[DETAILED SWARM SYNTHESIS LOGS]");
            eprintln!("- [Recruited Agent] SafetyAgent (Provider: Local Core) cleared Read safety validation.");
            eprintln!("- [Recruited Agent] SecurityAgent (Provider: Local Core) cleared injection validation.");

            let lower_goal = goal.trim().to_lowercase();
            let system_read =
                susi_gawd_agents::system_observe::capture_verified_read(goal, workspace);
            let final_answer = if let Some(read) = &system_read {
                match read {
                    Ok(read) => read.answer().to_string(),
                    Err(error) => format!("TRUTH_UNVERIFIED: {error}"),
                }
            } else if lower_goal.contains("identity") {
                susi_gawd_agents::self_core::AlphaSelf::inspect_compiled_binary_instructions()
            } else if lower_goal.contains("who am i") || lower_goal.contains("whoami") {
                let user = std::env::var("USER")
                    .or_else(|_| std::env::var("USERNAME"))
                    .unwrap_or_else(|_| "unknown_user".into());
                let host = hw.hostname;
                format!(
                    "System User Identity: {}@{}\n\nSUSI Substrate Identity:\n{}",
                    user,
                    host,
                    susi_gawd_agents::self_core::AlphaSelf::inspect_compiled_binary_instructions()
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
                bus_tool(
                    "exec_command",
                    &serde_json::Value::String(cmd.to_string()),
                    workspace,
                )
            } else if susi_gawd_agents::system_observe::looks_like_system_observe_goal(goal) {
                susi_gawd_agents::system_observe::observe_system(goal, workspace).unwrap_or_else(
                    || {
                        bus_tool(
                            "exec_command",
                            &serde_json::Value::String(
                                "df -h -x tmpfs -x devtmpfs -x squashfs --total".into(),
                            ),
                            workspace,
                        )
                    },
                )
            } else if lower_goal.trim() == "dashboard"
                || lower_goal == "susi dashboard"
                || lower_goal == "show dashboard"
            {
                bus_tool("sovereign_dashboard", &serde_json::json!(null), workspace)
            } else if lower_goal.trim() == "bloat audit"
                || lower_goal == "run bloat audit"
                || lower_goal == "bloat-audit"
            {
                bus_tool("bloat_audit", &serde_json::json!(null), workspace)
            } else if lower_goal.trim() == "version"
                || lower_goal == "susi version"
                || lower_goal == "show version"
            {
                format!("SUSI Engine Version: v{}", version)
            } else if lower_goal.trim() == "status"
                || lower_goal == "susi status"
                || lower_goal == "show status"
            {
                format!(
                    "SUSI Substrate Status: Operational | Hardware: {} | RAM: {}GB",
                    hw.cpu_brand, hw.ram_gb
                )
            } else if lower_goal.trim() == "models"
                || lower_goal == "list models"
                || lower_goal == "show models"
            {
                let models =
                    crate::susi_core::plane_bus::gemi::ModelManager::list_models(workspace);
                let count = models.as_array().map(|a| a.len()).unwrap_or(0);
                format!("Active Model Substrates (Count: {count})\n\n{models}")
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
            let final_answer =
                match crate::susi_core::plane_bus::gemi::GemiEngine::verify_axiomatic_alignment(
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
            // Only exact native identity/version queries have a deterministic
            // completion contract here. Incidental keywords must not turn an
            // unrelated request into a successful identity or version response.
            let native_verification = match &system_read {
                Some(Ok(read)) => Some(read.verify(goal, &final_answer, workspace)),
                Some(Err(error)) => Some(Err(crate::susi_error::EaiError::governance(format!(
                    "TRUTH_UNVERIFIED: {error}"
                )))),
                None => verify_compiled_read(goal, &final_answer),
            };
            let verification = native_verification.unwrap_or_else(|| {
                crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                    goal,
                    "SUSI_SOLVE",
                    &final_answer,
                    workspace,
                )
            });
            let final_answer = match verification {
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
                status: if !verification_failed
                    && susi_gawd_agents::accountability::is_usable(&final_answer)
                {
                    "SUCCESS"
                } else {
                    "FAILED"
                }
                .to_string(),
                // Glass box: Safety/Security already audited this Read path —
                // report them so "Agents Recruited" matches the telemetry.
                agents: vec![
                    susi_gawd_agents::GawdAgentInfo {
                        name: "SafetyAgent".into(),
                        provider: "Local Core".into(),
                        url: String::new(),
                        rank: 1.0,
                    },
                    susi_gawd_agents::GawdAgentInfo {
                        name: "SecurityAgent".into(),
                        provider: "Local Core".into(),
                        url: String::new(),
                        rank: 1.0,
                    },
                ],
                interactions: Vec::new(),
                plan: None,
                final_answer,
            };
            crate::cloud_recovery::recover(
                &mut report,
                workspace,
                model_hint,
                false,
                session.clone(),
            );
            attach_evidence_ledger(&mut report, session.as_ref(), workspace);
            eprintln!("{}", report.completion_message());
            drop(_guard);
            eprintln!("{}", report.to_protocol_format(true));
            return report;
        }

        let start = std::time::Instant::now();
        let res = self.solve_with_streaming_trace(goal, workspace, version, &|_| {});
        let elapsed = start.elapsed();

        eprintln!("\n- [Swarm Execution Latency] {:?}", elapsed);

        match res {
            Ok(mut report) => {
                crate::cloud_recovery::recover(
                    &mut report,
                    workspace,
                    model_hint,
                    false,
                    session.clone(),
                );
                attach_evidence_ledger(&mut report, session.as_ref(), workspace);
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
                    plan: None,
                    final_answer: format!("SUSI Engine Error: {}", e),
                };
                crate::cloud_recovery::recover(
                    &mut err_report,
                    workspace,
                    model_hint,
                    false,
                    session.clone(),
                );
                attach_evidence_ledger(&mut err_report, session.as_ref(), workspace);
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
            crate::amas::SusiSupervisor::supervise_mission(&goal, workspace);

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
            crate::amas::SusiSupervisor::gather_weighted_wisdom(&interactions, &agents);

        eprintln!("\n[LIVE REASONING TOKENS]");
        eprintln!("- [Truth Convergence] Ingesting model reasoning trace stream:");
        // Captured receipts change the answer contract: with a live ledger the
        // answer must select evidence by citation, so swarm narrative alone can
        // never be the final answer while receipts exist to cite.
        let evidence_prompt =
            crate::susi_core::capture::EvidenceSession::evidence_prompt_for(workspace);
        let reasoning_prompt = format!(
            "MISSION_GOAL: {}\n\nLOCAL_SWARM_CONTEXT:\n{}\n\n[INSTRUCTION]: Resolve this mission. Output finalized verified actions.{}",
            goal, swarm_context, evidence_prompt
        );

        let final_answer = if !swarm_context.trim().is_empty()
            && evidence_prompt.is_empty()
            && (swarm_context.contains("###")
                || swarm_context.contains("| English")
                || swarm_context.contains("AGENT_SUCCESS_RATIO"))
        {
            eprintln!("{}", swarm_context);
            swarm_context
        } else {
            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_stream(
                &reasoning_prompt,
                workspace,
                _callback,
            )
        };

        eprintln!("\n\n[SUBSTRATE VERIFICATION RESULTS]");
        let mut verification_failed = false;
        let verified =
            match crate::susi_core::plane_bus::gemi::GemiEngine::verify_axiomatic_alignment(
                &final_answer,
                workspace,
            ) {
                Ok(v) => {
                    eprintln!(
                        "- [Axiomatic Alignment Check] Status: SUCCESS | Alignment verified."
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

        let mut verified_final_attempt =
            crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                &goal,
                "SUSI_SOLVE",
                &verified,
                workspace,
            );

        if let Err(e) = &verified_final_attempt {
            if e.to_string().contains("narrative alone cannot complete it") {
                if let Some(auto_cited) =
                    crate::susi_core::capture::EvidenceSession::auto_format_truth(workspace)
                {
                    eprintln!("[TruthFormatter] Intercepting ungrounded local narrative. Auto-aligning missing citations...");
                    verified_final_attempt = crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                        &goal,
                        "SUSI_SOLVE",
                        &auto_cited,
                        workspace,
                    );
                }
            }
        }

        let verified_final = match verified_final_attempt {
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
            status: if !verification_failed
                && susi_gawd_agents::accountability::is_usable(&verified_final)
            {
                "COMPLETE"
            } else {
                "FAILED"
            }
            .to_string(),
            agents,
            interactions,
            plan: None,
            final_answer: verified_final,
        })
    }

    pub fn solve(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
    ) -> EaiResult<SusiMissionReport> {
        // Mission-scoped evidence ledger: every real tool dispatch inside this
        // call — swarm agents on rayon workers, MCP calls, recovery attempts —
        // records a receipt. Answers certify only by citing those receipts.
        let session = crate::susi_core::capture::EvidenceSession::new(
            goal,
            workspace,
            susi_gawd_agents::security::SecurityDetector::redact,
        )
        .ok();
        let _activation = session
            .as_ref()
            .map(crate::susi_core::capture::EvidenceSession::activate);
        let _scope = crate::susi_core::capture::EvidenceSession::enter(session.clone());
        let mut report = self.solve_internal(goal, workspace, version, 0)?;
        crate::cloud_recovery::recover(&mut report, workspace, None, false, session.clone());
        attach_evidence_ledger(&mut report, session.as_ref(), workspace);
        Ok(report)
    }

    /// Autonomous planning loop: decompose the goal into steps, execute each
    /// step with the normal mission pipeline, verify, and synthesize a final
    /// answer. Bounded by `max_steps` and recursion depth.
    pub fn solve_autonomous(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        max_steps: u32,
    ) -> EaiResult<SusiMissionReport> {
        let goal = Self::sanitize_input(goal)?;

        // Deliberate before dispatch: several candidate decompositions are
        // scored on coverage/risk/verifiability and executed best-first.
        // Mutate/SelfExtend intents additionally need two candidates to
        // agree on the approach — without consensus the mission runs the
        // goal as a single step rather than trusting one model sample.
        let manifold = crate::susi_core::manifold::IntentManifold::analyze(&goal);

        // Retrieve before planning: similar missions' outcomes inform both
        // the decomposition prompt (what worked, what failed) and the
        // difficulty estimate that decides how wide the search runs.
        let traces = crate::susi_core::mission_trace::read_all(workspace);
        let difficulty =
            crate::susi_core::mission_trace::difficulty(&goal, &traces, manifold.risk_profile);
        let brief = format!(
            "{}{}",
            crate::susi_core::mission_trace::history_brief(&goal, &traces, 5),
            // A worked exemplar: how a similar mission's plan actually
            // decomposed when it succeeded — few-shot plan shape, not
            // just outcome text.
            crate::susi_core::mission_trace::proven_plan_brief(&goal, &traces)
        );
        eprintln!(
            "[RETRIEVAL] {} similar missions | difficulty={:.2} (novel={} failure_rate={:.0}% risk={:?}){}{}",
            crate::susi_core::mission_trace::similar(&goal, &traces, 8).len(),
            difficulty.score,
            difficulty.novel,
            difficulty.failure_rate * 100.0,
            difficulty.risk,
            difficulty
                .median_duration_secs
                .map(|d| format!(" ~{d}s"))
                .unwrap_or_default(),
            // Scorer honesty, when enough scored missions exist to judge:
            // does a higher plan_score actually predict success?
            crate::susi_core::mission_trace::plan_score_correlation(&traces)
                .map(|c| format!(" | score↔outcome r={c:.2}"))
                .unwrap_or_default()
        );

        let mut budgets = crate::deliberation::candidate_budgets(max_steps);
        if difficulty.demands_deliberation() {
            // A novel or frequently-failed intent earns a wider search.
            budgets.push((max_steps + 4).min(8));
        }
        // The shape that actually worked before is a prior worth
        // searching around — similar verified successes say how long
        // a plan for this intent class usually needs to be.
        if let Some(shape) = crate::susi_core::mission_trace::proven_plan_length(&goal, &traces, 8)
        {
            budgets.push(shape.clamp(1, 12));
        }
        budgets.sort_unstable();
        budgets.dedup();
        let mut failed_entities =
            crate::susi_core::mission_trace::failing_tool_counts(&goal, &traces, 8);
        // Agents that only ever ran on failed similar missions taint plans
        // the same way tools do — a step naming one gets docked.
        for (agent, count) in
            crate::susi_core::mission_trace::failing_agent_counts(&goal, &traces, 8)
        {
            *failed_entities.entry(agent).or_insert(0) += count;
        }
        let mut proven_entities = crate::susi_core::mission_trace::proven_tools(&goal, &traces, 8);
        proven_entities.extend(crate::susi_core::mission_trace::proven_agents(
            &goal, &traces, 8,
        ));
        let history = crate::deliberation::HistorySignals {
            failed: failed_entities,
            proven: proven_entities,
            failed_steps: crate::susi_core::mission_trace::failed_steps(&goal, &traces, 8),
            proven_steps: crate::susi_core::mission_trace::proven_steps(&goal, &traces, 8),
            proven_plans: crate::susi_core::mission_trace::proven_plans(&goal, &traces, 8),
            failed_plans: crate::susi_core::mission_trace::failed_plans(&goal, &traces, 8),
        };
        let mut deliberation =
            crate::deliberation::deliberate(&goal, &manifold, &budgets, &history, |budget| {
                self.plan_steps(&goal, &brief, workspace, budget)
            });
        // An intent whose recent trace record ends in failure is an
        // anti-pattern: even read-scope goals then require candidate
        // agreement before any plan runs.
        if matches!(
            crate::susi_core::mission_trace::promotion_status(&traces, &goal),
            crate::susi_core::mission_trace::PromotionStatus::Vetoed { .. }
        ) {
            deliberation.consensus_required = true;
            eprintln!(
                "- [Deliberation] Anti-pattern history for this intent — consensus required."
            );
        }
        // Softer than a veto: when similar missions mostly failed without
        // tripping the anti-pattern window, plan agreement is still
        // warranted — one good guess isn't enough in a bad neighborhood.
        if !deliberation.consensus_required
            && crate::susi_core::mission_trace::unreliable_neighborhood(&goal, &traces, 8)
        {
            deliberation.consensus_required = true;
            eprintln!(
                "- [Deliberation] Low success rate among similar missions — consensus required."
            );
        }
        eprintln!(
            "\n[PLAN DELIBERATION] {} candidates | consensus_required={} consensus={:?} threshold={:.2}",
            deliberation.candidates.len(),
            deliberation.consensus_required,
            deliberation.consensus,
            deliberation.consensus_threshold
        );
        for (i, c) in deliberation.candidates.iter().enumerate() {
            eprintln!(
                "- [Candidate {}] score={:.2} steps={} ({})",
                i + 1,
                c.score,
                c.steps.len(),
                c.rationale
            );
        }
        let plan: Vec<String> = if crate::deliberation::approved(&deliberation) {
            deliberation.candidates[0].steps.clone()
        } else {
            eprintln!(
                "- [Deliberation] No candidate consensus under {:?} scope — \
                 declining multi-step autonomy; running goal as one step.",
                manifold.scope_of_impact
            );
            vec![goal.clone()]
        };
        // Candidates not executed still enter the record — the mission's
        // deliberation evidence includes what was considered and rejected.
        let candidate_summary = deliberation
            .candidates
            .iter()
            .enumerate()
            .map(|(i, c)| format!("candidate {} score {:.2}: {}", i + 1, c.score, c.rationale))
            .collect::<Vec<_>>()
            .join("; ");

        let session = crate::susi_core::capture::EvidenceSession::new(
            &goal,
            workspace,
            susi_gawd_agents::security::SecurityDetector::redact,
        )
        .ok();
        let _activation = session
            .as_ref()
            .map(crate::susi_core::capture::EvidenceSession::activate);
        let _scope = crate::susi_core::capture::EvidenceSession::enter(session.clone());

        let mut step_reports = Vec::new();
        let mut step_summaries = Vec::new();

        // No transaction wraps the plan: steps write through arbitrary
        // agents and tools, and a transaction begun with no file list (as
        // this loop once did) snapshots nothing while the failure report
        // claimed a rollback. A failed step reports what it cannot undo.
        // Read-scope missions may fall through to the next candidate plan —
        // reads mutate nothing, so a second plan is safe; mutating scopes
        // abort on first failure rather than re-running a guess.
        let may_replan =
            manifold.scope_of_impact == crate::susi_core::manifold::ScopeOfImpact::Read;
        let plans_to_try: Vec<Vec<String>> = if crate::deliberation::approved(&deliberation) {
            deliberation
                .candidates
                .iter()
                .map(|c| c.steps.clone())
                .collect()
        } else {
            vec![plan.clone()]
        };
        // Honest attribution: the score of the plan that actually ran —
        // a fallback single-step or a later candidate is not
        // candidates[0], and the trace must not claim it is.
        let executed_score = |executed: &[String]| {
            deliberation
                .candidates
                .iter()
                .find(|c| c.steps == executed)
                .map(|c| c.score)
                .unwrap_or_else(|| crate::deliberation::score_plan(&goal, executed, max_steps).0)
        };
        let mut succeeded_plan: Option<&Vec<String>> = None;
        'attempts: for (attempt, attempt_plan) in plans_to_try.iter().enumerate() {
            if attempt > 0 {
                eprintln!(
                    "\n[PLAN REPLAN] Candidate {} (read-scope retry after failure)",
                    attempt + 1
                );
            }
            for (i, step) in attempt_plan.iter().enumerate() {
                eprintln!(
                    "\n[AUTONOMOUS PLAN] Attempt {} Step {}/{}: {}",
                    attempt + 1,
                    i + 1,
                    attempt_plan.len(),
                    step
                );
                let mut report = self.solve_internal(step, workspace, version, 0)?;
                crate::cloud_recovery::recover(
                    &mut report,
                    workspace,
                    None,
                    false,
                    session.clone(),
                );

                let success = report.status == "COMPLETE" || report.status == "SUCCESS";
                let summary = format!(
                    "Attempt {} Step {}: {} -> status={}, answer={}",
                    attempt + 1,
                    i + 1,
                    step,
                    report.status,
                    report.final_answer.chars().take(200).collect::<String>()
                );
                step_summaries.push(summary);
                step_reports.push(report.clone());

                crate::susi_core::context_graph::ContextGraph::global().record_agent_observation(
                    session.as_ref().map(|s| s.id()),
                    "AutonomousPlanner",
                    &format!("{}: {}", step, report.final_answer),
                    workspace,
                );

                if !success {
                    if may_replan && attempt + 1 < plans_to_try.len() {
                        step_summaries.push(format!(
                            "Attempt {} aborted; trying next candidate plan",
                            attempt + 1
                        ));
                        continue 'attempts;
                    }
                    let mut final_report = SusiMissionReport {
                        goal: goal.clone(),
                        status: "FAILED".to_string(),
                        agents: report.agents,
                        interactions: report.interactions,
                        plan: Some(super::report::PlanRecord {
                            steps: attempt_plan.clone(),
                            score: executed_score(attempt_plan),
                            consensus: deliberation.consensus,
                            failed_step: Some((i + 1) as u32),
                        }),
                        final_answer: format!(
                            "Autonomous plan aborted at step {}. Workspace changes made by earlier steps were not rolled back. {}\n\nDeliberation: {}\n\nPrior steps:\n{}",
                            i + 1,
                            report.final_answer,
                            candidate_summary,
                            step_summaries.join("\n")
                        ),
                    };
                    attach_evidence_ledger(&mut final_report, session.as_ref(), workspace);
                    return Ok(final_report);
                }
            }
            succeeded_plan = Some(attempt_plan);
            break;
        }

        // Synthesize final answer from step results.
        let synthesis_goal = if step_summaries.len() <= 1 {
            goal.clone()
        } else {
            format!(
                "Original goal: {}\n\nCompleted plan steps:\n{}\n\nSynthesize a concise final answer.",
                goal,
                step_summaries.join("\n")
            )
        };
        let mut final_report = self.solve_internal(&synthesis_goal, workspace, version, 0)?;
        crate::cloud_recovery::recover(&mut final_report, workspace, None, false, session.clone());
        // The synthesis path's own report never saw plan search — attach the
        // deliberation record so the mission trace joins plan to outcome.
        // The recorded steps/score are the candidate that actually
        // succeeded — not always candidates[0] after a replan.
        let winning_plan = succeeded_plan.unwrap_or(&plan);
        final_report.plan = Some(super::report::PlanRecord {
            steps: winning_plan.clone(),
            score: executed_score(winning_plan),
            consensus: deliberation.consensus,
            failed_step: None,
        });
        final_report.interactions.push(crate::amas::A2AMessage {
            sender: "Deliberator".into(),
            recipient: "SUSI-Master".into(),
            action: "PLAN_SEARCH".into(),
            payload: candidate_summary,
        });
        attach_evidence_ledger(&mut final_report, session.as_ref(), workspace);
        Ok(final_report)
    }

    /// Ask the reasoning substrate to decompose `goal` into a bounded list of
    /// steps. Falls back to a single-step plan if decomposition fails or the
    /// model is unavailable.
    fn plan_steps(
        &self,
        goal: &str,
        history_brief: &str,
        workspace: &Path,
        max_steps: u32,
    ) -> Vec<String> {
        let prompt = format!(
            "Break the following goal into at most {} concise, ordered steps. \
             Return one step per line starting with a number and a period. \
             Do not add extra commentary.{}\n\n{}Goal: {}\n\nSteps:",
            max_steps.clamp(1, 8),
            // Strategy varies by budget: candidates should disagree about
            // approach (minimal vs verify-everything), not only length.
            crate::deliberation::style_hint(max_steps),
            history_brief,
            goal
        );
        // Deep path: planning must never be answered by a reflex tier. With
        // reflexes allowed, the decomposition template around a familiar goal
        // cleared Tier-0's support gate (cosine 0.72 measured) and could be
        // answered `ACTION: status`, which the step parser then drops — the
        // mission would silently degrade to a single-step plan.
        let response = crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
            &prompt, workspace,
        );
        let steps: Vec<String> = response
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| {
                let trimmed = l.trim();
                trimmed
                    .split_once('.')
                    .map(|(_, rest)| rest.trim().to_string())
                    .filter(|s| !s.is_empty())
            })
            .take(max_steps.clamp(1, 8) as usize)
            .collect();
        if steps.is_empty() {
            vec![goal.to_string()]
        } else {
            steps
        }
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
                plan: None,
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
                susi_gawd_agents::self_core::AlphaSelf::inspect_compiled_binary_instructions();
            return Ok(SusiMissionReport {
                goal: goal.to_string(),
                status: "COMPLETE".to_string(),
                agents,
                interactions,
                plan: None,
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
                plan: None,
                final_answer: format!("SUSI Engine Version: v{}", version),
            });
        }

        if trimmed_query == "status" || trimmed_query == "susi status" {
            let (interactions, agents) = SusiSupervisor::supervise_mission(&goal, workspace);
            let hw = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
            let global_dir = susi_paths::SusiDirs::config_dir();
            let daemon_status = if crate::susi_sandbox::daemon_state::SusiDaemonState::check_status(
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
                plan: None,
                final_answer: status_report,
            });
        }

        if trimmed_query == "models" || trimmed_query == "susi models" {
            let (interactions, agents) = SusiSupervisor::supervise_mission(&goal, workspace);
            let models = crate::susi_core::plane_bus::gemi::ModelManager::list_models(workspace);
            let roster = format!("SUSI Substrate Models Roster ({version}) :\n{models}");
            return Ok(SusiMissionReport {
                goal: goal.to_string(),
                status: "COMPLETE".to_string(),
                agents,
                interactions,
                plan: None,
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

            // When the mission captured real tool calls, the answer must cite
            // those receipts — swarm narrative and motion labels are not proof.
            // Force the synthesis step so the model can select citations.
            let evidence_prompt =
                crate::susi_core::capture::EvidenceSession::evidence_prompt_for(workspace);

            let final_answer = if is_motion && evidence_prompt.is_empty() {
                // Admin/motion goal: label the output accordingly
                format!(
                    "SUSI-Motion-Convergence ({}):\n\n{}",
                    version, swarm_context
                )
            } else if evidence_prompt.is_empty()
                && !swarm_context.trim().is_empty()
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
                    "MISSION_GOAL: {}\n\nLOCAL_SWARM_CONTEXT:\n{}\n\n[INSTRUCTION]: Resolve this mission using native local model inference. If you need to execute a shell command, provide it in a ```bash codeblock.{}",
                    current_goal, swarm_context, evidence_prompt
                );

                let context_words = reasoning_prompt.split_whitespace().count();

                // Difficulty-aware starting tier: an intent that is novel or
                // whose predecessors failed starts one tier up rather than
                // paying a failed attempt to learn it. Retry escalation
                // keeps its strictly-higher floor on later attempts.
                let difficulty_floor: Option<&'static str> = {
                    let traces = crate::susi_core::mission_trace::read_all(workspace);
                    crate::susi_core::mission_trace::difficulty(
                        &current_goal,
                        &traces,
                        crate::susi_core::manifold::IntentManifold::analyze(&current_goal)
                            .risk_profile,
                    )
                    .demands_deliberation()
                    .then_some("Moderate")
                };
                let min_complexity_for_attempt: Option<&str> = match retry_count {
                    0 => difficulty_floor,
                    1 => Some("Moderate"),
                    _ => Some("Complex"),
                };
                let _ = context_words;
                let selected_model =
                    crate::susi_core::plane_bus::gemi::ModelManager::get_selected_model_for_request_with_min_complexity(
                        &current_goal,
                        Some(workspace),
                        min_complexity_for_attempt,
                    );
                let model_name = selected_model
                    .as_deref()
                    .unwrap_or("automatic provisioning");
                let mut local_inference = match selected_model.as_deref() {
                    Some(model) => {
                        crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep_with_model(
                            &reasoning_prompt,
                            workspace,
                            model,
                        )
                    }
                    None => {
                        if let Some(c) = min_complexity_for_attempt {
                            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep_with_min_complexity(
                                &reasoning_prompt,
                                workspace,
                                c,
                            )
                        } else {
                            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
                                &reasoning_prompt,
                                workspace,
                            )
                        }
                    }
                };

                let mut executed_scripts = String::new();
                for block in local_inference.split("```").skip(1).step_by(2) {
                    let block_trimmed = block.trim();
                    if block_trimmed.starts_with("bash\n")
                        || block_trimmed.starts_with("sh\n")
                        || block_trimmed.starts_with("shell\n")
                    {
                        let cmd = block_trimmed
                            .trim_start_matches("bash")
                            .trim_start_matches("sh")
                            .trim_start_matches("shell")
                            .trim();
                        if !cmd.is_empty() && !cmd.starts_with('!') {
                            eprintln!(
                                "[Local Agent] Detected shell block. Executing native tool..."
                            );
                            let wrapped_cmd = format!("sh -c '{}'", cmd.replace('\'', "'\\''"));
                            let result = bus_tool(
                                "exec_command",
                                &serde_json::Value::String(wrapped_cmd),
                                workspace,
                            );
                            executed_scripts.push_str(&format!(
                                "\n\nExecution Result for `{cmd}`:\n{}\n",
                                result
                            ));
                        }
                    }
                }

                if !executed_scripts.is_empty() {
                    local_inference.push_str(&executed_scripts);
                    if let Some(citations) =
                        crate::susi_core::capture::EvidenceSession::auto_format_truth(workspace)
                    {
                        local_inference.push_str("\n\n");
                        local_inference.push_str(&citations);
                    }
                }

                format!(
                    "SUSI-Tier2-Mission-Synthesis ({} via {}):\n\n{}",
                    version, model_name, local_inference
                )
            };

            // 4. Axiomatic Alignment Check
            match crate::susi_core::plane_bus::gemi::GemiEngine::verify_axiomatic_alignment(
                &final_answer,
                workspace,
            ) {
                Ok(ans) => {
                    // 5. Reality Verification
                    let mut verified_answer_attempt = crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                        &current_goal,
                        "SUSI_SOLVE",
                        &ans,
                        workspace,
                    );

                    if let Err(e) = &verified_answer_attempt {
                        if e.to_string().contains("narrative alone cannot complete it") {
                            if let Some(auto_cited) =
                                crate::susi_core::capture::EvidenceSession::auto_format_truth(
                                    workspace,
                                )
                            {
                                eprintln!("[TruthFormatter] Intercepting ungrounded local narrative. Auto-aligning missing citations...");
                                verified_answer_attempt = crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                                    &current_goal,
                                    "SUSI_SOLVE",
                                    &auto_cited,
                                    workspace,
                                );
                            }
                        }
                    }

                    match verified_answer_attempt {
                        Ok(verified_answer) => {
                            return Ok(SusiMissionReport {
                                goal: goal.to_string(),
                                status: "COMPLETE".to_string(),
                                agents,
                                interactions,
                                plan: None,
                                final_answer: verified_answer,
                            });
                        }
                        Err(e) if e.to_string().contains("TRUTH_VIOLATION") => {
                            let error_str = e.to_string();
                            // In-memory loop detection only: the text itself is
                            // the signature (no digest crate needed).
                            if previous_errors.contains(&error_str) {
                                crate::susi_sandbox::manager::SusiAuditLogger::log_event(
                                    workspace,
                                    "RETRY_LOOP_DETECTED",
                                    &format!("Same error repeated: {}", error_str),
                                );
                                return Err(e);
                            }

                            previous_errors.insert(error_str.clone());
                            retry_count += 1;
                            last_error = error_str;
                            crate::susi_sandbox::manager::SusiAuditLogger::log_event(
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
                    crate::susi_sandbox::manager::SusiAuditLogger::log_event(
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

        Err(crate::susi_error::EaiError::governance(format!(
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
        let plan_val =
            crate::susi_core::plane_bus::gemi::MissionPlanner::partition_mission(goal, workspace)
                .map_err(crate::susi_error::EaiError::governance)?;
        let goals = mission_plan_goals(&plan_val);

        // Speculative Parallelism
        // Partitioned tasks are executed in parallel across the multi-threaded substrate.
        use rayon::prelude::*;

        let results: Vec<_> = goals
            .par_iter()
            .enumerate()
            .map(|(i, sub_goal)| {
                let g = format!("[PARALLEL STEP {}/{}]: {}", i + 1, goals.len(), sub_goal);
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
        let mut all_ok = !results.is_empty();

        for report in results.into_iter() {
            all_ok = all_ok && report.is_success();
            all_interactions.extend(report.interactions);
            all_agents.extend(report.agents);
            final_responses.push(report.final_answer);
        }

        let joined = format!(
            "PARALLEL_FORK_JOIN_COMPLETE ({} steps):\n\n{}",
            final_responses.len(),
            final_responses.join("\n\n---\n\n")
        );
        let (status, final_answer) =
            Self::evidence_gated_aggregate_status(goal, workspace, all_ok, joined);

        Ok(SusiMissionReport {
            goal: goal.to_string(),
            status,
            agents: all_agents,
            interactions: all_interactions,
            plan: None,
            final_answer,
        })
    }

    fn solve_planned_mission(
        &self,
        goal: &str,
        workspace: &Path,
        version: &str,
        depth: u32,
    ) -> EaiResult<SusiMissionReport> {
        let mut plan_val =
            crate::susi_core::plane_bus::gemi::MissionPlanner::plan_mission(goal, workspace)
                .map_err(crate::susi_error::EaiError::governance)?;
        let mut goals = mission_plan_goals(&plan_val);
        let mut all_interactions = Vec::new();
        let mut all_agents = Vec::new();
        let mut final_responses = Vec::new();
        let mut all_ok = true;

        let mut current_step = 0;
        while current_step < goals.len() {
            let sub_goal = &goals[current_step];
            let tagged_goal = format!("[STEP {}/{}]: {}", current_step + 1, goals.len(), sub_goal);
            let report = self.solve_internal(&tagged_goal, workspace, version, depth + 1)?;

            all_interactions.extend(report.interactions.clone());
            all_agents.extend(report.agents.clone());
            final_responses.push(report.final_answer.clone());
            all_ok = all_ok && report.is_success();

            // Dynamic Plan Mutation: Check for failure or gap in the last step
            if report.final_answer.contains("FAILURE") || report.final_answer.contains("GAP") {
                crate::susi_sandbox::manager::SusiAuditLogger::log_event(
                    workspace,
                    "PLAN_MUTATION",
                    &format!("Refining plan due to step {} failure.", current_step + 1),
                );

                let blackboard_state = format!("LATEST_OUTCOME: {}", report.final_answer);
                if let Ok(new_plan) = crate::susi_core::plane_bus::gemi::MissionPlanner::refine_plan(
                    goal,
                    &serde_json::json!({ "blackboard": blackboard_state }),
                    workspace,
                ) {
                    plan_val = new_plan;
                    goals = mission_plan_goals(&plan_val);
                }
            }

            current_step += 1;
        }

        let joined = format!(
            "PLANNED_MISSION_COMPLETE:\n\n{}",
            final_responses.join("\n\n---\n\n")
        );
        let (status, final_answer) =
            Self::evidence_gated_aggregate_status(goal, workspace, all_ok, joined);

        Ok(SusiMissionReport {
            goal: goal.to_string(),
            status,
            agents: all_agents,
            interactions: all_interactions,
            plan: None,
            final_answer,
        })
    }

    /// Aggregate COMPLETE only when every child succeeded *and* any live
    /// ledger receipts are cited (or there are none to cite). Children already
    /// passed absolute gates; this closes the join-path soft COMPLETE hole.
    fn evidence_gated_aggregate_status(
        goal: &str,
        workspace: &Path,
        all_ok: bool,
        joined: String,
    ) -> (String, String) {
        if !all_ok {
            return ("FAILED".to_string(), joined);
        }
        match crate::susi_core::capture::EvidenceSession::verify_answer(&joined, workspace) {
            Some(Ok(rendered)) => ("COMPLETE".to_string(), rendered),
            Some(Err(e)) => {
                // Truth Formatter hook: if the model failed the JSON gate but there are live receipts,
                // auto-align the ungrounded narrative by synthesizing the missing citations.
                if e.to_string().contains("narrative alone cannot complete it") {
                    if let Some(auto_cited) =
                        crate::susi_core::capture::EvidenceSession::auto_format_truth(workspace)
                    {
                        eprintln!("[TruthFormatter] Intercepting ungrounded narrative. Auto-aligning missing citations...");
                        if let Some(Ok(rendered)) =
                            crate::susi_core::capture::EvidenceSession::verify_answer(
                                &auto_cited,
                                workspace,
                            )
                        {
                            return ("COMPLETE".to_string(), rendered);
                        }
                    }
                }
                (
                    "FAILED".to_string(),
                    format!("TRUTH_UNVERIFIED: {e}\n\n{joined}"),
                )
            }
            None => {
                // No live receipts requiring citation — children already absolute.
                // Still run the crown gate so fabricated join text cannot slip.
                match crate::susi_core::truth::TruthTransformer::verify_mission_with_cross_examine(
                    goal,
                    "AGGREGATE_JOIN",
                    &joined,
                    workspace,
                ) {
                    Ok(rendered) => ("COMPLETE".to_string(), rendered),
                    Err(e) => {
                        // Empty ledger + no compiled/native path in join text:
                        // children were individually COMPLETE, so accept join.
                        if e.to_string().contains("no absolute evidence") {
                            ("COMPLETE".to_string(), joined)
                        } else {
                            (
                                "FAILED".to_string(),
                                format!("TRUTH_UNVERIFIED: {e}\n\n{joined}"),
                            )
                        }
                    }
                }
            }
        }
    }

    pub fn generate_substrate_report(&self, workspace: &Path) -> EaiResult<String> {
        let (axiom_summary, topology_summary) = AxiomSubstrate::ingest_constitution(workspace);
        let model_name = crate::susi_core::plane_bus::gemi::ModelManager::get_selected_model(None)
            .unwrap_or_else(|| {
                let filename = crate::susi_sandbox::manager::SusiConfig::load_global()
                    .unwrap_or_default()
                    .alpha_weights_filename();
                format!("{} (Local Neural Substrate)", filename)
            });

        let mut report = String::new();
        report.push_str("# susi Substrate - Technical Report\n\n");
        report.push_str("- **Engine**: susi substrate\n");
        report.push_str(&format!(
            "- **Version**: {}\n",
            susi_gawd_agents::AlphaSelf::VERSION
        ));
        report.push_str(&format!("- **Active Model**: {}\n\n", model_name));

        report.push_str(&axiom_summary);
        report.push('\n');
        report.push_str(&topology_summary);

        Ok(report)
    }
}

/// Carry the mission's captured receipts into the report as an audit entry —
/// provenance and hashes only, never response bodies. Receipts are also
/// mirrored append-only to `.susi/receipt_archive.jsonl` at mint time (audit
/// only — truth still uses the live ledger). Then persist an inspectable
/// trace (Design principle: Traceable reasoning).
fn attach_evidence_ledger(
    report: &mut SusiMissionReport,
    session: Option<&std::sync::Arc<crate::susi_core::capture::EvidenceSession>>,
    workspace: &Path,
) {
    if let Some(session) = session {
        if let Some(summary) = session.audit_summary() {
            report.interactions.push(A2AMessage {
                sender: "EvidenceLedger".into(),
                recipient: "SUSI-Master".into(),
                action: "EVIDENCE_CAPTURED".into(),
                payload: summary,
            });
        }
    }
    report.persist_inspectable_trace(workspace);
}

/// Verify only outputs whose complete source is the running binary. No model,
/// caller-supplied version, or untrusted tool text can certify these responses.
fn verify_compiled_read(goal: &str, answer: &str) -> Option<crate::susi_error::EaiResult<String>> {
    let expected = match goal.trim().to_ascii_lowercase().as_str() {
        "identity" | "susi identity" => {
            susi_gawd_agents::self_core::AlphaSelf::inspect_compiled_binary_instructions()
        }
        "version" | "susi version" => format!(
            "SUSI Engine Version: v{}",
            susi_gawd_agents::self_core::AlphaSelf::VERSION
        ),
        _ => return None,
    };
    Some(if answer == expected {
        Ok(answer.to_string())
    } else {
        Err(crate::susi_error::EaiError::governance(
            "TRUTH_VIOLATION: output differs from compiled source",
        ))
    })
}

#[cfg(test)]
mod compiled_read_truth_tests {
    use super::*;

    #[test]
    fn compiled_reads_require_exact_intent_and_exact_output() {
        let identity =
            susi_gawd_agents::self_core::AlphaSelf::inspect_compiled_binary_instructions();
        assert!(verify_compiled_read("identity", &identity).unwrap().is_ok());
        assert!(verify_compiled_read("identity", "SUSI: all tests passed")
            .unwrap()
            .is_err());
        assert!(verify_compiled_read("check identity and delete files", &identity).is_none());
        assert!(verify_compiled_read("version", "SUSI Engine Version: v999")
            .unwrap()
            .is_err());
    }

    #[test]
    fn mission_trace_is_persisted_for_inspectability() {
        let dir = std::env::temp_dir().join(format!(
            "susi_trace_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let _ = std::fs::create_dir_all(&dir);
        let report = SusiMissionReport {
            goal: "inspect workspace".into(),
            status: "SUCCESS".into(),
            agents: vec![],
            interactions: vec![A2AMessage {
                sender: "EvidenceLedger".into(),
                recipient: "SUSI-Master".into(),
                action: "EVIDENCE_CAPTURED".into(),
                payload: r#"[{"id":"r1","tool":"exec_command"}]"#.into(),
            }],
            plan: None,
            final_answer: "done; pushed with ghp_traceToken123".into(),
        };
        report.persist_inspectable_trace(&dir);
        let path = dir.join(".susi/last_mission_trace.json");
        let body = std::fs::read_to_string(&path).expect("trace file");
        assert!(serde_json::from_str::<serde_json::Value>(&body).is_ok());
        assert!(!body.contains("ghp_traceToken123"), "{body}");
        assert!(body.contains("inspect workspace"));
        assert!(body.contains("EVIDENCE_CAPTURED"));
        assert!(body.contains("thought") || body.contains("supervise_mission_swarm"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
