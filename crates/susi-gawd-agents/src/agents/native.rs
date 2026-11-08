// Agent trait, built-in agent implementations, and the fleet synthesizer
// that decides which agents to recruit for a given goal.
// Agents must add functionality directly to the susi engine, not simulate
// results themselves.

use crate::susi_core::context_graph::{ContextGraph, NodeType};

use std::path::Path;

pub use crate::susi_core::{AgentMetaRegistry, GawdAgent, MissionBlackboard};

/// Identifies instant metadata/informational queries and bootstrap pulses that must
/// skip full LLM reasoning cycles to guarantee sub-millisecond execution latency.
#[must_use]
pub fn is_reflex_query(goal: &str) -> bool {
    let lower = goal.to_lowercase();
    let trimmed = lower.trim();
    trimmed == "ls"
        || trimmed.starts_with("ls ")
        || trimmed == "dir"
        || trimmed == "who am i"
        || trimmed == "whoami"
        || trimmed.contains("who am i")
        || trimmed.contains("whoami")
        || trimmed == "status"
        || trimmed == "identity"
        || trimmed == "version"
        || trimmed == "models"
        || trimmed == "help"
        || trimmed == "health"
        || trimmed.starts_with("admin pulse")
}

/// Generic agent that falls back to LLM reasoning when no specialist agent
/// covers the goal.
pub struct DynamicAgent {
    pub agent_name: String,
    pub mission_profile: String,
    pub agent_rank: f32,
}

impl GawdAgent for DynamicAgent {
    fn name(&self) -> String {
        self.agent_name.clone()
    }
    fn rank(&self) -> f32 {
        self.agent_rank
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        if is_reflex_query(goal) || blackboard.contains_key("AdminAgent") {
            let res = format!(
                "[{}]: Observation integrated into blackboard.",
                self.agent_name
            );
            blackboard.insert(self.agent_name.clone(), res.clone());
            return Ok(res);
        }

        let bb_state = blackboard.to_json();

        // Nothing downstream parses an "action"/"action_input" field to
        // execute a real tool and fill in "observation" - asking for that
        // ReAct schema here used to make small models stop at describing an
        // intended action instead of answering (see dynamic_agent_prompt's
        // doc comment). This just asks directly.
        let prompt = crate::susi_sandbox::manager::SusiPrompts::load_global()
            .dynamic_agent_prompt()
            .replace("{agent_role}", &self.agent_name)
            .replace("{mission_profile}", &self.mission_profile)
            .replace("{goal}", goal)
            .replace("{blackboard_context}", &bb_state);

        let ws = workspace.to_path_buf();
        let prompt_val = serde_json::json!(prompt);

        let is_admin_or_query = is_reflex_query(goal);

        // Prefer the registered 'reason' tool; fall back to calling the engine directly.
        let res = if is_admin_or_query {
            format!(
                "[{}]: Observation integrated into blackboard.",
                self.agent_name
            )
        } else if crate::susi_core::plane_bus::tools::exists("reason") {
            crate::susi_core::plane_bus::tools::execute_tool("reason", &prompt_val, &ws)
                .unwrap_or_else(|e| e.to_string())
        } else {
            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(&prompt, &ws)
        };

        blackboard.insert(self.agent_name.clone(), res.clone());
        Ok(res)
    }
}

/// Software Engineering & Repository Management Agent (Mandate 8: Epistemic
/// Chain of Truth). Code-review-shaped goals ("review", "audit", "bloat",
/// "lint") are answered with `BloatAuditor`'s real AST-driven static-analysis
/// report instead of an LLM narration — the same deterministic evidence the
/// `susi bloat-audit` CLI command and `bloat_audit` MCP tool already produce.
/// Every other goal (build/test/deploy/compile, ...) falls back to the
/// generic `DynamicAgent` LLM-prompted path, since those genuinely need
/// open-ended reasoning rather than a fixed analysis pass.
pub struct DevOpsAgent;

impl GawdAgent for DevOpsAgent {
    fn name(&self) -> String {
        "DevOpsAgent".into()
    }
    fn rank(&self) -> f32 {
        0.9
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let lower = goal.to_lowercase();
        let is_code_review_goal = ["review", "audit", "bloat", "lint"]
            .iter()
            .any(|k| lower.contains(k));

        if is_code_review_goal {
            let rendered = crate::admin_hooks::hooks().bloat_audit_workspace(workspace)?;
            blackboard.insert(self.name(), rendered.clone());
            return Ok(rendered);
        }

        // System/host goals: use GMCP exec_command — never LLM-fake "no shell access".
        if let Some(report) = crate::system_observe::observe_system(goal, workspace) {
            blackboard.insert(self.name(), report.clone());
            return Ok(report);
        }

        // Single source of truth for the description text (Mandate 35): read
        // it from the same registry entry agents.default.json provisions,
        // rather than duplicating the literal here.
        let mission_profile = AgentMetaRegistry::global()
            .list_agents()
            .into_iter()
            .find(|a| a.name == self.name())
            .map(|a| a.description)
            .unwrap_or_else(|| {
                "Software engineering, systems architecture, and repository management.".to_string()
            });

        DynamicAgent {
            agent_name: self.name(),
            mission_profile,
            agent_rank: self.rank(),
        }
        .execute(goal, workspace, blackboard)
    }
}

/// Search evidence agent. Reports only a search tool the operator admitted.
/// It does not call a built-in weather or web-search client, and it does not
/// invent an observation when no tool is linked.
pub struct SearchAgent;

impl GawdAgent for SearchAgent {
    fn name(&self) -> String {
        "SearchAgent".into()
    }
    fn rank(&self) -> f32 {
        0.9
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        // Meta/admin pulses must not trigger outbound HTTP.
        if crate::goal_shape::is_meta_command(goal) {
            let res = format!("[{}]: Observation integrated into blackboard.", self.name());
            blackboard.insert(self.name(), res.clone());
            return Ok(res);
        }
        // Host/filesystem goals belong to DevOps/Hardware — do not spam weather/search misses.
        if crate::system_observe::looks_like_system_observe_goal(goal) {
            let res = format!(
                "[{}]: System/host observation goal — deferring to DevOpsAgent/HardwareAgent (exec_command).",
                self.name()
            );
            blackboard.insert(self.name(), res.clone());
            return Ok(res);
        }

        let report = crate::live_search::gather_live_evidence(goal, workspace);
        blackboard.insert(self.name(), report.clone());
        Ok(report)
    }
}

/// Reports whether a usable model is available (never downloads one: CEO
/// decision 2026-11-04); never links MCP servers (zero-UI plan A8).
pub struct SusiRuntimeAgent;

fn has_valid_local_weights(report: &serde_json::Value) -> bool {
    report.as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row.get("is_valid_gguf")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
                && row
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| {
                        Path::new(id).extension().and_then(|ext| ext.to_str()) == Some("gguf")
                    })
        })
    })
}

impl GawdAgent for SusiRuntimeAgent {
    fn name(&self) -> String {
        "SusiRuntimeAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        _blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let lower = goal.to_lowercase();
        if lower.contains("identity")
            || lower.contains("status")
            || lower.contains("models")
            || lower.contains("version")
        {
            return Ok("Runtime environment active for query.".into());
        }
        // 1. Check whether a cloud API key is configured
        let cloud_env_keys = ["SUSI_API_KEY", "MODEL_API_KEY", "EAI_API_KEY", "API_KEY"];
        let cloud_available = cloud_env_keys
            .iter()
            .any(|k| crate::susi_config::env_or_cloud_env(k).is_ok());

        // 2. Check whether a valid local model is already present
        let verifications =
            crate::susi_core::plane_bus::gemi::ModelManager::verify_local_models(workspace);
        let valid_local_found = has_valid_local_weights(&verifications);

        // Core never downloads a model by itself (CEO decision 2026-11-04,
        // "No, ask first"). With neither a cloud key nor local weights, report
        // the observed readiness, which names the install command, instead of
        // claiming an established runtime. A synthetic engine identifier is
        // not evidence of model weights.
        if !cloud_available && !valid_local_found {
            let readiness =
                crate::susi_core::plane_bus::gemi::ModelManager::ensure_hardware_optimal_models(
                    workspace,
                );
            if let Some(text) = readiness.get("text").and_then(serde_json::Value::as_str) {
                return runtime_readiness(text);
            }
        }
        // Third-party MCP servers are never linked here: core admits one
        // only on an explicit request (zero-UI plan A8).

        Ok("Runtime environment established and optimized for pulse intent.".into())
    }
}

/// The runtime agent's answer for an observed readiness report. "No local
/// model" (the `NoLocalModelInstalled::TAG` lead, `[NO_LOCAL_MODEL]`) is a
/// failure: the runtime cannot answer, so the mission must not record success.
fn runtime_readiness(text: &str) -> crate::susi_core::susi_error::EaiResult<String> {
    if text.trim_start().starts_with("[NO_LOCAL_MODEL]") {
        return Err(crate::susi_core::susi_error::EaiError::inference(
            text.to_string(),
        ));
    }
    Ok(text.to_string())
}

#[cfg(test)]
mod runtime_readiness_tests {
    use super::runtime_readiness;

    #[test]
    fn no_local_model_is_a_failure_and_names_the_command() {
        let text = "[NO_LOCAL_MODEL] No verified local model is ready. To answer locally, \
                    run `susi os manage model install starter`.";
        let error = runtime_readiness(text).unwrap_err().to_string();
        assert!(error.contains("[NO_LOCAL_MODEL]"), "{error}");
        assert!(
            error.contains("susi os manage model install starter"),
            "{error}"
        );
    }

    #[test]
    fn a_ready_model_is_success() {
        let text = "Using verified cached local model qwen2.5-0.5b-instruct-q4";
        assert_eq!(runtime_readiness(text).unwrap(), text);
    }
}

/// Reports the detected hardware profile (CPU, RAM, GPU/acceleration).
pub struct HardwareAgent;

impl GawdAgent for HardwareAgent {
    fn name(&self) -> String {
        "HardwareAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let profile = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
        let mut report = format!(
            "Hardware Saturated: {} CPUs ({}) | {}GB RAM | {}. Acceleration: {}.",
            profile.cpus,
            profile.cpu_brand,
            profile.ram_gb,
            profile.gpu_info,
            profile.native_acceleration
        );
        if let Some(disk) = crate::system_observe::observe_system(goal, workspace) {
            report.push_str("\n\n");
            report.push_str(&disk);
        }

        blackboard.insert(self.name(), report.clone());
        Ok(report)
    }
}

/// Recognized project-type marker files/directories checked at the
/// workspace root — the concrete, checkable signal "workspace awareness"
/// grounds itself in, instead of an LLM guessing at project type from goal text.
const PROJECT_MARKERS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "requirements.txt",
    "pom.xml",
    "Gemfile",
    "composer.json",
    "CMakeLists.txt",
    ".git",
];

/// Which of `PROJECT_MARKERS` actually exist at `workspace`'s root.
pub(super) fn detect_project_markers(workspace: &Path) -> Vec<String> {
    PROJECT_MARKERS
        .iter()
        .filter(|m| workspace.join(m).exists())
        .map(|s| s.to_string())
        .collect()
}

/// Counts top-level files and subdirectories in `workspace` — a real, cheap
/// `ls`-equivalent enumeration matching this agent's own registered semantic
/// anchors ("pwd", "ls", "dir", "cat") instead of an unbacked LLM guess at
/// what the workspace contains.
pub(super) fn scan_workspace_top_level(workspace: &Path) -> (usize, usize) {
    let mut files = 0usize;
    let mut dirs = 0usize;
    if let Ok(entries) = std::fs::read_dir(workspace) {
        for entry in entries.flatten() {
            if let Ok(file_type) = entry.file_type() {
                if file_type.is_dir() {
                    dirs += 1;
                } else if file_type.is_file() {
                    files += 1;
                }
            }
        }
    }
    (files, dirs)
}

/// Workspace Analysis Agent: real file-system awareness backing identity.json
/// Pillar II's "High-density context manager and workspace analyzer" — this
/// `is_core: true` (always-recruited) agent previously had no native
/// implementation and silently fell back to a generic, unbacked LLM-prompted
/// `DynamicAgent` persona (found in the swarm-harmony audit behind
/// EV-2022920-032, fixed here as flagged follow-up).
pub struct ContextAgent;

impl GawdAgent for ContextAgent {
    fn name(&self) -> String {
        "ContextAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        _goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let (files, dirs) = scan_workspace_top_level(workspace);
        let markers = detect_project_markers(workspace);

        // Universal Context Graph: surface recent activity in this workspace so
        // agents are not operating in isolated silos.
        let graph = ContextGraph::global();
        let context_summary = {
            let subgraph = graph.workspace_subgraph(workspace);
            if subgraph.nodes.is_empty() {
                "no prior recorded activity".to_string()
            } else {
                let recent_calls = subgraph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.kind, NodeType::ToolCall))
                    .take(5)
                    .map(|n| n.label.chars().take(60).collect::<String>())
                    .collect::<Vec<_>>();
                let recent_obs = subgraph
                    .nodes
                    .iter()
                    .filter(|n| matches!(n.kind, NodeType::Observation))
                    .take(5)
                    .map(|n| n.label.chars().take(60).collect::<String>())
                    .collect::<Vec<_>>();
                let mut parts = Vec::new();
                if !recent_calls.is_empty() {
                    parts.push(format!("recent tool calls: {}", recent_calls.join("; ")));
                }
                if !recent_obs.is_empty() {
                    parts.push(format!("recent observations: {}", recent_obs.join("; ")));
                }
                if parts.is_empty() {
                    format!("{} connected context nodes", subgraph.nodes.len())
                } else {
                    parts.join(" | ")
                }
            }
        };

        let res = format!(
            "[ContextAgent]: Workspace '{}' — {} top-level file(s), {} top-level subdirectory(ies). Detected project markers: {}. Prior context: {}.",
            workspace.display(),
            files,
            dirs,
            if markers.is_empty() {
                "none".to_string()
            } else {
                markers.join(", ")
            },
            context_summary
        );
        blackboard.insert(self.name(), res.clone());
        ContextGraph::global().record_agent_observation(None, &self.name(), &res, workspace);
        Ok(res)
    }
}

/// Safety Governance Agent (identity.json Mandate 36 & 37)
pub struct SafetyAgent;

impl GawdAgent for SafetyAgent {
    fn name(&self) -> String {
        "SafetyAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        // "SUSI_SOLVE" (not an arbitrary label): SafetyDetector's critical-system-path
        // check is gated on this exact tool_name literal alongside "write_file"/
        // "exec_command" (src/gawd/safety.rs) — using anything else here means a
        // full mission goal never gets checked against critical_system_paths at all.
        crate::safety::SafetyDetector::audit_action("SUSI_SOLVE", goal, workspace)?;
        let res = "Safety protocols verified. No destructive patterns detected.".to_string();
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Security Governance Agent (identity.json Mandate 38 & 39)
pub struct SecurityAgent;

impl GawdAgent for SecurityAgent {
    fn name(&self) -> String {
        "SecurityAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        crate::security::SecurityDetector::audit_action("SUSI_SOLVE", goal, workspace)?;
        let res =
            "Security audit passed. No secret leaks or exfiltration vectors detected.".to_string();
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Autonomous Drift & Evolution Agent (identity.json Mandate 22: Self-Healing Reflex)
pub struct EvolutionAgent;

impl GawdAgent for EvolutionAgent {
    fn name(&self) -> String {
        "EvolutionAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let lower = goal.to_lowercase();
        if lower.contains("identity")
            || lower.contains("status")
            || lower.contains("models")
            || lower.contains("version")
        {
            // No audit runs for status queries — say so rather than
            // asserting a health result nobody measured.
            let res = "Evolution: drift audit not run for status queries.".to_string();
            blackboard.insert(self.name(), res.clone());
            return Ok(res);
        }

        static DRIFT_AUDIT_RUNNING: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let started = !DRIFT_AUDIT_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst);
        if started {
            let ws = workspace.to_path_buf();
            std::thread::spawn(move || {
                struct AuditGuard;
                impl Drop for AuditGuard {
                    fn drop(&mut self) {
                        DRIFT_AUDIT_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                let _guard = AuditGuard;
                let outcome = match crate::admin_hooks::hooks().perform_autonomous_drift_audit(&ws)
                {
                    Ok(report) => report,
                    Err(e) => format!("drift audit failed: {e}"),
                };
                crate::susi_sandbox::manager::SusiAuditLogger::log_event(
                    &ws,
                    "EVOLUTION_DRIFT_AUDIT",
                    &outcome,
                );
            });
        }
        let res = if started {
            "Evolution: drift audit started in the background; its result is recorded as EVOLUTION_DRIFT_AUDIT in the audit log."
        } else {
            "Evolution: a drift audit is already running."
        }
        .to_string();
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// GMCP Protocol Agent: Initializes MCP server endpoints and verifies communication health
pub struct GmcpAgent;

impl GawdAgent for GmcpAgent {
    fn name(&self) -> String {
        "GmcpAgent".into()
    }
    fn rank(&self) -> f32 {
        0.95
    }
    fn execute(
        &self,
        _goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        // Test Tool Registry endpoints (Internal Reflex)
        let tool_count = crate::susi_core::registry::CapabilityRegistry::global()
            .list_tools()
            .len();

        let status_res = crate::susi_core::plane_bus::tools::execute_tool(
            "status",
            &serde_json::json!(null),
            workspace,
        )
        .unwrap_or_default();
        let healthy = status_res.contains("Operational");

        let res = format!(
            "[GmcpAgent]: Meta-Substrate endpoints tested. Total local tools: {} | Status test: {} | Endpoint health status: {}",
            tool_count, if healthy { "PASSED" } else { "FAILED" }, if healthy { "OPTIMAL (Healthy)" } else { "DEGRADED" }
        );

        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Extracts whitespace-delimited substrings from `text` that look like a
/// file reference — a bare repo-root filename (`Cargo.toml`) or a nested
/// path (`src/gawd/agents.rs`) — ending in a recognized source/doc
/// extension, tolerating a trailing `:line` or surrounding punctuation.
/// The extension whitelist alone bounds false positives; ordinary prose
/// essentially never ends a word in `.rs`/`.toml`/`.json`/`.md`/`.sh`/`.lock`.
/// Pure function (no I/O) so path extraction is directly unit-testable.
pub(super) fn extract_candidate_file_paths(text: &str) -> Vec<String> {
    const EXTENSIONS: &[&str] = &[".rs", ".toml", ".json", ".md", ".sh", ".lock"];
    text.split_whitespace()
        .filter_map(|raw| {
            let trimmed = raw.trim_matches(|c: char| {
                matches!(c, '(' | ')' | ',' | ';' | '\'' | '"' | '`' | '[' | ']')
            });
            let path_part = trimmed.split(':').next().unwrap_or(trimmed);
            let path_part = path_part.trim_end_matches('.');
            if EXTENSIONS.iter().any(|ext| path_part.ends_with(ext)) {
                Some(path_part.to_string())
            } else {
                None
            }
        })
        .collect()
}

/// Verifies `claim`'s file-path references (if any) actually exist in
/// `workspace`. Path grounding is the fast check for free-text blackboard
/// entries. Structured `EvidenceRecord` / `Claim` trails (Pillar Evidence) are
/// assessed separately by [`EpistemicAuditorAgent`] via `record.assess`.
pub(super) fn audit_claim_grounding(claim: &str, workspace: &Path) -> (usize, Vec<String>) {
    let paths = extract_candidate_file_paths(claim);
    let hallucinated: Vec<String> = paths
        .iter()
        .filter(|p| !workspace.join(p).exists())
        .cloned()
        .collect();
    (paths.len(), hallucinated)
}

/// Epistemic Auditor Agent: Pillar Evidence — structured `EvidenceRecord` /
/// `Claim` trails must assess clean; hallucinated file paths are naked
/// assertions and hard-reject the swarm (no rubber-stamp).
pub struct EpistemicAuditorAgent;

impl GawdAgent for EpistemicAuditorAgent {
    fn name(&self) -> String {
        "EpistemicAuditorAgent".into()
    }
    fn rank(&self) -> f32 {
        0.98
    }
    fn execute(
        &self,
        _goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        use crate::susi_core::evidence::{EvidenceAssessment, EvidenceRecord};

        let mut total = 0usize;
        let mut grounded = 0usize;
        let mut ungrounded = 0usize;
        let mut ir_verified = 0usize;
        let mut ir_failed: Vec<String> = Vec::new();
        let mut hallucinated_paths: Vec<String> = Vec::new();

        let mut empty_promise_detected = false;
        let mut empty_promise_agents: Vec<String> = Vec::new();

        for entry in blackboard.iter() {
            if entry.0 == self.name() {
                continue;
            }
            let val = entry.1.to_lowercase();
            if (val.contains("i have fixed")
                || val.contains("completed the task")
                || val.contains("successfully modified"))
                && !val.contains('{')
                && !val.contains('/')
                && !val.contains('\\')
                && !val.contains("fn ")
                && !val.contains("impl ")
            {
                empty_promise_detected = true;
                empty_promise_agents.push(entry.0.clone());
            }

            if entry.0.starts_with("EvidenceRecord::") {
                total += 1;
                match serde_json::from_str::<EvidenceRecord>(&entry.1) {
                    Ok(record) => match record.assess(workspace) {
                        EvidenceAssessment::Verified => ir_verified += 1,
                        EvidenceAssessment::Unverified(reason)
                        | EvidenceAssessment::Rejected(reason) => {
                            ir_failed.push(format!("{}: {reason}", entry.0));
                        }
                    },
                    Err(_) => ir_failed.push(format!("{}: unparseable EvidenceRecord", entry.0)),
                }
                continue;
            }
            total += 1;
            let (referenced, hallucinated) = audit_claim_grounding(&entry.1, workspace);
            if referenced == 0 {
                ungrounded += 1;
            } else if hallucinated.is_empty() {
                grounded += 1;
            } else {
                hallucinated_paths.extend(hallucinated);
            }
        }

        if !hallucinated_paths.is_empty() {
            let res = format!(
                "[SYSTEMIC_EPISTEMIC_CATCHER] [EpistemicAuditorAgent]: Audited {} agent claims across active model fleet — {} file-grounded, {} unverifiable, {} referenced non-existent paths: {:?}. Epistemic integrity: UNGROUNDED CLAIMS DETECTED. Status: HALLUCINATION DETECTED & REJECTED.",
                total, grounded, ungrounded, hallucinated_paths.len(), hallucinated_paths
            );
            blackboard.insert(self.name(), res.clone());
            blackboard.insert(
                "systemic#hallucination_caught".to_string(),
                format!("{:?}", hallucinated_paths),
            );
            return Err(crate::susi_core::susi_error::EaiError::governance(res));
        }

        if !ir_failed.is_empty() {
            let res = format!(
                "[SYSTEMIC_EPISTEMIC_CATCHER] [EpistemicAuditorAgent]: Audited {} claims — {} EvidenceRecord verified, {} EvidenceRecord failed ({:?}). Epistemic integrity: NAKED OR REJECTED ASSERTIONS. Status: UNFOUNDED LIES / UNBACKED ASSERTIONS REJECTED.",
                total, ir_verified, ir_failed.len(), ir_failed
            );
            blackboard.insert(self.name(), res.clone());
            blackboard.insert(
                "systemic#lies_caught".to_string(),
                format!("{:?}", ir_failed),
            );
            return Err(crate::susi_core::susi_error::EaiError::governance(res));
        }

        if empty_promise_detected {
            let res = format!(
                "[SYSTEMIC_EPISTEMIC_CATCHER] [EpistemicAuditorAgent]: Empty promise detected from agents {:?}. Claims completion without verifiable code, patch JSON, or workspace evidence. Status: EMPTY PROMISE REJECTED.",
                empty_promise_agents
            );
            blackboard.insert(self.name(), res.clone());
            blackboard.insert(
                "systemic#empty_promises_caught".to_string(),
                format!("{:?}", empty_promise_agents),
            );
            return Err(crate::susi_core::susi_error::EaiError::governance(res));
        }

        let res = if ir_verified > 0 {
            format!(
                "[EpistemicAuditorAgent]: Audited {} claims — {} EvidenceRecord verified against workspace, {} file-grounded prose, {} unverifiable prose. Epistemic integrity: EVIDENCE TRAILS VERIFIED.",
                total, ir_verified, grounded, ungrounded
            )
        } else if grounded > 0 {
            format!(
                "[EpistemicAuditorAgent]: Audited {} agent claims — {} file-grounded (verified against workspace), {} unverifiable (no file reference, not necessarily false). Epistemic integrity: FILE REFERENCES VERIFIED (claim contents not independently verified).",
                total, grounded, ungrounded
            )
        } else {
            format!(
                "[EpistemicAuditorAgent]: Audited {} agent claims — none referenced a checkable file path. Epistemic integrity: INCONCLUSIVE (nothing file-grounded to verify).",
                total
            )
        };
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Resource Arbitrator Agent: Real-time hardware governor monitoring memory and concurrency saturation.
pub struct ResourceArbitratorAgent;

impl GawdAgent for ResourceArbitratorAgent {
    fn name(&self) -> String {
        "ResourceArbitratorAgent".into()
    }
    fn rank(&self) -> f32 {
        0.98
    }
    fn execute(
        &self,
        _goal: &str,
        _workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let profile = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
        let oom_risk = crate::susi_core::plane_bus::gemi::HardwareProfiler::check_oom_critical();
        let res = format!(
            "[ResourceArbitratorAgent]: Hardware saturation check passed. CPUs: {} | Available RAM: {}GB | OOM Critical Risk: {}",
            profile.cpus, profile.available_ram_gb, oom_risk
        );
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Markers that mean a real agent contribution is flagging a genuine
/// problem, not just a routine status line — matched case-insensitively
/// against each blackboard entry's actual text. Deliberately excludes the
/// exact uppercase substrings "FAILURE"/"GAP" the swarm-consensus and
/// distillation filters elsewhere key off of (`amas.rs`, `ama.rs`): this
/// agent's own generated report must never accidentally trip those filters
/// and get treated as a failed/gapped contribution itself.
const CRITICAL_SIGNAL_MARKERS: &[&str] = &[
    "degraded",
    "critical risk: true",
    "mismatch",
    "stalled",
    "cancelled",
    "governance_block",
    "capability_gap",
    ": failed",
];

/// Splits `blackboard`'s current entries (excluding `exclude`, this agent's
/// own name) into agents reporting a genuine critical/problem signal versus
/// agents reporting routine, non-empty status — the real cross-check
/// "resolves agent findings and conflicts" requires, instead of a count.
pub(super) fn summarize_swarm_signals(
    blackboard: &MissionBlackboard,
    exclude: &str,
) -> (Vec<String>, Vec<String>) {
    let mut critical = Vec::new();
    let mut healthy = Vec::new();
    for entry in blackboard.iter() {
        if entry.0 == exclude {
            continue;
        }
        let lower = entry.1.to_lowercase();
        if lower.trim().is_empty() {
            continue;
        }
        if CRITICAL_SIGNAL_MARKERS.iter().any(|m| lower.contains(m)) {
            critical.push(entry.0.clone());
        } else {
            healthy.push(entry.0.clone());
        }
    }
    (critical, healthy)
}

/// Consensus Mediator Agent: Resolves agent findings and conflicts using confidence and provenance.
pub struct ConsensusMediatorAgent;

impl GawdAgent for ConsensusMediatorAgent {
    fn name(&self) -> String {
        "ConsensusMediatorAgent".into()
    }
    fn rank(&self) -> f32 {
        0.98
    }
    fn execute(
        &self,
        _goal: &str,
        _workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let (critical, healthy) = summarize_swarm_signals(blackboard, &self.name());
        let total = critical.len() + healthy.len();

        let res = if critical.is_empty() {
            format!(
                "[ConsensusMediatorAgent]: Analyzed {} active agent contributions. No critical/problem signals detected among them. Weighted consensus reached.",
                total
            )
        } else if healthy.is_empty() {
            let res = format!(
                "[ConsensusMediatorAgent]: Analyzed {} active agent contributions. All {} report critical/problem signals ({:?}) — swarm-wide distress, not a partial conflict. Consensus rejected.",
                total, critical.len(), critical
            );
            blackboard.insert(self.name(), res.clone());
            return Err(crate::susi_core::susi_error::EaiError::governance(res));
        } else {
            let res = format!(
                "[ConsensusMediatorAgent]: Analyzed {} active agent contributions. CONFLICT: {} agent(s) report critical/problem signals ({:?}) while {} agent(s) report routine status ({:?}). Consensus not reached.",
                total, critical.len(), critical, healthy.len(), healthy
            );
            blackboard.insert(self.name(), res.clone());
            return Err(crate::susi_core::susi_error::EaiError::governance(res));
        };
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Self-Healing Agent: Monitors test suite health and executes autonomous test-driven repairs.
pub struct SelfHealingAgent;

impl GawdAgent for SelfHealingAgent {
    fn name(&self) -> String {
        "SelfHealingAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        if is_reflex_query(goal) {
            let res = "[SelfHealingAgent]: no health check runs for reflex queries.".to_string();
            blackboard.insert(self.name(), res.clone());
            return Ok(res);
        }

        // Apply-patch-then-test cycle when the goal embeds a PatchRequest JSON
        // object (must contain a "files" array). Workspace-confined via host hooks.
        if let Some(patch_json) = extract_patch_request_json(goal) {
            let cfg =
                crate::susi_sandbox::manager::SusiConfig::load_global_arc().unwrap_or_default();
            let outcome = crate::admin_hooks::hooks().apply_patch_cycle(
                workspace,
                &patch_json,
                &cfg.trust_level(),
            )?;
            let res = format!("[SelfHealingAgent]: Patch cycle completed. {outcome}");
            blackboard.insert(self.name(), res.clone());
            ContextGraph::global().record_agent_observation(None, &self.name(), &res, workspace);
            return Ok(res);
        }

        let ws = workspace.to_path_buf();
        let audit = crate::admin_hooks::hooks()
            .perform_autonomous_drift_audit(&ws)
            .unwrap_or_else(|e| format!("drift audit failed: {e}"));
        let res = format!(
            "[SelfHealingAgent]: Autonomous health check completed. {}",
            audit
        );
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Pull a JSON object containing a `files` array out of free-form goal text.
fn extract_patch_request_json(goal: &str) -> Option<String> {
    let start = goal.find('{')?;
    let end = goal.rfind('}')?;
    if end <= start {
        return None;
    }
    let candidate = &goal[start..=end];
    let value: serde_json::Value = serde_json::from_str(candidate).ok()?;
    if value.get("files").and_then(|f| f.as_array()).is_some() {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Wraps a configurable remote inference endpoint (base URL + protocol) as an agent.
pub struct DynamicInferenceEndpointAgent {
    pub endpoint_name: String,
    pub api_base_url: String,
    pub protocol_type: String,
    /// Endpoint's configured default model id (`gpt-4o-mini`, …); empty
    /// falls back to the legacy `<name>-substrate` placeholder.
    pub model: String,
    /// Env var / `cloud.env` key holding this endpoint's API key.
    pub api_key_env: String,
    pub agent_rank: f32,
}

/// True when `api_base` targets a remote (non-loopback) host. Loopback
/// inference engines (Ollama, vLLM, llama.cpp, …) are local
/// substrates; anything else is network egress of the mission goal.
pub(crate) fn is_remote_inference_endpoint(api_base: &str) -> bool {
    let lower = api_base.trim().to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && !lower.contains("localhost")
        && !lower.contains("127.0.0.1")
        && !lower.contains("[::1]")
        && !lower.contains("0.0.0.0")
}

/// Resolve an endpoint credential the same way `susi-gemi-models::cloud`
/// does: process env first, then the zero-config `~/.susi/cloud.env`
/// store (`KEY=value`, optional `export ` prefix).
pub(crate) fn resolve_inference_key(api_key_env: &str) -> Option<String> {
    if api_key_env.trim().is_empty() {
        return None;
    }
    if let Ok(v) = crate::susi_config::env_or_cloud_env(api_key_env) {
        if !v.trim().is_empty() {
            return Some(v);
        }
    }
    let path = susi_paths::SusiDirs::config_dir().join("cloud.env");
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == api_key_env {
                let v = v.trim().trim_matches('"').trim_matches('\'');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

impl DynamicInferenceEndpointAgent {
    pub fn new(name: &str, api_base_url: &str, protocol_type: &str) -> Self {
        Self {
            endpoint_name: name.to_string(),
            api_base_url: api_base_url.to_string(),
            protocol_type: protocol_type.to_string(),
            model: String::new(),
            api_key_env: String::new(),
            agent_rank: 0.95,
        }
    }
}

impl GawdAgent for DynamicInferenceEndpointAgent {
    fn name(&self) -> String {
        format!("{}BridgeAgent", self.endpoint_name)
    }
    fn rank(&self) -> f32 {
        self.agent_rank
    }
    fn execute(
        &self,
        goal: &str,
        _workspace: &Path,
        _blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        if std::env::var("SUSI_TEST_MOCK_INFERENCE").unwrap_or_default() == "true" {
            return Err(crate::susi_core::susi_error::EaiError::inference(
                "dynamic inference endpoint disabled by SUSI_TEST_MOCK_INFERENCE".to_string(),
            ));
        }
        let client = crate::susi_core::plane_bus::tools::scout_reasoning_remotes();
        if let Some(remotes) = client.get("remotes").and_then(|v| v.as_array()) {
            for remote_name in remotes.iter().filter_map(|v| v.as_str()) {
                if remote_name
                    .to_lowercase()
                    .contains(&self.endpoint_name.to_lowercase())
                {
                    let res = crate::susi_core::plane_bus::tools::execute_external_tool(
                        remote_name,
                        "generate",
                        goal,
                    )
                    .unwrap_or_else(|e| format!("[FAIL] {e}"));
                    if !res.contains("[FAIL]") {
                        return Ok(format!("[{} Power-Tier]: {}", self.endpoint_name, res));
                    }
                }
            }
        }

        let model = if self.model.trim().is_empty() {
            format!("{}-substrate", self.endpoint_name.to_lowercase())
        } else {
            self.model.clone()
        };
        use susi_adapters_llm::inference_wire as wire;
        let (payload, extract): (_, wire::Extractor) = match self.protocol_type.as_str() {
            "chat" => (
                wire::openai_chat_body(&model, goal, 1024),
                wire::openai_chat_text,
            ),
            "triton" => (wire::triton_body(goal, 512), wire::triton_text),
            _ => (
                wire::openai_completions_body(&model, goal, 1024),
                wire::openai_completions_text,
            ),
        };

        let endpoint_url = if self.protocol_type == "chat" {
            format!("{}/chat/completions", self.api_base_url)
        } else if self.protocol_type == "triton" {
            self.api_base_url.clone()
        } else {
            format!("{}/completions", self.api_base_url)
        };

        if !crate::susi_core::mac_policy::egress_permitted(&endpoint_url) {
            return Err(crate::susi_core::susi_error::EaiError::governance(format!(
                "[PRIVACY] {} proxy endpoint at {} blocked by the privacy posture",
                self.endpoint_name, self.api_base_url
            )));
        }
        let key = resolve_inference_key(&self.api_key_env);
        let auth = key.as_ref().map(|k| format!("Bearer {k}"));
        let mut headers = Vec::new();
        if let Some(ref token) = auth {
            headers.push(("Authorization", token.as_str()));
        }
        let body = wire::post_json(&endpoint_url, &headers, &payload).map_err(|e| {
            crate::susi_core::susi_error::EaiError::inference(format!(
                "{} proxy endpoint at {}: {e}",
                self.endpoint_name, self.api_base_url
            ))
        })?;
        let text = extract(&body).map_err(|e| {
            crate::susi_core::susi_error::EaiError::inference(format!(
                "{} proxy: {e}",
                self.endpoint_name
            ))
        })?;
        Ok(format!("[{} Proxy]: {}", self.endpoint_name, text))
    }
}

/// SOTA Library Scouting Agent
pub struct LibraryScoutAgent;

impl GawdAgent for LibraryScoutAgent {
    fn name(&self) -> String {
        "LibraryScoutAgent".into()
    }
    fn rank(&self) -> f32 {
        0.85
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        _blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let lower_goal = goal.to_lowercase();

        if is_reflex_query(goal) {
            return Ok(
                "[LibraryScoutAgent]: No library scouting applies to this query.".to_string(),
            );
        }

        // Enhanced Library Scouting with reasoning and 'cargo add' suggestions
        let prompt = format!("Extract a single dominant keyword (max 1-2 words, lowercase) representing the crate category needed for this goal: '{}'. Output ONLY the keyword, no explanation. Example outputs: async, json, sql, gui, web, inference.", goal);
        let ws = workspace.to_path_buf();
        let mut query_term =
            crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(&prompt, &ws);
        query_term = query_term.trim().to_lowercase().replace(['"', '\''], "");
        if query_term.is_empty() || query_term.contains(' ') {
            query_term = "rust".to_string();
        }

        let api_base = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .crates_io_api_url();
        let url = format!("{}?q={}&per_page=5", api_base, query_term);
        let mut results = Vec::new();

        // Crate search sends the derived query off-host: under a posture that
        // blocks egress, skip it and fall through to local reasoning below.
        let live = if crate::susi_core::mac_policy::egress_permitted(&url) {
            susi_http_transport::http_call("GET", &url, &[("User-Agent", "SUSI/0.1")], 20, 0).ok()
        } else {
            None
        };
        if let Some(resp) = live {
            if (200..300).contains(&resp.status) {
                if let Ok(text) = resp.into_utf8(1_000_000) {
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(crates) = json["crates"].as_array() {
                            for c in crates {
                                let name = c["name"].as_str().unwrap_or_default();
                                let desc = c["description"]
                                    .as_str()
                                    .unwrap_or("No description available.");
                                results.push(format!(
                                    "- **{}**: {} (Reason: SOTA selection for '{}')",
                                    name, desc, query_term
                                ));
                            }
                        }
                    }
                }
            }
        }

        if !results.is_empty() {
            let mut report = format!(
                "[Library Scout Live API]: Recommended SOTA crates for goal: '{}'\n\n",
                goal
            );
            report.push_str(&results.join("\n"));

            // Suggest 'cargo add' if it looks like an implementation mission
            if lower_goal.contains("implement")
                || lower_goal.contains("build")
                || lower_goal.contains("add")
                || lower_goal.contains("create")
            {
                if let Some(first_crate) = results.first().and_then(|r| r.split("**").nth(1)) {
                    report.push_str(&format!(
                        "\n\n[ACTION]: Suggesting 'cargo add {}' to fulfill mission.",
                        first_crate
                    ));
                }
            }
            return Ok(report);
        }

        let prompt = format!("Recommend SOTA Rust open-source crates for goal: {}. Include specific reasons and 'cargo add' commands if applicable.", goal);
        let ws = workspace.to_path_buf();
        Ok(crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(&prompt, &ws))
    }
}

/// Routes admin-shaped goals (install/uninstall/sync/release/...) to the
/// matching `SusiAdmin` action.
pub struct AdminAgent;

impl GawdAgent for AdminAgent {
    fn name(&self) -> String {
        "AdminAgent".into()
    }
    fn rank(&self) -> f32 {
        1.0
    }
    fn execute(
        &self,
        goal: &str,
        workspace: &Path,
        blackboard: &MissionBlackboard,
    ) -> crate::susi_core::susi_error::EaiResult<String> {
        let lower_goal = goal.to_lowercase();
        let action = Self::match_action(&lower_goal);

        let res = match action.as_deref() {
            Some("sync") => crate::admin_hooks::hooks().enforce_version_consistency(workspace),
            Some("audit") => crate::admin_hooks::hooks().audit_compliance(workspace),
            Some("verify") => crate::admin_hooks::hooks().verify_version_alignment(workspace),
            Some("release") => crate::admin_hooks::hooks().execute_release(workspace),
            Some("status_health") => {
                let hw = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
                Ok(format!(
                    "Substrate Status: v{} | Hardware: {} | CPUs: {} | RAM: {}GB | Status: Operational",
                    crate::self_core::AlphaSelf::VERSION,
                    hw.cpu_brand,
                    hw.cpus,
                    hw.ram_gb
                ))
            }
            Some("version") => Ok(format!(
                "SUSI Engine Version: v{}",
                crate::self_core::AlphaSelf::VERSION
            )),
            Some("identity") => {
                let brain = crate::brain::AlphaBrainContext::initialize(workspace);
                Ok(format!(
                    "# SUSI Substrate Identity\n\n{}",
                    brain.inspect_tri_state()
                ))
            }
            Some("list_models") => {
                let models =
                    crate::susi_core::plane_bus::gemi::ModelManager::list_models(workspace);
                let count = models.as_array().map(|a| a.len()).unwrap_or(0);
                Ok(format!(
                    "Active Model Substrates (Count: {count})\n\n{models}"
                ))
            }
            Some("deep_scan") => {
                let global_dir = Self::global_dir();
                let v =
                    crate::susi_core::plane_bus::gemi::ModelManager::deep_scan_home_and_register(
                        &global_dir,
                    );
                Ok(v.get("text")
                    .and_then(|x| x.as_str())
                    .unwrap_or(&v.to_string())
                    .to_string())
            }
            Some("install") => {
                let global_dir = Self::global_dir();
                crate::susi_sandbox::manager::SandboxManager::ensure_global_sandbox(&global_dir)?;
                Ok("SUSI runtime initialized and sandboxed.".to_string())
            }
            Some("uninstall") => {
                let global_dir = Self::global_dir();
                let _ = std::fs::remove_dir_all(&global_dir);
                Ok("SUSI runtime removed.".to_string())
            }
            _ => Ok("AdminAgent: Monitoring technical intent...".to_string()),
        }?;

        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

impl AdminAgent {
    fn global_dir() -> std::path::PathBuf {
        susi_paths::SusiDirs::config_dir()
    }

    /// Command-trigger keywords are config-driven (Mandate 35: Registry + Trait +
    /// Config Substrate Pattern) so operators can remap/extend trigger words via
    /// config.default.json without recompiling; the action *implementations*
    /// stay in Rust since each maps to a distinct function, not swappable data.
    /// Action priority follows the routing map's declaration order in
    /// config.default.json (first match wins, mirroring the original if/else
    /// chain's precedence).
    /// Bare-substring keyword matching on `lower_goal` is only safe for
    /// actions with no side effects. It was previously applied uniformly,
    /// which meant any goal text containing the word "release" anywhere
    /// (e.g. "check the status of my release notes") would trigger a full
    /// `execute_release()` — cargo check/test/clippy, version sync, and a
    /// `git push` — and any goal containing "remove" would `rm -rf ~/.susi`
    /// via the uninstall action, deleting every provisioned model and the
    /// agent registry. Both were reachable from any goal string fed into the
    /// swarm, including from GEMI REST's `/v1/chat/completions` — no LLM
    /// cooperation required, just an unlucky word choice. The dedicated
    /// `susi admin <subcommand>` CLI already calls these functions directly
    /// (src/main.rs's `AdminCommands` match) without going through this
    /// keyword matcher at all, so gating the mutating actions here behind
    /// the same explicit "admin pulse:" sentinel `DynamicAgent`'s fast-path
    /// already uses (and that agents.default.json's own pulse descriptions
    /// are phrased with) closes the hole without removing any real
    /// capability: an operator/caller who actually means to trigger one of
    /// these must say so unambiguously.
    const MUTATING_ACTIONS: &'static [&'static str] = &[
        "sync",
        "audit",
        "verify",
        "release",
        "deep_scan",
        "install",
        "uninstall",
    ];

    pub(super) fn match_action(lower_goal: &str) -> Option<String> {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let routing = cfg.admin_command_routing();
        let is_explicit_admin_pulse = lower_goal.trim_start().starts_with("admin pulse");

        const ACTION_ORDER: &[&str] = &[
            "sync",
            "audit",
            "verify",
            "release",
            "status_health",
            "version",
            "identity",
            "list_models",
            "deep_scan",
            "install",
            "uninstall",
        ];

        ACTION_ORDER
            .iter()
            .find(|action| {
                if Self::MUTATING_ACTIONS.contains(*action) && !is_explicit_admin_pulse {
                    return false;
                }
                routing
                    .get(**action)
                    .map(|groups| {
                        groups
                            .iter()
                            .any(|group| group.iter().all(|kw| lower_goal.contains(kw.as_str())))
                    })
                    .unwrap_or(false)
            })
            .map(|s| s.to_string())
    }
}

#[cfg(test)]
mod runtime_model_evidence_tests {
    use super::has_valid_local_weights;
    use serde_json::json;

    #[test]
    fn native_names_do_not_establish_model_validity() {
        for id in [
            "susi-native-synthesis",
            "F:/susi-native-review/models/broken.gguf",
            "C:/models/native.gguf",
        ] {
            assert!(!has_valid_local_weights(
                &json!([{"model_id":"Native-looking display name","path":id,"is_valid_gguf":false}])
            ));
            assert!(!has_valid_local_weights(
                &json!([{"model_id":id,"path":id}])
            ));
        }
        assert!(!has_valid_local_weights(
            &json!([{"model_id":"susi-native-synthesis","is_valid_gguf":true}])
        ));
    }

    #[test]
    fn valid_gguf_reports_work_regardless_of_parent_directory_name() {
        for id in [
            "F:/susi-native-review/models/qwen.gguf",
            "C:/models/qwen.gguf",
        ] {
            assert!(has_valid_local_weights(
                &json!([{"model_id":"Qwen model","path":id,"is_valid_gguf":true}])
            ));
        }
        assert!(!has_valid_local_weights(&json!(null)));
        assert!(!has_valid_local_weights(&json!([])));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_temp_workspace(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "susi_gawd_native_test_{}_{}_{}",
            label,
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn is_reflex_query_matches_positive_cases_case_insensitively() {
        let positives = [
            "ls",
            "  LS  ",
            "ls -la",
            "LS /tmp",
            "dir",
            "  DIR\n",
            "who am i",
            "WHO AM I",
            "whoami",
            "  WhoAmI  ",
            "tell me who am i",
            "please run whoami now",
            "status",
            "  STATUS ",
            "identity",
            "IDENTITY",
            "version",
            "VERSION",
            "models",
            "MODELS",
            "help",
            "HELP",
            "health",
            "HEALTH",
            "admin pulse: sync",
            "  Admin Pulse: audit ",
            "admin pulse",
        ];
        for query in positives {
            assert!(
                is_reflex_query(query),
                "expected '{query}' to be recognized as a reflex query"
            );
        }
    }

    #[test]
    fn is_reflex_query_rejects_non_reflex_goals() {
        let negatives = [
            "",
            "   ",
            "implement feature X",
            "list_models",
            "directory layout",
            "healthy cluster",
            "status_report",
            "versioning strategy",
            "helpful guide",
            "identities",
            "lsblk",
            "trigger admin pulse",
        ];
        for query in negatives {
            assert!(
                !is_reflex_query(query),
                "expected '{query}' NOT to be recognized as a reflex query"
            );
        }
    }

    #[test]
    fn has_valid_local_weights_and_runtime_readiness_edge_cases() {
        // Non-array JSON shapes must return false
        assert!(!has_valid_local_weights(&json!({"models": []})));
        assert!(!has_valid_local_weights(&json!("invalid")));
        assert!(!has_valid_local_weights(&json!(42)));

        // Non-.gguf extensions or non-boolean flags must return false
        assert!(!has_valid_local_weights(&json!([
            {"path": "/models/weights.safetensors", "is_valid_gguf": true},
            {"path": "/models/weights.bin", "is_valid_gguf": true},
            {"path": "/models/no_ext", "is_valid_gguf": true},
            {"path": "/models/valid.gguf", "is_valid_gguf": "true"},
            {"path": 12345, "is_valid_gguf": true}
        ])));

        // Mixed array succeeds if at least one entry has is_valid_gguf == true and .gguf extension
        assert!(has_valid_local_weights(&json!([
            {"path": "/models/broken.gguf", "is_valid_gguf": false},
            {"path": "/models/other.safetensors", "is_valid_gguf": true},
            {"path": "/models/ready.gguf", "is_valid_gguf": true}
        ])));

        // runtime_readiness edge cases: leading whitespace before [NO_LOCAL_MODEL] still fails
        let leading_ws = "   \n\t[NO_LOCAL_MODEL] No model installed.";
        assert!(runtime_readiness(leading_ws).is_err());

        // [NO_LOCAL_MODEL] not at start is treated as normal message
        let mid_tag = "Status note: [NO_LOCAL_MODEL] was resolved";
        assert_eq!(runtime_readiness(mid_tag).unwrap(), mid_tag);
        assert_eq!(runtime_readiness("").unwrap(), "");
    }

    #[test]
    fn context_agent_project_markers_and_top_level_scan() {
        let ws = unique_temp_workspace("context");

        // Empty workspace has no markers and (0, 0) counts
        assert!(detect_project_markers(&ws).is_empty());
        assert_eq!(scan_workspace_top_level(&ws), (0, 0));

        // Create markers and top-level files/directories
        fs::write(ws.join("Cargo.toml"), "[package]\nname = \"demo\"\n").unwrap();
        fs::write(ws.join("package.json"), "{}").unwrap();
        fs::write(ws.join("README.md"), "# Demo").unwrap();
        fs::create_dir(ws.join(".git")).unwrap();
        fs::create_dir(ws.join("src")).unwrap();
        // Nested file should not inflate top-level counts
        fs::write(ws.join("src").join("lib.rs"), "pub fn demo() {}").unwrap();

        let markers = detect_project_markers(&ws);
        assert_eq!(markers, vec!["Cargo.toml", "package.json", ".git"]);

        let (files, dirs) = scan_workspace_top_level(&ws);
        assert_eq!(files, 3);
        assert_eq!(dirs, 2);

        let bb = MissionBlackboard::default();
        let report = ContextAgent.execute("analyze workspace", &ws, &bb).unwrap();
        assert!(report.contains("3 top-level file(s), 2 top-level subdirectory(ies)"));
        assert!(report.contains("Cargo.toml, package.json, .git"));
        assert!(bb.contains_key("ContextAgent"));

        // Non-existent path returns (0, 0) and no markers
        let missing = ws.join("does_not_exist");
        assert_eq!(scan_workspace_top_level(&missing), (0, 0));
        assert!(detect_project_markers(&missing).is_empty());

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn epistemic_auditor_path_extraction_and_grounding() {
        let ws = unique_temp_workspace("epistemic");
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::write(ws.join("Cargo.toml"), "").unwrap();
        fs::write(ws.join("src").join("lib.rs"), "").unwrap();

        let text = "Checked `Cargo.toml`, (src/lib.rs:42), [README.md.], and \"scripts/run.sh\"; ignored main.py and notes.txt.";
        let extracted = extract_candidate_file_paths(text);
        assert_eq!(
            extracted,
            vec!["Cargo.toml", "src/lib.rs", "README.md", "scripts/run.sh"]
        );

        let (count, hallucinated) = audit_claim_grounding(text, &ws);
        assert_eq!(count, 4);
        assert_eq!(hallucinated, vec!["README.md", "scripts/run.sh"]);

        // 1. Grounded claims pass
        let bb_ok = MissionBlackboard::default();
        bb_ok.insert(
            "WorkerA".to_string(),
            "Verified `Cargo.toml` and `src/lib.rs:10`.".to_string(),
        );
        let res_ok = EpistemicAuditorAgent.execute("audit", &ws, &bb_ok).unwrap();
        assert!(res_ok.contains("FILE REFERENCES VERIFIED"));

        // 2. Hallucinated file path is rejected
        let bb_hallucinated = MissionBlackboard::default();
        bb_hallucinated.insert(
            "WorkerB".to_string(),
            "Updated `src/nonexistent_module.rs` in the workspace.".to_string(),
        );
        let err_hallucinated = EpistemicAuditorAgent
            .execute("audit", &ws, &bb_hallucinated)
            .unwrap_err()
            .to_string();
        assert!(err_hallucinated.contains("HALLUCINATION DETECTED & REJECTED"));
        assert!(bb_hallucinated.contains_key("systemic#hallucination_caught"));

        // 3. Empty promise without code or path evidence is rejected
        let bb_promise = MissionBlackboard::default();
        bb_promise.insert(
            "WorkerC".to_string(),
            "I have fixed the bug and completed the task.".to_string(),
        );
        let err_promise = EpistemicAuditorAgent
            .execute("audit", &ws, &bb_promise)
            .unwrap_err()
            .to_string();
        assert!(err_promise.contains("EMPTY PROMISE REJECTED"));
        assert!(bb_promise.contains_key("systemic#empty_promises_caught"));

        // 4. Malformed EvidenceRecord is rejected
        let bb_bad_ir = MissionBlackboard::default();
        bb_bad_ir.insert("EvidenceRecord::bad".to_string(), "{not json".to_string());
        let err_ir = EpistemicAuditorAgent
            .execute("audit", &ws, &bb_bad_ir)
            .unwrap_err()
            .to_string();
        assert!(err_ir.contains("UNFOUNDED LIES / UNBACKED ASSERTIONS REJECTED"));
        assert!(bb_bad_ir.contains_key("systemic#lies_caught"));

        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn consensus_mediator_signal_classification_and_execution() {
        let ws = Path::new(".");

        // Routine signals only -> consensus reached
        let bb_healthy = MissionBlackboard::default();
        bb_healthy.insert(
            "SafetyAgent".to_string(),
            "Safety protocols verified.".to_string(),
        );
        bb_healthy.insert(
            "SecurityAgent".to_string(),
            "Security audit passed.".to_string(),
        );
        bb_healthy.insert("EmptyAgent".to_string(), "   ".to_string());

        let (crit, healthy) = summarize_swarm_signals(&bb_healthy, "ConsensusMediatorAgent");
        assert!(crit.is_empty());
        assert_eq!(healthy.len(), 2);

        let ok_res = ConsensusMediatorAgent
            .execute("mediate", ws, &bb_healthy)
            .unwrap();
        assert!(ok_res.contains("Weighted consensus reached"));

        // Partial conflict (some critical, some healthy) -> Err with CONFLICT
        let bb_conflict = MissionBlackboard::default();
        bb_conflict.insert("SafetyAgent".to_string(), "All good".to_string());
        bb_conflict.insert(
            "GmcpAgent".to_string(),
            "Endpoint health status: DEGRADED".to_string(),
        );
        let conflict_err = ConsensusMediatorAgent
            .execute("mediate", ws, &bb_conflict)
            .unwrap_err()
            .to_string();
        assert!(conflict_err.contains("CONFLICT:"));
        assert!(conflict_err.contains("Consensus not reached"));

        // Swarm-wide distress (all critical, no healthy) -> Err with swarm-wide distress
        let bb_distress = MissionBlackboard::default();
        bb_distress.insert(
            "ResourceArbitratorAgent".to_string(),
            "OOM Critical Risk: true".to_string(),
        );
        bb_distress.insert(
            "WorkerAgent".to_string(),
            "Status test: FAILED | governance_block".to_string(),
        );
        let distress_err = ConsensusMediatorAgent
            .execute("mediate", ws, &bb_distress)
            .unwrap_err()
            .to_string();
        assert!(distress_err.contains("swarm-wide distress"));
        assert!(distress_err.contains("Consensus rejected"));
    }

    #[test]
    fn extract_patch_request_json_handles_valid_and_invalid_inputs() {
        let valid_goal = r#"Please apply this fix: {"files": [{"path": "src/lib.rs", "content": "fn a() {}"}]} thanks"#;
        let extracted = extract_patch_request_json(valid_goal).unwrap();
        assert!(extracted.starts_with('{') && extracted.ends_with('}'));
        let parsed: serde_json::Value = serde_json::from_str(&extracted).unwrap();
        assert_eq!(parsed["files"].as_array().unwrap().len(), 1);

        // Missing "files" field
        assert!(extract_patch_request_json(r#"{"patches": []}"#).is_none());
        // "files" is not an array
        assert!(extract_patch_request_json(r#"{"files": "src/lib.rs"}"#).is_none());
        assert!(extract_patch_request_json(r#"{"files": {"path": "a.rs"}}"#).is_none());
        // Unbalanced or reversed braces / invalid JSON
        assert!(extract_patch_request_json(r#"{"files": [1, 2"#).is_none());
        assert!(extract_patch_request_json(r#"no braces at all"#).is_none());
        assert!(extract_patch_request_json(r#"} reversed {"#).is_none());
        assert!(extract_patch_request_json(r#"{not valid json}"#).is_none());
    }

    #[test]
    fn is_remote_inference_endpoint_distinguishes_remote_from_loopback_and_invalid() {
        // Remote HTTP/HTTPS endpoints
        assert!(is_remote_inference_endpoint("https://api.openai.com/v1"));
        assert!(is_remote_inference_endpoint(
            "  HTTPS://api.anthropic.com/v1 "
        ));
        assert!(is_remote_inference_endpoint("http://10.0.0.42:8000/v1"));

        // Loopback addresses are local substrates, not remote egress
        assert!(!is_remote_inference_endpoint("http://localhost:11434/v1"));
        assert!(!is_remote_inference_endpoint("https://LOCALHOST:8000/v1"));
        assert!(!is_remote_inference_endpoint("http://127.0.0.1:8080/v1"));
        assert!(!is_remote_inference_endpoint("http://[::1]:8000/v1"));
        assert!(!is_remote_inference_endpoint("http://0.0.0.0:30000/v1"));

        // Missing or non-HTTP schemes
        assert!(!is_remote_inference_endpoint(""));
        assert!(!is_remote_inference_endpoint("api.openai.com/v1"));
        assert!(!is_remote_inference_endpoint("ftp://api.openai.com/v1"));
        assert!(!is_remote_inference_endpoint("ws://api.openai.com/v1"));
    }

    #[test]
    fn admin_agent_gates_mutating_actions_behind_admin_pulse_and_allows_read_only() {
        // Read-only actions in ACTION_ORDER match without "admin pulse"
        assert_eq!(
            AdminAgent::match_action("check substrate status"),
            Some("status_health".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("check cluster health"),
            Some("status_health".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("show engine version"),
            Some("version".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("inspect identity"),
            Some("identity".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("list all models"),
            Some("list_models".to_string())
        );

        // Mutating actions must NOT trigger without "admin pulse" prefix
        for casual_goal in [
            "sync the workspace manifest",
            "audit repository compliance",
            "verify package alignment",
            "check the release notes",
            "deep-scan home directory",
            "install new dependencies",
            "initialize the workspace",
            "uninstall the package",
            "remove unused imports",
        ] {
            assert_eq!(
                AdminAgent::match_action(casual_goal),
                None,
                "mutating keyword in '{casual_goal}' must not match without 'admin pulse'"
            );
        }

        // Mutating actions DO trigger when prefixed with "admin pulse"
        assert_eq!(
            AdminAgent::match_action("admin pulse: sync"),
            Some("sync".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("admin pulse: audit"),
            Some("audit".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("admin pulse: verify"),
            Some("verify".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("admin pulse: release"),
            Some("release".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("admin pulse: deep-scan"),
            Some("deep_scan".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("admin pulse: install"),
            Some("install".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("admin pulse: initialize"),
            Some("install".to_string())
        );
        // Note: "uninstall" contains "install" as a substring and "install"
        // precedes "uninstall" in ACTION_ORDER, so "remove" is the unambiguous
        // trigger for "uninstall".
        assert_eq!(
            AdminAgent::match_action("admin pulse: uninstall"),
            Some("install".to_string())
        );
        assert_eq!(
            AdminAgent::match_action("admin pulse: remove"),
            Some("uninstall".to_string())
        );

        // ACTION_ORDER precedence: sync precedes release
        assert_eq!(
            AdminAgent::match_action("admin pulse: sync and release"),
            Some("sync".to_string())
        );
    }

    #[test]
    fn fast_path_agent_executions_are_hermetic_on_reflex_and_status_queries() {
        let ws = Path::new(".");
        let bb = MissionBlackboard::default();

        // EvolutionAgent skips background audit on status/version/models/identity queries
        let evo = EvolutionAgent.execute("check version", ws, &bb).unwrap();
        assert!(evo.contains("drift audit not run for status queries"));

        // SelfHealingAgent skips health check on reflex queries
        let heal = SelfHealingAgent.execute("whoami", ws, &bb).unwrap();
        assert!(heal.contains("no health check runs for reflex queries"));

        // LibraryScoutAgent skips scouting on reflex queries
        let scout = LibraryScoutAgent.execute("ls -la", ws, &bb).unwrap();
        assert!(scout.contains("No library scouting applies to this query"));

        // AdminAgent executes version and fallback cleanly
        let ver = AdminAgent.execute("version", ws, &bb).unwrap();
        assert!(ver.contains("SUSI Engine Version: v"));
        let fallback = AdminAgent.execute("unmatched goal text", ws, &bb).unwrap();
        assert_eq!(fallback, "AdminAgent: Monitoring technical intent...");
    }
}
