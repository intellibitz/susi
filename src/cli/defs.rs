//! Native bootstrap commands and public root-help contract.
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "susi")]
#[command(about = "susi: RSI swarm AI OS for hosting, provisioning, and managing local and cloud AI — GAWD, GEMI & GMCP", long_about = None)]
pub(crate) struct CliSurface<C: Subcommand> {
    #[command(subcommand)]
    pub(crate) command: Option<C>,

    /// Natural language intent or pulse
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) intent: Vec<String>,
}

pub(crate) type Cli = CliSurface<Commands>;
pub(crate) type RootInfo = CliSurface<CommandSummaries>;

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Initialize sandboxed .susi environment
    Install,
    /// Activate a qualified Windows release or restore the previous verified release
    InstallRelease {
        /// Trusted SHA-256 release ID supplied by the local production qualifier
        #[arg(long, required_unless_present_any = ["rollback","status","promote"], conflicts_with_all = ["rollback","status","promote"])]
        id: Option<String>,
        /// Qualified predecessor in the adjacent sibling release directory
        #[arg(long, requires = "id")]
        previous: Option<String>,
        /// Import the published Windows v0.21.51 engine from the legacy flat installation
        #[arg(long, requires_all = ["id", "controller_sha256"], conflicts_with = "previous")]
        import_legacy: bool,
        /// Trusted SHA-256 of the migration controller from the upgrade distribution
        #[arg(long, requires = "import_legacy")]
        controller_sha256: Option<String>,
        /// Restore the previously admitted release
        #[arg(long)]
        rollback: bool,
        /// Re-activate this exact already installed release after a rollback
        #[arg(long, conflicts_with_all = ["rollback","status","expect"])]
        promote: Option<String>,
        /// Verify the installed selector, executable, PATH and Start Menu entry
        #[arg(long, conflicts_with = "rollback")]
        status: bool,
        /// Require this specific release for verification or rollback
        #[arg(long, conflicts_with = "id")]
        expect: Option<String>,
        /// Internal response channel for the current user's Windows host task.
        #[arg(long, hide = true)]
        host_worker: bool,
    },
    /// Build, test, integrate and qualify Susi locally through native guarded tools
    Local {
        /// build debug|production, workflow <command>, or release <command>
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum CommandSummaries {
    /// Manage external task executors end to end
    Agents,
    /// Manage agent frameworks/engines end to end (LangGraph, CrewAI, …)
    Frameworks,
    /// Ensure the global susi daemon is running and report host-contract endpoints
    Start,
    /// Stop the global susi daemon (deterministic control plane — never a mission)
    Stop,
    /// Restart the global susi daemon (stop then start — never a mission)
    Restart,
    /// Initialize sandboxed .susi environment
    Install,
    /// Activate a qualified Windows release or restore the previous verified release
    InstallRelease,
    /// Build, test, integrate and qualify Susi locally through native guarded tools
    Local,
    /// Clean up sandboxed .susi environment
    Uninstall,
    /// Manage leading MCP tool servers, or serve SUSI's native MCP stdio endpoint
    Mcp,
    /// Serve SUSI's native ACP JSON-RPC 2.0 stdio endpoint, or inspect ACP discovery
    Acp,
    /// Manage extension packs (auto-seeded vendor opinions under ~/.susi/extensions)
    #[command(name = "extensions", visible_alias = "ext")]
    Extensions,
    /// Zero-config auto substrate: packs, MCP, models, peers readiness
    Auto,
    /// One end-to-end ecosystem setup: detect, consent, apply (safe to rerun)
    Setup,
    /// Moved to Susi Commercial Apps (`susi-commercial workflow`); refuses
    /// naming that command
    #[command(hide = true, disable_help_flag = true)]
    Workflow,
    /// Harness orchestration: author a plan, run a mission, serve the harness API
    Harness,
    /// RSI eval-corpus runs (the task queue is `susi-commercial tasks`)
    Tasks,
    /// The brain: which model susi trusts for which kind of work (evidence-ranked)
    Brain,
    /// Local AI ecosystem: installed engines/runtimes, GPU backends, explicit start
    #[command(name = "ecosystem", visible_alias = "eco")]
    Ecosystem,
    /// Inspect or post to the mission blackboard (posts mirror to Susi Native Board)
    #[command(name = "blackboard", visible_alias = "bb")]
    Blackboard,
    /// Collaborate through the Susi Native Board and enforce worktree isolation
    #[command(name = "board")]
    Board,
    /// Inspect and query the universal context graph
    #[command(name = "context-graph", visible_alias = "cg")]
    ContextGraph,
    /// Inter-app permission and message broker
    #[command(name = "broker")]
    Broker,
    /// Sample host telemetry (thermal / battery / load)
    #[command(name = "telemetry")]
    Telemetry,
    /// Apply a patch and run tests, rolling back on failure
    #[command(name = "patch")]
    Patch,
    /// Solve a goal using the autonomous planning loop
    #[command(name = "plan")]
    Plan,
    /// Privacy mode, egress consent, and cryptographic capability grants
    #[command(name = "privacy")]
    Privacy,
    /// Semantic intent bus (advertise / need / match)
    #[command(name = "intent")]
    Intent,
    /// Ambient FS → context graph / vector index sync
    #[command(name = "ambient")]
    Ambient,
    /// Multi-agent transactional snapshots
    #[command(name = "tx")]
    Tx,
    /// Pluggable / Sandbox / Governance / Host-contract / Reflexes status
    Substrate,
    /// Tier S crown: verify every USP holds (Truth, Evidence, Swarm, …)
    Crown,
    /// Start GEMI REST server
    Gemi,
    /// Inspect workspace health report
    Status,
    /// Leaf-service process table: supervised pids, live health, restart
    Services,
    /// Audit the replicated quorum-commit ledger (signature-verified)
    Commits,
    /// Operating-system view: consensus term/leader, services, verified peers
    Os,
    /// Verified cluster roster: list peers, revoke a member's trust
    Peers,
    /// Run one leaf service in-process — the daemon's spawn mode: the
    /// staged `susi` binary re-execs itself as `service-run <name>` so
    /// supervised services need no sibling binaries on disk.
    #[command(name = "service-run", hide = true)]
    ServiceRun,
    /// Report on autonomous invisible work performed by the substrate
    #[command(name = "substrate-report", alias = "sovereign-dashboard")]
    SubstrateReport,
    /// Audit every Rust source in the workspace (AST-based, production code only) and target/ for bloat and hardcoded secrets, in parallel across all cores
    #[command(name = "bloat-audit")]
    BloatAudit,
    /// Manage leading developer/agent models end to end
    Models,
    /// Manage top open-weight frontier models end to end (Ollama/vLLM pull + prefer + probe)
    #[command(name = "openweight", visible_alias = "ow")]
    OpenWeight,
    /// Manage top frontier models end to end (Claude / GPT / DeepSeek / Gemini / Llama)
    #[command(name = "frontier", visible_alias = "fm")]
    Frontier,
    /// Manage OpenRouter end to end (keys, prefer, probe, live models)
    #[command(name = "openrouter", visible_alias = "or")]
    OpenRouter,
    /// Cloud API keys — list, set, prefer, remove (like `models` for backends)
    #[command(name = "keys", visible_alias = "key")]
    Keys,
    /// Select or override active model
    SelectModel,
    /// Parallel deep scan of substrate home for local models
    DeepScan,
    /// Open-admit an MCP server (stdio command or HTTP URL) into ~/.susi/mcp_config.json
    #[command(name = "mcp-add")]
    McpAdd,
    /// Verify model download agent, network status, and the hardware-selected model ladder
    #[command(name = "verify-download-agent")]
    VerifyDownloadAgent,
    /// Scout or install model substrate via live foreground network stream
    #[command(name = "scout-model")]
    ScoutModel,
    /// Measure real local inference latency/tokens-per-sec, and compare
    /// against a cloud endpoint if SUSI_BENCH_CLOUD_API_BASE is set
    Benchmark,
    /// Ingest a natural language intent into sovereign memory (evidence.json)
    Pulse,
    /// First-class automation: run an autonomous evidence-gated swarm mission
    Automate,
    /// Review daemon and OS environment status
    Review,
    /// Remove SUSI-owned residue (never host or other apps' caches) and report bytes freed
    #[command(name = "os-clean")]
    OsClean,
    /// Perform compliance audit and verify the tamper-evident audit chain
    Audit,
    /// Administrative commands
    Admin,
    /// Clean workspace build artifacts
    Clean,
    /// Direct streaming model execution (local Ollama/llama.cpp/Candle or cloud)
    #[command(name = "run")]
    Run,
    /// Unified operator control surface: workload, resources, providers, config, federation, and evidence
    #[command(name = "top")]
    Top,
    /// Internal daemon start (always binds to substrate home; --workspace kept for compat)
    DaemonStart,
}
