//! Clap surface for the susi CLI: top-level parser, command/subcommand enums,
//! and the daemon-requirement table. Dispatch lives in `control_plane_cli`
//! (pre-boot) and `mission_cli` (post-boot).

use susi::SUSI_VERSION;

use clap::{Parser, Subcommand};

use crate::agent_cli;
use crate::aider_cli;
use crate::auto_cli;
use crate::blackboard_cli;
use crate::browser_use_cli;
use crate::crown_cli;
use crate::deerflow_cli;
use crate::extensions_cli;
use crate::framework_cli;
use crate::frontier_cli;
use crate::gemini_cli;
use crate::mcp_cli;
use crate::model_cli;
use crate::open_weight_cli;
use crate::openclaw_cli;
use crate::openhands_cli;
use crate::openrouter_cli;
use crate::openviking_cli;
use crate::python_engine_cli;
use crate::substrate_cli;
use crate::swe_agent_cli;

#[derive(Parser)]
#[command(name = "susi")]
#[command(version = SUSI_VERSION)]
#[command(about = "susi: evidence-gated AI agent substrate — GAWD, GEMI & GMCP", long_about = None)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Commands>,

    /// Natural language intent or pulse
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) intent: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Manage external task executors end to end
    Agents {
        #[command(subcommand)]
        action: agent_cli::AgentCommands,
    },
    /// Manage agent frameworks/engines end to end (LangGraph, CrewAI, …)
    Frameworks {
        #[command(subcommand)]
        action: framework_cli::FrameworkCommands,
    },
    /// Ensure the global susi daemon is running and report host-contract endpoints
    Start,
    /// Stop the global susi daemon (deterministic control plane — never a mission)
    Stop,
    /// Restart the global susi daemon (stop then start — never a mission)
    Restart,
    /// Start persistent SUSI Pulse Shell
    Shell,
    /// Initialize sandboxed .susi environment
    Install,
    /// Clean up sandboxed .susi environment
    Uninstall,
    /// Manage leading MCP tool servers, or serve SUSI's native MCP stdio endpoint
    Mcp {
        #[command(subcommand)]
        action: Option<mcp_cli::McpCommands>,
    },
    /// Manage extension packs (auto-seeded vendor opinions under ~/.susi/extensions)
    #[command(name = "extensions", visible_alias = "ext")]
    Extensions {
        #[command(subcommand)]
        action: Option<extensions_cli::ExtensionCommands>,
    },
    /// Zero-config auto substrate: packs, MCP, models, peers readiness
    Auto {
        #[command(subcommand)]
        action: Option<auto_cli::AutoCommands>,
    },
    /// Inspect the last mission blackboard (swarm shared state)
    #[command(name = "blackboard", visible_alias = "bb")]
    Blackboard {
        #[command(subcommand)]
        action: Option<blackboard_cli::BlackboardCommands>,
    },
    /// Pluggable / Sandbox / Governance / Host-contract / Reflexes status
    Substrate {
        #[command(subcommand)]
        action: Option<substrate_cli::SubstrateCommands>,
    },
    /// Tier S crown: verify every USP holds (Truth, Evidence, Swarm, …)
    Crown {
        #[command(subcommand)]
        action: Option<crown_cli::CrownCommands>,
    },
    /// Start GEMI REST server
    Gemi,
    /// Inspect workspace health report
    Status,
    /// Report on autonomous invisible work performed by the substrate
    SovereignDashboard,
    /// Recursively audit src/ (AST-based) and target/ for bloat and hardcoded secrets, rayon-parallel across all cores
    #[command(name = "bloat-audit")]
    BloatAudit,
    /// Manage leading developer/agent models end to end
    Models {
        #[command(subcommand)]
        action: Option<model_cli::ModelCommands>,
    },
    /// Manage top open-weight frontier models end to end (Ollama/vLLM pull + prefer + probe)
    #[command(name = "openweight", visible_alias = "ow")]
    OpenWeight {
        #[command(subcommand)]
        action: Option<open_weight_cli::OpenWeightCommands>,
    },
    /// Manage top frontier models end to end (Claude / GPT / DeepSeek / Gemini / Llama)
    #[command(name = "frontier", visible_alias = "fm")]
    Frontier {
        #[command(subcommand)]
        action: Option<frontier_cli::FrontierCommands>,
    },
    /// Manage OpenRouter end to end (keys, prefer, probe, live models)
    #[command(name = "openrouter", visible_alias = "or")]
    OpenRouter {
        #[command(subcommand)]
        action: Option<openrouter_cli::OpenRouterCommands>,
    },
    /// Manage OpenHands end to end (CLI executor + LLM env + durable tasks)
    #[command(name = "openhands", visible_alias = "oh")]
    OpenHands {
        #[command(subcommand)]
        action: Option<openhands_cli::OpenHandsCommands>,
    },
    /// Manage Gemini CLI end to end (headless executor + auth + durable tasks)
    #[command(name = "gemini", visible_alias = "gcli")]
    Gemini {
        #[command(subcommand)]
        action: Option<gemini_cli::GeminiCliCommands>,
    },
    /// Manage Aider end to end (headless scripting + LLM env + durable tasks)
    #[command(name = "aider")]
    Aider {
        #[command(subcommand)]
        action: Option<aider_cli::AiderCommands>,
    },
    /// Manage SWE-agent end to end (headless local repo + durable tasks)
    #[command(name = "swe-agent", visible_alias = "sweagent")]
    SweAgent {
        #[command(subcommand)]
        action: Option<swe_agent_cli::SweAgentCommands>,
    },
    /// Manage OpenClaw end to end (agent exec + auth + durable tasks)
    #[command(name = "openclaw", visible_alias = "oc")]
    OpenClaw {
        #[command(subcommand)]
        action: Option<openclaw_cli::OpenClawCommands>,
    },
    /// Manage Browser Use end to end (headless prompt mode + durable tasks)
    #[command(name = "browser-use", visible_alias = "bu")]
    BrowserUse {
        #[command(subcommand)]
        action: Option<browser_use_cli::BrowserUseCommands>,
    },
    /// Manage OpenViking end to end (context DB client + durable find tasks)
    #[command(name = "openviking", visible_alias = "viking")]
    OpenViking {
        #[command(subcommand)]
        action: Option<openviking_cli::OpenVikingCommands>,
    },
    /// Manage DeerFlow end to end (headless harness + durable tasks)
    #[command(name = "deerflow", visible_alias = "deer")]
    DeerFlow {
        #[command(subcommand)]
        action: Option<deerflow_cli::DeerFlowCommands>,
    },
    /// Manage LangGraph end to end (Python engine + config + durable tasks)
    #[command(name = "langgraph", visible_alias = "lg")]
    LangGraph {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage OpenAI Agents SDK end to end (Python engine + config + durable tasks)
    #[command(name = "openai-agents", visible_alias = "oas")]
    OpenAiAgents {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage AutoGen end to end (Python AgentChat engine + config + durable tasks)
    #[command(name = "autogen", visible_alias = "ag")]
    AutoGen {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage Hugging Face smolagents end to end (Python engine + config + durable tasks)
    #[command(name = "smolagents", visible_alias = "smol")]
    SmolAgents {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage CrewAI end to end (role-based multi-agent orchestration)
    #[command(name = "crewai")]
    CrewAi {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage LlamaIndex end to end (RAG / agent data framework)
    #[command(name = "llamaindex", visible_alias = "llama")]
    LlamaIndex {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage Temporal end to end (durable workflow engine)
    #[command(name = "temporal")]
    Temporal {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage E2B end to end (cloud sandbox code-execution runtime)
    #[command(name = "e2b")]
    E2b {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage Haystack end to end (document / RAG pipelines)
    #[command(name = "haystack")]
    Haystack {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Manage n8n end to end (automation / event-trigger workflows)
    #[command(name = "n8n")]
    N8n {
        #[command(subcommand)]
        action: Option<python_engine_cli::EngineCommands>,
    },
    /// Cloud API keys — list, set, prefer, remove (like `models` for backends)
    #[command(name = "keys", visible_alias = "key")]
    Keys {
        #[command(subcommand)]
        action: Option<KeyCommands>,
    },
    /// Select or override active model
    SelectModel { model: String },
    /// Parallel deep scan of substrate home for local models
    DeepScan,
    /// Autonomous web-scouting of open-source MCP servers
    McpScout,
    /// Open-admit an MCP server (stdio command or HTTP URL) into ~/.susi/mcp_config.json
    #[command(name = "mcp-add")]
    McpAdd {
        /// Registry name for the server
        name: String,
        /// Stdio command or streamable HTTP MCP URL (`http://…` / `https://…`)
        command_or_url: String,
        /// Extra argv for stdio MCP servers
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Verify model download agent, network status, and 32b/72b model provisioning
    #[command(name = "verify-download-agent")]
    VerifyDownloadAgent,
    /// Scout or install model substrate via live foreground network stream
    #[command(name = "scout-model")]
    ScoutModel { url: String },
    /// Measure real local inference latency/tokens-per-sec, and compare
    /// against a cloud endpoint if SUSI_BENCH_CLOUD_API_BASE is set
    Benchmark,
    /// Native LLM reasoning accuracy evaluations (MMLU-lite)
    Eval,
    /// Ingest a natural language intent into sovereign memory (evidence.json)
    Pulse {
        #[arg(trailing_var_arg = true)]
        intent: Vec<String>,
    },
    /// First-class automation: run an autonomous evidence-gated swarm mission
    Automate {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        intent: Vec<String>,
    },
    /// Accept and merge all staged intent bundles in the current workspace
    Accept,
    /// Rollback and undo all staged intent fixes in the current workspace
    Undo,
    /// Review staged intent bundles and OS environment status
    Review,
    /// Execute proactive OS package and cache hygiene
    #[command(name = "os-clean")]
    OsClean,
    /// Perform compliance audit and technical verification
    Audit,
    /// Administrative commands
    Admin {
        #[command(subcommand)]
        subcommand: AdminCommands,
    },
    /// Clean workspace build artifacts
    Clean,
    /// Internal daemon start (always binds to substrate home; --workspace kept for compat)
    DaemonStart {
        #[arg(long, default_value_t = String::new())]
        workspace: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum KeyCommands {
    /// Save a vendor API key and register the cloud provider
    Set {
        /// Vendor: openai, anthropic, gemini, deepseek, kimi, minimax, openrouter, …
        vendor: String,
        /// API key (omit to be prompted, or pipe via stdin)
        api_key: Option<String>,
    },
    /// Show which known vendors have a key configured (never prints secrets)
    List,
    /// Prefer a cloud vendor when several keys are registered (e.g. paid over free)
    Prefer {
        /// Vendor to prefer (openai, deepseek, …). Omit to show current preference.
        vendor: Option<String>,
        /// Clear the sticky preferred cloud
        #[arg(long)]
        clear: bool,
    },
    /// Remove a vendor API key from ~/.susi/cloud.env
    Remove { vendor: String },
}

#[derive(Subcommand)]
pub(crate) enum AdminCommands {
    /// Synchronize version consistency
    Sync,
    /// Ingest pulse via admin
    Pulse {
        #[arg(trailing_var_arg = true)]
        intent: Vec<String>,
    },
    /// Compliance audit
    Audit,
    /// Verify version alignment
    Verify,
    /// Full release orchestration. With --cut, also bumps the engine
    /// version and pushes the matching vX.Y.Z tag atomically, so a git tag
    /// (what actually makes a new release appear on GitHub - release.yml
    /// only triggers on a pushed `v*.*.*` tag) can never again drift out of
    /// sync with the version number the way it did under the old
    /// bump-on-every-push scheme.
    Release {
        #[arg(long)]
        cut: Option<susi_gawd::admin::VersionBump>,
    },
    /// Run clippy
    Lint,
    /// Run cargo audit
    AuditDeps,
    /// Dynamic configuration hot-reload
    Reload,
}

/// Mandate 32: only ensure the daemon for commands that need the background
/// substrate. Local-only admin/workspace ops must return without blocking on
/// binary integrity checks or daemon restart.
pub(crate) fn command_requires_daemon(command: &Commands) -> bool {
    match command {
        Commands::Agents { .. }
        | Commands::Frameworks { .. }
        | Commands::Models { .. }
        | Commands::OpenWeight { .. }
        | Commands::Frontier { .. }
        | Commands::OpenRouter { .. }
        | Commands::OpenHands { .. }
        | Commands::Gemini { .. }
        | Commands::Aider { .. }
        | Commands::SweAgent { .. }
        | Commands::OpenClaw { .. }
        | Commands::BrowserUse { .. }
        | Commands::OpenViking { .. }
        | Commands::DeerFlow { .. }
        | Commands::LangGraph { .. }
        | Commands::OpenAiAgents { .. }
        | Commands::AutoGen { .. }
        | Commands::SmolAgents { .. }
        | Commands::CrewAi { .. }
        | Commands::LlamaIndex { .. }
        | Commands::Temporal { .. }
        | Commands::E2b { .. }
        | Commands::Haystack { .. }
        | Commands::N8n { .. }
            if !matches!(
                command,
                Commands::Models {
                    action: Some(model_cli::ModelCommands::Local)
                }
            ) =>
        {
            false
        }
        Commands::Extensions { .. } => false,
        Commands::Auto { .. }
        | Commands::Blackboard { .. }
        | Commands::Substrate { .. }
        | Commands::Crown { .. } => false,
        Commands::Mcp {
            action: Some(mcp_cli::McpCommands::Serve) | None,
        } => true,
        Commands::Mcp { .. } => false,
        Commands::Clean
        | Commands::Review
        | Commands::Accept
        | Commands::Undo
        | Commands::OsClean
        | Commands::Uninstall
        | Commands::Stop
        | Commands::Restart
        | Commands::Pulse { .. }
        | Commands::McpAdd { .. }
        | Commands::Keys { .. }
        | Commands::DaemonStart { .. } => false,
        Commands::Admin { subcommand } => {
            matches!(
                subcommand,
                AdminCommands::Release { .. } | AdminCommands::Audit
            )
        }
        _ => true,
    }
}
