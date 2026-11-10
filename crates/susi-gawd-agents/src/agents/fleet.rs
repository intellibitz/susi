// Agent trait, built-in agent implementations, and the fleet synthesizer
// that decides which agents to recruit for a given goal.
// Agents must add functionality directly to the susi engine, not simulate
// results themselves.

use crate::susi_error::EaiResult;

use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::susi_core::{
    AgentMetaRegistry, AgentProfile, DiscoverableAsset, GawdAgent, GawdAgentInfo,
    HighDensityContextStore, MissionBlackboard, SwarmBlackboard,
};

use super::native::*;

// `AgentMetaRegistry`'s data-persistence methods (list/register/re-rank)
// live in `susi_agents::registry` now - it's what `gemi`/`gmcp` actually
// need, and it has no dependency on any concrete agent struct. These two
// functions construct concrete agent structs below, so they stay here in
// `gawd` as free functions rather than methods on the (now cross-crate)
// `AgentMetaRegistry` type; nothing outside `gawd` ever called them.
use crate::susi_core::registry::DynamicServiceRegistry;
use std::sync::OnceLock;

pub fn agent_registry() -> &'static DynamicServiceRegistry {
    static REGISTRY: OnceLock<DynamicServiceRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let registry = DynamicServiceRegistry::new();
        registry.register_factory("DevOpsAgent", || {
            Arc::new(Arc::new(DevOpsAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("SearchAgent", || {
            Arc::new(Arc::new(SearchAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("SusiRuntimeAgent", || {
            Arc::new(Arc::new(SusiRuntimeAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("HardwareAgent", || {
            Arc::new(Arc::new(HardwareAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("SafetyAgent", || {
            Arc::new(Arc::new(SafetyAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("SecurityAgent", || {
            Arc::new(Arc::new(SecurityAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("EvolutionAgent", || {
            Arc::new(Arc::new(EvolutionAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("GmcpAgent", || {
            Arc::new(Arc::new(GmcpAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("EpistemicAuditorAgent", || {
            Arc::new(Arc::new(EpistemicAuditorAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("ResourceArbitratorAgent", || {
            Arc::new(Arc::new(ResourceArbitratorAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("ConsensusMediatorAgent", || {
            Arc::new(Arc::new(ConsensusMediatorAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("SelfHealingAgent", || {
            Arc::new(Arc::new(SelfHealingAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("LibraryScoutAgent", || {
            Arc::new(Arc::new(LibraryScoutAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("AdminAgent", || {
            Arc::new(Arc::new(AdminAgent) as Arc<dyn GawdAgent>)
        });
        registry.register_factory("ContextAgent", || {
            Arc::new(Arc::new(ContextAgent) as Arc<dyn GawdAgent>)
        });
        crate::external_peers::register_external_peer_factories(&registry);
        registry
    })
}

pub fn instantiate_native_agent(name: &str) -> Option<Arc<dyn GawdAgent>> {
    agent_registry()
        .instantiate::<std::sync::Arc<dyn GawdAgent>>(name)
        .map(|arc_of_arc| (*arc_of_arc).clone())
}
pub fn instantiate_agent(profile: &AgentProfile) -> Arc<dyn GawdAgent> {
    if let Some(agent) = instantiate_native_agent(&profile.name) {
        agent
    } else if profile.categories.iter().any(|c| c == "external") {
        // An external executor with no factory is a peer this host has not
        // installed: never stand in for it with a model-prompted persona.
        crate::external_peers::installed_or_unavailable_peer(&profile.name)
    } else {
        Arc::new(DynamicAgent {
            agent_name: profile.name.clone(),
            mission_profile: profile.description.clone(),
            agent_rank: profile.base_rank,
        })
    }
}

/// Neural Agent Factory
/// Autonomously generates specialist agent profiles when capability gaps are detected.
pub struct NeuralAgentFactory;

impl NeuralAgentFactory {
    const MAX_NAME_LEN: usize = 64;
    const MAX_DESCRIPTION_LEN: usize = 300;
    const MAX_KEYWORD_LEN: usize = 40;
    const MAX_KEYWORDS: usize = 8;

    pub fn synthesize_specialist(goal: &str, workspace: &Path) -> EaiResult<AgentProfile> {
        // An empty goal carries no capability signal — a specialist
        // synthesized from nothing would be arbitrary. It also closes a
        // resource-exhaustion surface: an A2A caller sending an empty
        // message could otherwise trigger a full LLM generation per
        // request (~60s CPU-bound each).
        if goal.trim().is_empty() {
            return Err(crate::susi_error::EaiError::config(
                "cannot synthesize a specialist for an empty goal",
            ));
        }
        let prompts = crate::susi_sandbox::manager::SusiPrompts::load_global();
        let prompt = prompts.agent_factory_prompt().replace("{goal}", goal);

        let res = crate::susi_core::plane_bus::gemi::GemiEngine::generate_reasoning_deep(
            &prompt, workspace,
        );
        let profile: AgentProfile = serde_json::from_str(&res).map_err(|e| {
            crate::susi_error::EaiError::protocol(format!(
                "Neural Agent Synthesis Failed: {}. Raw: {}",
                e, res
            ))
        })?;

        Self::sanitize_profile(profile, workspace)
    }

    /// Bounds and governance-checks an LLM-synthesized `AgentProfile` before
    /// it can ever be persisted to `~/.susi/agent_registry.json` or fed back
    /// into future missions' prompts. The goal text driving synthesis is
    /// untrusted (it can come from an unauthenticated GEMI REST caller), and
    /// the LLM's JSON output is not sanitized by construction, so this is a
    /// real persistent-injection surface, not a theoretical one:
    /// `description` becomes `{mission_profile}` in `dynamic_agent_prompt`
    /// for every future mission that recruits this agent, verbatim.
    fn sanitize_profile(mut profile: AgentProfile, workspace: &Path) -> EaiResult<AgentProfile> {
        // `is_core` agents are auto-recruited into every future mission
        // unconditionally (GawdAgentFleet::synthesize_fleet step 1) — that
        // flag must only ever come from the bundled, trusted
        // agents.default.json, never from LLM output, or a single poisoned
        // synthesis would inject into every subsequent mission forever.
        profile.is_core = false;

        profile
            .name
            .retain(|c| c.is_ascii_alphanumeric() || c == '_');
        profile.name.truncate(Self::MAX_NAME_LEN);
        if profile.name.is_empty() {
            return Err(crate::susi_error::EaiError::protocol(
                "Neural Agent Synthesis rejected: empty/invalid agent name after sanitization"
                    .to_string(),
            ));
        }

        profile.description.truncate(Self::MAX_DESCRIPTION_LEN);
        profile.categories.truncate(Self::MAX_KEYWORDS);
        for c in profile.categories.iter_mut() {
            c.truncate(Self::MAX_KEYWORD_LEN);
        }
        profile.semantic_anchors.truncate(Self::MAX_KEYWORDS);
        for a in profile.semantic_anchors.iter_mut() {
            a.truncate(Self::MAX_KEYWORD_LEN);
        }
        profile.base_rank = profile.base_rank.clamp(0.1, 1.0);

        // Reuse the same governance detectors that gate every tool call
        // (Mandates 36-39): a synthesized description carrying a destructive
        // command pattern or secret-token shape must never be persisted.
        crate::safety::SafetyDetector::audit_action(
            "AGENT_SYNTHESIS",
            &profile.description,
            workspace,
        )
        .map_err(|e| {
            crate::susi_error::EaiError::governance(format!(
                "Neural Agent Synthesis rejected by SafetyAgent: {}",
                e
            ))
        })?;
        crate::security::SecurityDetector::audit_action(
            "AGENT_SYNTHESIS",
            &profile.description,
            workspace,
        )
        .map_err(|e| {
            crate::susi_error::EaiError::governance(format!(
                "Neural Agent Synthesis rejected by SecurityAgent: {}",
                e
            ))
        })?;

        Ok(profile)
    }
}

pub struct GawdAgentFleet;

static THROTTLE_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl GawdAgentFleet {
    pub fn throttle_concurrency(active: bool) {
        THROTTLE_ACTIVE.store(active, std::sync::atomic::Ordering::Relaxed);
    }

    /// How high a goal/agent semantic-similarity score must be before an
    /// existing agent counts as "covers this capability" for Neural Agent
    /// Synthesis purposes, per the configured `trust_level`. Pure function
    /// (no I/O) so the mapping is directly unit-testable.
    fn synthesis_similarity_threshold(trust_level: &str) -> f32 {
        match trust_level.to_lowercase().as_str() {
            "conservative" => 0.7,
            "autonomous" => 0.2,
            _ => 0.4, // "balanced" (bundled default) and any unrecognized level
        }
    }

    pub fn get_max_concurrent_agents() -> usize {
        let hw = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();

        let base_limit = if THROTTLE_ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
            // Load Shedding: Reduce to 25% capacity if system is under stress
            (cfg.max_concurrent_agents() / 4).max(1)
        } else {
            cfg.max_concurrent_agents()
        };

        // Mandate: Never cause OOM. Cap at 90% utilization.
        // Heuristic: Each agent requires ~512MB RAM for context/inference overhead.
        let ram_based_limit = (hw.available_ram_gb * 1024 / 512).max(1);
        ram_based_limit.min(base_limit)
    }

    /// Whether `goal` is either a substrate meta-command (identity/status/
    /// models/version/admin/ls/whoami - handled by dedicated fast paths
    /// elsewhere) or an ordinary natural-language question. Either way,
    /// spending a full LLM generation to invent a brand-new specialist
    /// agent for it (`synthesize_fleet`'s Neural Agent Synthesis step) is
    /// pure waste: a generalist agent (`UniversalReasoner`, or whatever
    /// matched via routing/semantic similarity) already covers this class
    /// of intent, and the synthesized agent would just route back to the
    /// same generic LLM reasoning anyway.
    ///
    /// Verified live: "What is the capital of France?" (no substring
    /// overlap with the meta-command list) fell through the old
    /// meta-command-only gate and triggered *two* full ~256-token "invent
    /// a specialist agent" generations (one per retry-loop iteration in
    /// `ama.rs::solve_internal`) before the mission ever got to answering
    /// the actual one-word question - over 30 seconds and 3-4 separate
    /// model calls for something `UniversalReasoner` alone answered
    /// correctly in 617ms once it was actually asked. This was duplicated
    /// (and had silently drifted slightly out of sync) across three call
    /// sites in `agents.rs`/`amas.rs`; consolidated here so a future
    /// addition to the list only needs to happen once.
    pub fn is_meta_or_simple_query(goal: &str) -> bool {
        crate::goal_shape::is_meta_or_simple_query(goal)
    }

    /// Substrate meta-commands only — see [`crate::goal_shape::is_meta_command`].
    pub fn is_meta_command(goal: &str) -> bool {
        crate::goal_shape::is_meta_command(goal)
    }

    /// Neural Fleet Synthesizer: Dynamically decides which agents are required for a mission.
    /// Uses semantic centroids to match agents.
    pub fn synthesize_fleet(goal: &str, workspace: &Path) -> Vec<Arc<dyn GawdAgent>> {
        let mut fleet: Vec<Arc<dyn GawdAgent>> = vec![];
        let lower_goal = goal.to_lowercase();

        let is_query_or_admin = Self::is_meta_or_simple_query(goal);

        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let routing = cfg.agent_routing();

        let max_agents = Self::get_max_concurrent_agents();
        let registry = AgentMetaRegistry::global();
        let available_agents = registry.list_agents();

        // External executor peers (`categories` contains "external") run
        // managed CLIs / paid APIs with real side effects — a generic
        // inference or chat goal must never fan out to them. They are
        // recruited only on explicit invocation: the peer named in the
        // goal or operator-configured `agent_routing` keywords.
        let is_external_executor =
            |agent: &AgentProfile| agent.categories.iter().any(|c| c == "external");
        let explicitly_requested = |agent: &AgentProfile| -> bool {
            let short = agent.name.to_lowercase().replace("agent", "");
            (!short.is_empty() && lower_goal.contains(&short))
                || routing
                    .get(&agent.name)
                    .is_some_and(|keys| keys.iter().any(|k| lower_goal.contains(k)))
        };

        // 1. Core Native Agents & Matched Handlers
        for agent in &available_agents {
            if agent.is_core {
                if !fleet.iter().any(|a| a.name() == agent.name) {
                    fleet.push(instantiate_agent(agent));
                }
                continue;
            }

            let mut should_add = false;
            // Config routing overrides
            if let Some(keys) = routing.get(&agent.name) {
                if keys.iter().any(|k| lower_goal.contains(k)) {
                    should_add = true;
                }
            } else if is_external_executor(agent) {
                should_add = explicitly_requested(agent);
            } else if agent
                .semantic_anchors
                .iter()
                .any(|anchor| lower_goal.contains(anchor))
                || agent
                    .categories
                    .iter()
                    .any(|category| lower_goal.contains(category))
                || lower_goal.contains(&agent.name.to_lowercase().replace("agent", ""))
            {
                should_add = true;
            }

            if should_add && !fleet.iter().any(|a| a.name() == agent.name) {
                fleet.push(instantiate_agent(agent));
            }
        }

        // An empty goal carries no capability signal — returning the
        // core agents is the only meaningful answer. Skipping the
        // semantic steps also closes a resource surface: step 3 runs a
        // model projection per registered agent (~150 IPC calls), so an
        // A2A caller sending an empty message could otherwise trigger a
        // ~minute of model work per request.
        if lower_goal.trim().is_empty() {
            return fleet;
        }

        // 1b. Semantic intent bus: match goal against agent descriptions/anchors
        // when keyword routing left gaps (or always boost discovery).
        {
            let caps: Vec<(String, String)> = available_agents
                .iter()
                .filter(|a| !a.is_core)
                .map(|a| {
                    let desc = format!(
                        "{} {} {}",
                        a.description,
                        a.semantic_anchors.join(" "),
                        a.categories.join(" ")
                    );
                    (a.name.clone(), desc)
                })
                .collect();
            let bus = crate::susi_core::intent_bus::IntentBus::global();
            for (name, desc) in &caps {
                let _ = bus.advertise(name, desc, serde_json::json!({"agent": name}), None);
            }
            let matched = bus.match_goal_to_capabilities(goal, &caps, 0.18, max_agents);
            for (name, _score) in matched {
                if fleet.iter().any(|a| a.name() == name) {
                    continue;
                }
                if let Some(profile) = available_agents.iter().find(|a| a.name == name) {
                    if is_external_executor(profile) && !explicitly_requested(profile) {
                        continue;
                    }
                    fleet.push(instantiate_agent(profile));
                }
            }
            let _ = bus.need(
                "MissionScheduler",
                goal,
                serde_json::json!({"workspace": workspace.display().to_string()}),
                None,
                0.18,
                8,
            );
        }

        // 2. Inference endpoints mapping — open admission: any configured
        // OpenAI-compat / protocol endpoint mounts as a DynamicInferenceEndpointAgent
        // when api_base is set (env override optional).
        //
        // Remote (non-loopback) endpoints receive the mission goal verbatim
        // as the request body — before auth is even evaluated by the remote.
        // Mounting one under local_only mode, or without a resolvable key
        // when the endpoint declares `api_key_env`, is pure data egress that
        // can never produce a completion. Loopback engines (Ollama, vLLM, …)
        // are exempt: they are local substrates.
        let mock_inference =
            std::env::var("SUSI_TEST_MOCK_INFERENCE").unwrap_or_default() == "true";
        let cloud_blocked =
            crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference();
        for endpoint in &cfg.inference_endpoints().endpoints {
            // The mock-inference seam must exclude loopback bridges too: a
            // test host may have a real Ollama/vLLM service listening, and
            // contacting it would make the suite non-hermetic.
            if mock_inference {
                continue;
            }
            if endpoint.api_base.trim().is_empty() {
                continue;
            }
            let env_var_name = format!(
                "{}_API_BASE",
                endpoint.name.to_uppercase().replace('.', "_")
            );
            let base_url = crate::susi_config::env_or_cloud_env(&env_var_name)
                .unwrap_or_else(|_| endpoint.api_base.clone());
            if base_url.trim().is_empty() {
                continue;
            }
            if is_remote_inference_endpoint(&base_url) {
                if cloud_blocked {
                    continue;
                }
                if !endpoint.api_key_env.trim().is_empty()
                    && resolve_inference_key(&endpoint.api_key_env).is_none()
                {
                    continue;
                }
            }
            if fleet
                .iter()
                .any(|a| a.name() == format!("{}BridgeAgent", endpoint.name))
            {
                continue;
            }
            let mut bridge = DynamicInferenceEndpointAgent::new(
                &endpoint.name,
                &base_url,
                &endpoint.protocol_type,
            );
            bridge.model = endpoint.model.clone();
            bridge.api_key_env = endpoint.api_key_env.clone();
            fleet.push(Arc::new(bridge));
        }

        // 3. Semantic Meta-Registry Discovery
        let available_agents = registry.list_agents();
        let mut max_global_similarity = 0.0f32;

        if !lower_goal.contains("admin mission") && !lower_goal.contains("admin pulse") {
            if let Ok(goal_vec) =
                crate::susi_core::plane_bus::gemi::SusiAlphaModel::semantic_centroid_projection(
                    goal, workspace,
                )
            {
                for agent in available_agents {
                    if fleet.len() >= max_agents {
                        break;
                    }
                    if fleet.iter().any(|a| a.name() == agent.name) {
                        continue;
                    }
                    if is_external_executor(&agent) && !explicitly_requested(&agent) {
                        continue;
                    }

                    let mut max_similarity = 0.0f32;
                    let mut agent_corpus = agent.categories.join(" ");
                    agent_corpus.push(' ');
                    agent_corpus.push_str(&agent.description);

                    if let Ok(agent_vec) =
                        crate::susi_core::plane_bus::gemi::SusiAlphaModel::semantic_centroid_projection(
                            &agent_corpus,
                            workspace,
                        )
                    {
                        let dot_product: f32 = goal_vec
                            .iter()
                            .zip(agent_vec.iter())
                            .map(|(a, b)| a * b)
                            .sum();
                        max_similarity = dot_product;
                        if max_similarity > max_global_similarity {
                            max_global_similarity = max_similarity;
                        }
                    }

                    if max_similarity > 0.35
                        || agent
                            .categories
                            .iter()
                            .any(|c| goal.to_lowercase().contains(c))
                    {
                        fleet.push(instantiate_agent(&agent));
                    }
                }
            }
        }

        // 4. Neural Agent Synthesis
        // `trust_level` (config.default.json) gates how eager the substrate is
        // to autonomously mint a brand-new specialist agent via LLM synthesis —
        // Mandate 16's "structural synthesis" — a standing config field with an
        // accessor (`SusiConfig::trust_level`) that no call site ever consulted
        // before this.
        let synthesis_similarity_threshold =
            Self::synthesis_similarity_threshold(&cfg.trust_level());
        let only_mandatory = fleet.len() <= 12; // Adjusted baseline
        if !is_query_or_admin
            && (max_global_similarity < synthesis_similarity_threshold || only_mandatory)
            && fleet.len() < max_agents
        {
            eprintln!("[Swarm] Capability gap detected (Similarity: {:.2}, Threshold: {:.2}, Trust: {}). Triggering Neural Agent Synthesis...", max_global_similarity, synthesis_similarity_threshold, cfg.trust_level());
            if let Ok(new_profile) = NeuralAgentFactory::synthesize_specialist(goal, workspace) {
                eprintln!("[Agent Factory] Specialist recruited: {}", new_profile.name);
                registry.register_agent(new_profile.clone());
                fleet.push(Arc::new(DynamicAgent {
                    agent_name: new_profile.name,
                    mission_profile: new_profile.description,
                    agent_rank: new_profile.base_rank,
                }));
            }
        }

        // 5. Fallback Universal Reasoner
        if fleet.len() < 4 && fleet.len() < max_agents {
            fleet.push(Arc::new(DynamicAgent {
                agent_name: "UniversalReasoner".into(),
                mission_profile: "General-purpose logic and task fulfillment.".into(),
                agent_rank: 0.7,
            }));
        }

        fleet
    }
    pub fn dispatch_explosive_swarm(
        goal: String,
        workspace: PathBuf,
        blackboard: MissionBlackboard,
    ) -> Vec<(String, String)> {
        use std::io::Write;
        let mut agents = Self::synthesize_fleet(&goal, &workspace);

        // Governance Sequencing: Safety/Security must clear the goal before any
        // execution-capable agent runs — they cannot race in the same parallel
        // batch or a destructive/leaking command could execute before the veto lands.
        let governance_idx: Vec<usize> = agents
            .iter()
            .enumerate()
            .filter(|(_, a)| a.name() == "SafetyAgent" || a.name() == "SecurityAgent")
            .map(|(i, _)| i)
            .collect();
        let mut governance_agents = Vec::new();
        for &i in governance_idx.iter().rev() {
            governance_agents.push(agents.remove(i));
        }
        governance_agents.reverse();

        let mut results: Vec<(String, String)> = Vec::new();
        for agent in governance_agents {
            let name = agent.name();
            match agent.execute(&goal, &workspace, &blackboard) {
                Ok(res) => {
                    blackboard.insert(name.clone(), res.clone());
                    results.push((name, res));
                }
                Err(e) => {
                    println!(
                        "- [Swarm Dispatch] Governance veto from {}: {} — aborting swarm dispatch.",
                        name, e
                    );
                    let _ = std::io::stdout().flush();
                    let failure = format!("[GOVERNANCE_BLOCK] {}", e);
                    blackboard.insert(name.clone(), failure.clone());
                    results.push((name, failure));
                    persist_governance_report(&workspace, &results, false);
                    return results;
                }
            }
        }
        persist_governance_report(&workspace, &results, true);

        // Priority admission: score the recruited fleet (learned rank +
        // intent match + urgency affinity) and run only the top
        // `max_concurrent_agents`; the rest are deferred this mission.
        let cap = Self::get_max_concurrent_agents();
        let (agents, decision) = crate::scheduler::MissionScheduler::schedule(&goal, agents, cap);
        let deferred: Vec<&str> = decision
            .entries
            .iter()
            .filter(|e| !e.admitted)
            .map(|e| e.name.as_str())
            .collect();
        if !deferred.is_empty() {
            println!(
                "- [Mission Scheduler] {} agents deferred (cap {}): {}",
                deferred.len(),
                cap,
                deferred.join(", ")
            );
            let _ = std::io::stdout().flush();
        }

        let agents_len = agents.len();

        println!(
            "- [Swarm Dispatch] Initializing thread-parallel execution for {} agents...",
            agents_len
        );
        let _ = std::io::stdout().flush();

        let par_results: Vec<(String, String)> = crate::susi_core::parallel::map(agents, |agent| {
            let name = agent.name();
            let task_handle = crate::susi_core::task_manager::SwarmTaskManager::global()
                .register_task(&name, &goal);
            let start = std::time::Instant::now();

            task_handle.check_pause();
            if task_handle.is_cancelled() {
                task_handle.mark_failed("Agent execution cancelled/stalled");
                return (
                    name,
                    "[STALLED] Agent execution cancelled by Swarm Watchdog.".to_string(),
                );
            }

            task_handle.report_progress();

            // Iteration 1: Panic Isolation Safeguard
            let exec_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                agent.execute(&goal, &workspace, &blackboard)
            }));

            let mut res = match exec_result {
                Ok(Ok(output)) => output,
                Ok(Err(e)) => format!("Agent Execution Failed: {e}"),
                Err(_) => format!("[PANIC_RECOVERED] Agent {name} encountered an unexpected panic during execution."),
            };

            let elapsed = start.elapsed();
            let duration_us = elapsed.as_micros();

            // Iteration 2: Microsecond Telemetry & Latency Accounting
            blackboard.insert(format!("{name}#latency_us"), duration_us.to_string());

            // Iteration 3: Dynamic Fallback Escalation Matrix
            if !crate::accountability::is_usable(&res) && name != "DynamicAgent" {
                let fallback_agent = DynamicAgent {
                    agent_name: format!("{name}#Fallback"),
                    mission_profile: format!("Fallback recovery agent for {name}"),
                    agent_rank: 0.5,
                };
                if let Ok(fallback_res) = fallback_agent.execute(&goal, &workspace, &blackboard) {
                    if crate::accountability::is_usable(&fallback_res) {
                        res = format!("{res} | [FALLBACK_RECOVERY]: {fallback_res}");
                    }
                }
            }

            let res = if task_handle.is_cancelled() {
                "[STALLED] Agent execution cancelled before its result was accepted.".to_string()
            } else {
                res
            };

            // The returned outcome supersedes optimistic intermediate observations.
            blackboard.insert(name.clone(), res.clone());

            if !crate::accountability::is_usable(&res) {
                task_handle.mark_failed(&res);
            } else {
                task_handle.mark_completed(&res);
            }

            if !res.trim().is_empty() && !res.contains("Query reflex audited") {
                let line_count = res.lines().count();
                if line_count > 1 {
                    println!("- [Swarm Flux Trace] {}: [Generated {} lines of payload/content] (Latency: {}µs)", name, line_count, duration_us);
                } else {
                    println!(
                        "- [Swarm Flux Trace] {}: {} (Latency: {}µs)",
                        name,
                        res.trim(),
                        duration_us
                    );
                }
            }
            let _ = std::io::stdout().flush();
            (name, res)
        });

        results.extend(par_results);

        // MissionDag lives in susi-gawd-swarm; agents leaf calls it only via
        // dag_hooks (registered by swarm / host). Keeps the agents→swarm DAG acyclic.
        results.extend(crate::dag_hooks::run(&goal, &workspace, &blackboard));

        results
    }
}

/// Glass-box proof of Mandate 37: Safety/Security cleared (or vetoed) before
/// the parallel fleet. Persisted for `susi substrate` / mission inspectability.
fn persist_governance_report(workspace: &Path, results: &[(String, String)], cleared: bool) {
    let dir = workspace.join(".susi");
    let _ = std::fs::create_dir_all(&dir);
    let agents: Vec<serde_json::Value> = results
        .iter()
        .map(|(name, outcome)| {
            // The shared credential redactor, not a private prefix list.
            let redacted = crate::susi_config::redact_credentials(outcome);
            serde_json::json!({
                "agent": name,
                "outcome": redacted,
            })
        })
        .collect();
    let body = serde_json::json!({
        "kind": "governance_first",
        "cleared": cleared,
        "sequencing": "SafetyAgent and SecurityAgent awaited before parallel fleet",
        "agents": agents,
    });
    let _ = crate::susi_config::atomic_write_bytes(
        &dir.join("last_governance.json"),
        serde_json::to_string_pretty(&body)
            .unwrap_or_else(|_| "{}".into())
            .as_bytes(),
    );
}

#[cfg(test)]
#[path = "../tests/fleet.rs"]
mod tests;
