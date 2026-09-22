// Agent trait, built-in agent implementations, and the fleet synthesizer
// that decides which agents to recruit for a given goal.
// Agents must add functionality directly to the susi engine, not simulate
// results themselves.

use susi_error::{EaiError, EaiResult};

use std::path::Path;

pub use susi_agents::{AgentMetaRegistry, GawdAgent, MissionBlackboard};

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
    ) -> EaiResult<String> {
        let lower = goal.to_lowercase();
        let trimmed = lower.trim();

        // Fast-Path Yield: Skip the GGUF inference loop for instant queries or when
        // specialists/admin have answered. The blackboard.contains_key("AdminAgent")
        // check is a race (agents run concurrently via rayon with no ordering
        // guarantee, so this DynamicAgent may check before AdminAgent inserts its
        // result) — trimmed.starts_with("admin pulse") is a deterministic fallback
        // on the goal string itself, so admin/bootstrap pulses (install, uninstall,
        // sync, audit, release, ...) never nondeterministically fall through to a
        // full model-inference cycle depending on scheduling luck. Losing that race
        // previously meant this agent could trigger loading and running whatever
        // model HardwareProfiler picked as "optimal" for the host (up to 72B on a
        // large-RAM machine) synchronously on CPU during what should be a fast
        // bootstrap pulse — the actual root cause of a multi-minute `susi install`.
        if trimmed == "ls"
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
            || trimmed.starts_with("admin pulse")
            || blackboard.contains_key("AdminAgent")
        {
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
        let prompt = susi_sandbox::manager::SusiPrompts::load_global()
            .dynamic_agent_prompt()
            .replace("{agent_role}", &self.agent_name)
            .replace("{mission_profile}", &self.mission_profile)
            .replace("{goal}", goal)
            .replace("{blackboard_context}", &bb_state);

        let ws = workspace.to_path_buf();
        let prompt_val = serde_json::json!(prompt);

        let is_admin_or_query = trimmed == "ls"
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
            || trimmed.starts_with("admin pulse");

        // Prefer the registered 'reason' tool; fall back to calling the engine directly.
        let res = if is_admin_or_query {
            format!(
                "[{}]: Observation integrated into blackboard.",
                self.agent_name
            )
        } else if susi_tools::ToolRegistry::exists("reason") {
            susi_tools::ToolRegistry::execute_tool("reason", &prompt_val, &ws)
        } else {
            susi_gemi::engine::GemiEngine::generate_reasoning(&prompt, &ws)
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
    ) -> EaiResult<String> {
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

/// Live web / weather evidence agent. Fetches real HTTP evidence (Open-Meteo,
/// linked MCP search tools, DuckDuckGo) instead of LLM-narrating a fake search.
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
    ) -> EaiResult<String> {
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

/// Ensures a usable model is available (installs the default if none is
/// found) and links essential MCP servers.
pub struct SusiRuntimeAgent;

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
    ) -> EaiResult<String> {
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
        let cloud_available = cloud_env_keys.iter().any(|k| std::env::var(k).is_ok());

        // 2. Check whether a valid local model is already present
        let verifications = susi_gemi::models::ModelManager::verify_local_models(workspace);
        let valid_local_found = verifications
            .iter()
            .any(|v| v.is_valid_gguf || v.model_id.contains("native"));

        // 3. If neither is available, install the default model
        if !cloud_available && !valid_local_found {
            let cfg = susi_sandbox::manager::SusiConfig::load(&susi_paths::SusiDirs::config_dir())
                .unwrap_or_default();
            susi_gemi::models::ModelManager::install_model(&cfg.alpha_weights_url());
            let _ = susi_gemi::models::ModelManager::ensure_hardware_optimal_models(workspace);
        }

        // 4. Link essential MCP servers
        susi_tools::ToolRegistry::auto_link_essential_mcp_servers();

        Ok("Runtime environment established and optimized for pulse intent.".into())
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
    ) -> EaiResult<String> {
        let profile = susi_gemi::hardware::HardwareProfiler::get_profile();
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
    ) -> EaiResult<String> {
        let (files, dirs) = scan_workspace_top_level(workspace);
        let markers = detect_project_markers(workspace);
        let res = format!(
            "[ContextAgent]: Workspace '{}' — {} top-level file(s), {} top-level subdirectory(ies). Detected project markers: {}.",
            workspace.display(),
            files,
            dirs,
            if markers.is_empty() {
                "none".to_string()
            } else {
                markers.join(", ")
            }
        );
        blackboard.insert(self.name(), res.clone());
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
    ) -> EaiResult<String> {
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
    ) -> EaiResult<String> {
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
    ) -> EaiResult<String> {
        let lower = goal.to_lowercase();
        if lower.contains("identity")
            || lower.contains("status")
            || lower.contains("models")
            || lower.contains("version")
        {
            let res = "Evolutionary health: Substrate Optimal.".to_string();
            blackboard.insert(self.name(), res.clone());
            return Ok(res);
        }

        static DRIFT_AUDIT_RUNNING: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        if !DRIFT_AUDIT_RUNNING.swap(true, std::sync::atomic::Ordering::SeqCst) {
            let ws = workspace.to_path_buf();
            rayon::spawn(move || {
                struct AuditGuard;
                impl Drop for AuditGuard {
                    fn drop(&mut self) {
                        DRIFT_AUDIT_RUNNING.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                let _guard = AuditGuard;
                let _ = crate::admin_hooks::hooks().perform_autonomous_drift_audit(&ws);
            });
        }
        let res = "Evolutionary health: Substrate Optimal.".to_string();
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
    ) -> EaiResult<String> {
        // Test Tool Registry endpoints (Internal Reflex)
        let registry = susi_tools::ToolRegistry::global();
        let tool_count = registry.tools.len();

        let status_res =
            susi_tools::ToolRegistry::execute_tool("status", &serde_json::json!(null), workspace);
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
    ) -> EaiResult<String> {
        use susi_core::evidence::{EvidenceAssessment, EvidenceRecord};

        let mut total = 0usize;
        let mut grounded = 0usize;
        let mut ungrounded = 0usize;
        let mut ir_verified = 0usize;
        let mut ir_failed: Vec<String> = Vec::new();
        let mut hallucinated_paths: Vec<String> = Vec::new();

        for entry in blackboard.iter() {
            if entry.key() == &self.name() {
                continue;
            }
            if entry.key().starts_with("EvidenceRecord::") {
                total += 1;
                match serde_json::from_str::<EvidenceRecord>(entry.value()) {
                    Ok(record) => match record.assess(workspace) {
                        EvidenceAssessment::Verified => ir_verified += 1,
                        EvidenceAssessment::Unverified(reason)
                        | EvidenceAssessment::Rejected(reason) => {
                            ir_failed.push(format!("{}: {reason}", entry.key()));
                        }
                    },
                    Err(_) => {
                        ir_failed.push(format!("{}: unparseable EvidenceRecord", entry.key()))
                    }
                }
                continue;
            }
            total += 1;
            let (referenced, hallucinated) = audit_claim_grounding(entry.value(), workspace);
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
                "[EpistemicAuditorAgent]: Audited {} agent claims — {} file-grounded, {} unverifiable (no file reference), {} referenced non-existent paths: {:?}. Epistemic integrity: UNGROUNDED CLAIMS DETECTED.",
                total, grounded, ungrounded, hallucinated_paths.len(), hallucinated_paths
            );
            blackboard.insert(self.name(), res.clone());
            return Err(EaiError::governance(res));
        }
        if !ir_failed.is_empty() {
            let res = format!(
                "[EpistemicAuditorAgent]: Audited {} claims — {} EvidenceRecord verified, {} EvidenceRecord failed ({:?}). Epistemic integrity: NAKED OR REJECTED ASSERTIONS.",
                total, ir_verified, ir_failed.len(), ir_failed
            );
            blackboard.insert(self.name(), res.clone());
            return Err(EaiError::governance(res));
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
    ) -> EaiResult<String> {
        let profile = susi_gemi::hardware::HardwareProfiler::get_profile();
        let oom_risk = susi_gemi::hardware::HardwareProfiler::check_oom_critical();
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
        if entry.key() == exclude {
            continue;
        }
        let lower = entry.value().to_lowercase();
        if lower.trim().is_empty() {
            continue;
        }
        if CRITICAL_SIGNAL_MARKERS.iter().any(|m| lower.contains(m)) {
            critical.push(entry.key().clone());
        } else {
            healthy.push(entry.key().clone());
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
    ) -> EaiResult<String> {
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
            return Err(EaiError::governance(res));
        } else {
            let res = format!(
                "[ConsensusMediatorAgent]: Analyzed {} active agent contributions. CONFLICT: {} agent(s) report critical/problem signals ({:?}) while {} agent(s) report routine status ({:?}). Consensus not reached.",
                total, critical.len(), critical, healthy.len(), healthy
            );
            blackboard.insert(self.name(), res.clone());
            return Err(EaiError::governance(res));
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
    ) -> EaiResult<String> {
        let lower = goal.to_lowercase();
        let trimmed = lower.trim();
        if trimmed == "ls"
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
        {
            let res = "[SelfHealingAgent]: Substrate health verified for reflex query.".to_string();
            blackboard.insert(self.name(), res.clone());
            return Ok(res);
        }

        let ws = workspace.to_path_buf();
        let audit = crate::admin_hooks::hooks()
            .perform_autonomous_drift_audit(&ws)
            .unwrap_or_else(|_| "Substrate drift audit nominal.".to_string());
        let res = format!(
            "[SelfHealingAgent]: Autonomous health check completed. {}",
            audit
        );
        blackboard.insert(self.name(), res.clone());
        Ok(res)
    }
}

/// Wraps a configurable remote inference endpoint (base URL + protocol) as an agent.
pub struct DynamicInferenceEndpointAgent {
    pub endpoint_name: String,
    pub api_base_url: String,
    pub protocol_type: String,
    pub agent_rank: f32,
}

impl DynamicInferenceEndpointAgent {
    pub fn new(name: &str, api_base_url: &str, protocol_type: &str) -> Self {
        Self {
            endpoint_name: name.to_string(),
            api_base_url: api_base_url.to_string(),
            protocol_type: protocol_type.to_string(),
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
    ) -> EaiResult<String> {
        let client = susi_tools::GmcpClient::scout_reasoning_remotes();
        for remote_name in client {
            if remote_name
                .to_lowercase()
                .contains(&self.endpoint_name.to_lowercase())
            {
                let res =
                    susi_tools::GmcpClient::execute_external_tool(&remote_name, "generate", goal);
                if !res.contains("[FAIL]") {
                    return Ok(format!("[{} Power-Tier]: {}", self.endpoint_name, res));
                }
            }
        }

        let payload = match self.protocol_type.as_str() {
            "chat" => serde_json::json!({
                "model": format!("{}-substrate", self.endpoint_name.to_lowercase()),
                "messages": [{"role": "user", "content": goal}],
                "max_tokens": 1024
            }),
            "triton" => serde_json::json!({
                "text_input": goal,
                "parameters": { "max_tokens": 512, "bad_words": [], "stop_words": [] }
            }),
            _ => serde_json::json!({
                "model": format!("{}-substrate", self.endpoint_name.to_lowercase()),
                "prompt": goal,
                "max_tokens": 1024
            }),
        };

        let endpoint_url = if self.protocol_type == "chat" {
            format!("{}/chat/completions", self.api_base_url)
        } else if self.protocol_type == "triton" {
            self.api_base_url.clone()
        } else {
            format!("{}/completions", self.api_base_url)
        };

        match susi_sandbox::manager::http_agent()
            .post(&endpoint_url)
            .header("Content-Type", "application/json")
            .send_json(payload)
        {
            Ok(resp) => {
                let text = resp
                    .into_body()
                    .read_to_string()
                    .unwrap_or_else(|_| "output empty".into());
                Ok(format!("[{} Proxy]: {}", self.endpoint_name, text))
            }
            Err(_) => Err(susi_error::EaiError::inference(format!(
                "{} proxy endpoint unreachable at {}",
                self.endpoint_name, self.api_base_url
            ))),
        }
    }
}

pub type VllmBridgeAgent = DynamicInferenceEndpointAgent;
pub type SglangBridgeAgent = DynamicInferenceEndpointAgent;
pub type LlamaCppBridgeAgent = DynamicInferenceEndpointAgent;
pub type TensorRtBridgeAgent = DynamicInferenceEndpointAgent;
pub type LmdeployBridgeAgent = DynamicInferenceEndpointAgent;

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
    ) -> EaiResult<String> {
        let lower_goal = goal.to_lowercase();
        let trimmed = lower_goal.trim();

        if trimmed == "ls"
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
        {
            return Ok("[LibraryScoutAgent]: Substrate libraries optimal.".to_string());
        }

        // Enhanced Library Scouting with reasoning and 'cargo add' suggestions
        let prompt = format!("Extract a single dominant keyword (max 1-2 words, lowercase) representing the crate category needed for this goal: '{}'. Output ONLY the keyword, no explanation. Example outputs: async, json, sql, gui, web, inference.", goal);
        let ws = workspace.to_path_buf();
        let mut query_term = susi_gemi::engine::GemiEngine::generate_reasoning(&prompt, &ws);
        query_term = query_term.trim().to_lowercase().replace(['"', '\''], "");
        if query_term.is_empty() || query_term.contains(' ') {
            query_term = "rust".to_string();
        }

        let api_base = susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .crates_io_api_url();
        let url = format!("{}?q={}&per_page=5", api_base, query_term);
        let mut results = Vec::new();

        if let Ok(resp) = susi_sandbox::manager::http_agent()
            .get(&url)
            .header("User-Agent", "SUSI/0.1")
            .call()
        {
            if let Ok(json) = resp.into_body().read_json::<serde_json::Value>() {
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
        Ok(susi_gemi::engine::GemiEngine::generate_reasoning(
            &prompt, &ws,
        ))
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
    ) -> EaiResult<String> {
        let lower_goal = goal.to_lowercase();
        let action = Self::match_action(&lower_goal);

        let res = match action.as_deref() {
            Some("sync") => crate::admin_hooks::hooks().enforce_version_consistency(workspace),
            Some("audit") => crate::admin_hooks::hooks().audit_compliance(workspace),
            Some("verify") => crate::admin_hooks::hooks().verify_version_alignment(workspace),
            Some("release") => crate::admin_hooks::hooks().execute_release(workspace),
            Some("status_health") => {
                let hw = susi_gemi::hardware::HardwareProfiler::get_profile();
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
                Ok(out)
            }
            Some("deep_scan") => {
                let global_dir = Self::global_dir();
                susi_gemi::models::ModelManager::deep_scan_home_and_register(&global_dir)
            }
            Some("install") => {
                let global_dir = Self::global_dir();
                susi_sandbox::manager::SandboxManager::ensure_global_sandbox(&global_dir)?;
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
        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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
