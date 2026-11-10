//! Clap surface for the susi CLI: top-level parser, command/subcommand enums,
//! and the daemon-requirement table. Dispatch lives in `control_plane_cli`
//! (pre-boot) and `mission_cli` (post-boot).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use super::agent_cli;
use super::ambient_cli;
use super::auto_cli;
use super::blackboard_cli;
use super::brain_cli;
use super::broker_cli;
pub(crate) use super::catalogs::{command_catalogs, mcp_catalogs};
use super::commits_cli;
use super::context_graph_cli;
use super::crown_cli;
use super::ecosystem_cli;
use super::extensions_cli;
use super::framework_cli;
use super::frontier_cli;
use super::harness_cli;
use super::intent_cli;
use super::mcp_cli;
use super::model_cli;
use super::open_weight_cli;
use super::openrouter_cli;
use super::os_cli;
use super::patch_cli;
use super::peers_cli;
use super::plan_cli;
use super::privacy_cli;
use super::services_cli;
use super::setup_cli;
use super::substrate_cli;
use super::tasks_cli;
use super::telemetry_cli;
use super::tx_cli;

#[derive(Parser)]
#[command(name = "susi")]
#[command(about = "susi: RSI swarm AI OS for hosting, provisioning, and managing local and cloud AI — GAWD, GEMI & GMCP", long_about = None)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Option<Commands>,

    /// Natural language intent or pulse
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub(crate) intent: Vec<String>,
}

#[allow(clippy::large_enum_variant)]
// a CLI flag bag: every variant is argument storage, and boxing one would only relocate the flags clap derives in place
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
    /// Initialize sandboxed .susi environment
    Install,
    /// Activate a qualified Windows release or restore the previous verified release
    InstallRelease {
        /// Trusted SHA-256 release ID supplied by the local production qualifier
        #[arg(long, required_unless_present_any = ["rollback","status"], conflicts_with_all = ["rollback","status"])]
        id: Option<String>,
        /// Qualified predecessor in the adjacent sibling release directory
        #[arg(long, requires = "id")]
        previous: Option<String>,
        /// Restore the previously admitted release
        #[arg(long)]
        rollback: bool,
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
    /// One end-to-end ecosystem setup: detect, consent, apply (safe to rerun)
    Setup {
        #[command(subcommand)]
        action: Option<setup_cli::SetupCommands>,
    },
    /// Moved to Susi Commercial Apps (`susi-commercial workflow`); refuses
    /// naming that command
    #[command(hide = true, disable_help_flag = true)]
    Workflow {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Harness orchestration: author a plan, run a mission, serve the harness API
    Harness {
        #[command(subcommand)]
        action: harness_cli::HarnessCommands,
    },
    /// RSI eval-corpus runs (the task queue is `susi-commercial tasks`)
    Tasks {
        #[command(subcommand)]
        action: Option<tasks_cli::TaskCommands>,
    },
    /// The brain: which model susi trusts for which kind of work (evidence-ranked)
    Brain {
        #[command(subcommand)]
        action: Option<brain_cli::BrainCommands>,
    },
    /// Local AI ecosystem: installed engines/runtimes, GPU backends, explicit start
    #[command(name = "ecosystem", visible_alias = "eco")]
    Ecosystem {
        #[command(subcommand)]
        action: Option<ecosystem_cli::EcosystemCommands>,
    },
    /// Inspect the last mission blackboard (swarm shared state)
    #[command(name = "blackboard", visible_alias = "bb")]
    Blackboard {
        #[command(subcommand)]
        action: Option<blackboard_cli::BlackboardCommands>,
    },
    /// Inspect and query the universal context graph
    #[command(name = "context-graph", visible_alias = "cg")]
    ContextGraph {
        #[command(subcommand)]
        action: Option<context_graph_cli::ContextGraphCommands>,
    },
    /// Inter-app permission and message broker
    #[command(name = "broker")]
    Broker {
        #[command(subcommand)]
        action: Option<broker_cli::BrokerCommands>,
    },
    /// Sample host telemetry (thermal / battery / load)
    #[command(name = "telemetry")]
    Telemetry {
        #[command(subcommand)]
        action: Option<telemetry_cli::TelemetryCommands>,
    },
    /// Apply a patch and run tests, rolling back on failure
    #[command(name = "patch")]
    Patch {
        #[command(flatten)]
        args: patch_cli::PatchApplyArgs,
    },
    /// Solve a goal using the autonomous planning loop
    #[command(name = "plan")]
    Plan {
        #[command(flatten)]
        args: plan_cli::PlanArgs,
    },
    /// Privacy mode, egress consent, and cryptographic capability grants
    #[command(name = "privacy")]
    Privacy {
        #[command(subcommand)]
        action: Option<privacy_cli::PrivacyCommands>,
    },
    /// Semantic intent bus (advertise / need / match)
    #[command(name = "intent")]
    Intent {
        #[command(subcommand)]
        action: Option<intent_cli::IntentCommands>,
    },
    /// Ambient FS → context graph / vector index sync
    #[command(name = "ambient")]
    Ambient {
        #[command(subcommand)]
        action: Option<ambient_cli::AmbientCommands>,
    },
    /// Multi-agent transactional snapshots
    #[command(name = "tx")]
    Tx {
        #[command(subcommand)]
        action: Option<tx_cli::TxCommands>,
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
    Status {
        /// Emit measured health and executable identity as one JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Leaf-service process table: supervised pids, live health, restart
    Services {
        #[command(subcommand)]
        action: Option<services_cli::ServicesCommands>,
    },
    /// Audit the replicated quorum-commit ledger (signature-verified)
    Commits {
        #[command(subcommand)]
        action: Option<commits_cli::CommitsCommands>,
    },
    /// Operating-system view: consensus term/leader, services, verified peers
    Os {
        /// Emit machine-readable JSON instead of the table view
        #[arg(long)]
        json: bool,
        #[command(subcommand)]
        action: Option<os_cli::OsCommands>,
    },
    /// Verified cluster roster: list peers, revoke a member's trust
    Peers {
        #[command(subcommand)]
        action: Option<peers_cli::PeersCommands>,
    },
    /// Run one leaf service in-process — the daemon's spawn mode: the
    /// staged `susi` binary re-execs itself as `service-run <name>` so
    /// supervised services need no sibling binaries on disk.
    #[command(name = "service-run", hide = true)]
    ServiceRun {
        /// Leaf service name: susi-paths | susi-error | susi-config |
        /// susi-sandbox | susi-native
        name: String,
        /// Port override (defaults to the service's contract port env var)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Report on autonomous invisible work performed by the substrate
    // Renamed from `sovereign-dashboard` (zero-UI plan A6); the old name
    // stays a hidden alias so existing scripts keep working.
    #[command(name = "substrate-report", alias = "sovereign-dashboard")]
    SubstrateReport,
    /// Audit every Rust source in the workspace (AST-based, production code only) and target/ for bloat and hardcoded secrets, in parallel across all cores
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
    /// Verify model download agent, network status, and the hardware-selected model ladder
    #[command(name = "verify-download-agent")]
    VerifyDownloadAgent,
    /// Scout or install model substrate via live foreground network stream
    #[command(name = "scout-model")]
    ScoutModel { url: String },
    /// Measure real local inference latency/tokens-per-sec, and compare
    /// against a cloud endpoint if SUSI_BENCH_CLOUD_API_BASE is set
    Benchmark,
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
    /// Review daemon and OS environment status
    Review,
    /// Remove SUSI-owned residue (never host or other apps' caches) and report bytes freed
    #[command(name = "os-clean")]
    OsClean,
    /// Perform compliance audit and verify the tamper-evident audit chain
    Audit {
        #[command(subcommand)]
        action: Option<AuditCommands>,
    },
    /// Administrative commands
    Admin {
        #[command(subcommand)]
        subcommand: AdminCommands,
    },
    /// Clean workspace build artifacts
    Clean,
    /// Direct streaming model execution (local Ollama/llama.cpp/Candle or cloud)
    #[command(name = "run")]
    Run(super::run_cli::RunArgs),
    /// Unified operator control surface: workload, resources, providers, config, federation, and evidence
    #[command(name = "top")]
    Top(super::top_cli::TopArgs),
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
    /// Live-check registered keys against each vendor (one read-only /models call each)
    Check {
        /// Vendor or alias (e.g. xai, grok, together). Omit to check every cloud vendor.
        vendor: Option<String>,
    },
    /// List the models a vendor serves right now (uses the registered key)
    Models {
        /// Vendor or alias (e.g. fireworks, deepinfra)
        vendor: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum AdminCommands {
    /// Ingest pulse via admin
    Pulse {
        #[arg(trailing_var_arg = true)]
        intent: Vec<String>,
    },
    /// Compliance audit
    Audit,
    /// Export and independently verify redacted audit evidence
    #[command(name = "audit-evidence", visible_alias = "evidence")]
    AuditEvidence {
        #[command(subcommand)]
        action: AuditEvidenceCommands,
    },
    /// Run clippy
    Lint,
    /// Run cargo audit
    AuditDeps,
    /// Dynamic configuration hot-reload
    Reload,
    /// Provider contract checks: fixture-backed checks always;
    /// credentialed live probes when SUSI_PROVIDER_PROBE=1
    #[command(name = "provider-contract")]
    ProviderContract,
    /// The graded user-intent ladder: print the last scorecard (default),
    /// run the full ladder hermetically (--run), or just the bounded
    /// start-up slice (--slice). The daemon records failing rungs in the
    /// host outbox (--gaps); an agent adopts one as a repair task in its
    /// own checkout with `susi-commercial admin intent-ladder --adopt-gap`.
    #[command(name = "intent-ladder")]
    IntentLadder {
        /// Run every rung now — hermetic, wall-clock bounded
        #[arg(long)]
        run: bool,
        /// Run only the start-up slice
        #[arg(long)]
        slice: bool,
        /// Re-check one intent id hermetically — the acceptance a repair
        /// task's close runs; exits nonzero unless the intent passes
        #[arg(long, value_name = "ID")]
        verify: Option<String>,
        /// Print the host outbox of failing rungs (`<home>/intent-gaps.json`)
        #[arg(long)]
        gaps: bool,
        /// Moved: adopting a gap authors a task-queue record, which is
        /// `susi-commercial admin intent-ladder --adopt-gap`
        #[arg(long, value_name = "ID", hide = true)]
        adopt_gap: Option<String>,
    },
    /// `sync`, `verify` and `release` moved to Susi Commercial Apps
    /// (`susi-commercial admin <verb>`); refused naming that command
    #[command(external_subcommand)]
    Moved(Vec<String>),
}

#[derive(Subcommand)]
pub(crate) enum AuditEvidenceCommands {
    /// Wrap a JSON segment list in the stable file format
    Export {
        /// JSON array of redacted audit segments (or an existing export)
        #[arg(long, value_name = "FILE")]
        input: PathBuf,
        /// Destination for the independently verifiable export
        #[arg(long, value_name = "FILE")]
        output: PathBuf,
    },
    /// Verify a file against one or more key-id=secret pairs
    Verify {
        /// Export file to verify
        #[arg(long, visible_alias = "file", value_name = "FILE")]
        input: PathBuf,
        /// Signing key in key-id=secret form; repeat for multiple keys
        #[arg(long = "key", visible_alias = "keys", value_name = "KEY=SECRET")]
        keys: Vec<String>,
    },
}

#[derive(Subcommand)]
pub(crate) enum AuditCommands {
    /// Verify every audit record, optionally reporting entries since a Unix timestamp
    Verify {
        /// Only count verified entries at or after this Unix timestamp; the full chain is checked
        #[arg(long)]
        since: Option<u64>,
    },
    /// Query redacted action history by actor, mission, kind, and time
    Query {
        #[arg(long)]
        actor: Option<String>,
        #[arg(long)]
        mission: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        since: Option<u64>,
        #[arg(long)]
        until: Option<u64>,
        /// Emit the query result as a JSON array
        #[arg(long)]
        json: bool,
    },
}

/// Daemon lifecycle and status commands answer from the lock, the contract
/// ports and the files the daemon writes; none of them touches tools,
/// catalogs or inference, so none runs the CLI composition root. That root
/// costs tens of seconds even on a healthy host (catalog priming probes
/// every launcher), and `daemon-start` composes its own substrate in
/// `run_daemon_loop` — running the CLI root first had doubled the daemon's
/// boot before it even took its lock.
pub(crate) fn command_skips_composition(command: &Commands) -> bool {
    matches!(
        command,
        Commands::DaemonStart { .. }
            | Commands::Start
            | Commands::Stop
            | Commands::Restart
            | Commands::Status { .. }
            | Commands::Audit {
                action: Some(AuditCommands::Verify { .. } | AuditCommands::Query { .. }),
            }
            // The dev-workflow commands that moved to Susi Commercial Apps
            // only print their refusal: nothing to compose for that.
            | Commands::Workflow { .. }
            | Commands::Tasks {
                action: None | Some(tasks_cli::TaskCommands::Moved(_)),
            }
            | Commands::Admin {
                subcommand: AdminCommands::Moved(_)
            }
    )
}

/// Local reports that read only the host's own files (`keys` reads key
/// presence and routing prefs): they need the plane
/// handlers, not the CLI composition root, whose cloud-endpoint registration
/// (a walk of the whole model catalog plus the credential scout) took 4.2 s
/// of `susi review`'s 4.9 s.
pub(crate) fn command_is_local_report(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Review | Commands::OsClean | Commands::Keys { .. }
    )
}

/// Mandate 32: only ensure the daemon for commands that need the background
/// substrate. Local-only admin/workspace ops must return without blocking on
/// binary integrity checks or daemon restart.
///
/// Every command is classified explicitly here, and
/// `tests::every_command_has_a_pinned_daemon_and_catalog_classification`
/// pins each one. The default is `false` (D-SP-1): a command added later
/// starts without the daemon until it is classified as needing it.
#[allow(clippy::wildcard_enum_match_arm)]
pub(crate) fn command_requires_daemon(command: &Commands) -> bool {
    match command {
        // `start` brings the daemon up through this gate and then waits for
        // its host contract (`control_plane_start`).
        Commands::Start
        // Substrate work: missions, the GEMI server, inference benchmarks,
        // model selection/scouting/scanning and the substrate's own reports.
        | Commands::Automate { .. }
        | Commands::Gemi
        | Commands::Benchmark
        | Commands::DeepScan
        | Commands::SelectModel { .. }
        | Commands::ScoutModel { .. }
        | Commands::VerifyDownloadAgent
        | Commands::SubstrateReport
        | Commands::BloatAudit
        // Dispatched before this gate is consulted (`install`, `local`,
        // `install-release` in `main::run`; `service-run` and `harness` in
        // `control_plane_cli::dispatch`). Kept at the value they had before
        // the default flipped.
        | Commands::Install
        | Commands::InstallRelease { .. }
        | Commands::Local { .. }
        | Commands::ServiceRun { .. }
        | Commands::Harness { .. } => true,
        Commands::Agents { .. }
        | Commands::Frameworks { .. }
        | Commands::Models { .. }
        | Commands::OpenWeight { .. }
        | Commands::Frontier { .. }
        | Commands::OpenRouter { .. } => false,
        Commands::Extensions { .. } => false,
        Commands::Auto { .. }
        | Commands::Setup { .. }
        | Commands::Ecosystem { .. }
        | Commands::Brain { .. }
        | Commands::Tasks { .. }
        | Commands::Workflow { .. }
        | Commands::Blackboard { .. }
        | Commands::ContextGraph { .. }
        | Commands::Broker { .. }
        | Commands::Telemetry { .. }
        | Commands::Patch { .. }
        | Commands::Plan { .. }
        | Commands::Privacy { .. }
        | Commands::Intent { .. }
        | Commands::Ambient { .. }
        | Commands::Tx { .. }
        | Commands::Substrate { .. }
        | Commands::Crown { .. }
        // Status is the substrate's own measured report — it must answer
        // even when the daemon is down (that's when an operator asks).
        | Commands::Status { .. }
        | Commands::Services { .. }
        | Commands::Commits { .. }
        | Commands::Os { .. }
        | Commands::Peers { .. }
        | Commands::Audit { .. }
        | Commands::Run { .. }
        | Commands::Top { .. } => false,
        // `mcp serve` answers from its own in-process service. Ensuring the
        // daemon first held the MCP handshake for the whole host-contract
        // wait (10 s when the daemon never binds) and left a daemon behind
        // every session.
        Commands::Mcp { .. } => false,
        Commands::Clean
        | Commands::Review
        | Commands::OsClean
        | Commands::Uninstall
        | Commands::Stop
        | Commands::Restart
        | Commands::Pulse { .. }
        | Commands::McpAdd { .. }
        | Commands::Keys { .. }
        | Commands::DaemonStart { .. } => false,
        Commands::Admin { subcommand } => {
            matches!(subcommand, AdminCommands::Audit)
        }
        // Every current variant is matched above; this arm is the default for
        // a variant added later, which compiles to "no daemon" until it is
        // classified (and the pinning test fails until it is pinned).
        #[allow(unreachable_patterns)]
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{command_catalogs, command_skips_composition, Cli};
    use clap::{CommandFactory, Parser};
    use susi_daemon::discovery_pipeline::Catalogs;

    fn skips(args: &[&str]) -> bool {
        let cli = Cli::try_parse_from(args).unwrap();
        command_skips_composition(&cli.command.unwrap())
    }

    fn catalogs(args: &[&str]) -> Catalogs {
        let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        command_catalogs(&cli.command.unwrap())
    }

    fn assert_primes(expected: Catalogs, commands: &[&[&str]]) {
        for cmd in commands {
            assert_eq!(catalogs(cmd), expected, "{cmd:?}");
        }
    }

    /// Commands whose output reads no primed catalog prime nothing.
    #[test]
    fn commands_reading_no_catalog_prime_nothing() {
        assert_primes(
            Catalogs::NONE,
            &[
                &["susi", "agents", "list"],
                &["susi", "agents", "doctor"],
                &["susi", "agents", "tasks"],
                &["susi", "agents", "worker", "t-1"],
                &["susi", "frameworks", "list"],
                &["susi", "frameworks", "doctor"],
                &["susi", "frameworks", "worker", "t-1"],
                &["susi", "openweight", "list"],
                &["susi", "openweight"],
                &["susi", "openweight", "doctor"],
                &["susi", "openweight", "setup", "m"],
                &["susi", "frontier", "list"],
                &["susi", "frontier"],
                &["susi", "frontier", "doctor"],
                &["susi", "frontier", "setup", "m"],
                &["susi", "models", "doctor"],
                &["susi", "models", "prefer", "m"],
                &["susi", "models", "reset", "m"],
                &["susi", "mcp", "enable", "filesystem"],
                &["susi", "mcp", "reset", "filesystem"],
                &["susi", "mcp", "tools", "127.0.0.1:9090"],
                &["susi", "mcp", "ledger"],
                &["susi", "mcp", "call", "127.0.0.1:9090", "t"],
                &["susi", "mcp", "serve"],
                &["susi", "auto", "status"],
                &["susi", "keys", "list"],
                &["susi", "keys"],
                &["susi", "review"],
                &["susi", "audit", "verify"],
                &["susi", "audit"],
                &["susi", "bloat-audit"],
                &["susi", "clean"],
                &["susi", "os-clean"],
            ],
        );
    }

    /// Commands that print the preferred coding model prime only that.
    #[test]
    fn preferred_model_readers_prime_only_coding_models() {
        assert_primes(
            Catalogs::CODING_MODELS,
            &[
                &["susi", "models"],
                &["susi", "models", "list"],
                &["susi", "models", "setup", "m"],
            ],
        );
    }

    /// Regression: every command that writes `selected_model_override.txt`
    /// without writing `preferred_coding_model.txt` primes coding models
    /// first, so the next auto-prefer finds a ready preferred model and
    /// keeps the pin instead of overwriting it.
    #[test]
    fn override_writers_prime_coding_models_first() {
        assert_primes(
            Catalogs::CODING_MODELS,
            &[
                &["susi", "select-model", "m"],
                &["susi", "frontier", "prefer", "m"],
                &["susi", "frontier", "prefer", "--clear"],
                &["susi", "openweight", "prefer", "m"],
                &["susi", "openweight", "prefer", "--clear"],
            ],
        );
    }

    /// Commands that print or use the enabled MCP set prime nothing: core
    /// never enables an MCP server on its own (zero-UI plan A8).
    #[test]
    fn enabled_mcp_readers_prime_nothing() {
        assert_primes(
            Catalogs::NONE,
            &[
                &["susi", "mcp", "list"],
                &["susi", "mcp", "status"],
                &["susi", "mcp", "doctor"],
                &["susi", "mcp", "setup", "filesystem"],
                &["susi", "mcp", "disable", "filesystem"],
            ],
        );
    }

    /// Regression: `mcp call` to anything but a network peer may dispatch in
    /// process (`susi_solve` runs a mission, `reason` generates with the
    /// selected model), so it primes every catalog, as missions do.
    #[test]
    fn in_process_mcp_calls_prime_every_catalog() {
        assert_primes(
            Catalogs::ALL,
            &[
                &[
                    "susi",
                    "mcp",
                    "call",
                    "susi",
                    "susi_solve",
                    "{\"intent\":\"x\"}",
                ],
                &["susi", "mcp", "call", "susi", "reason"],
                &["susi", "mcp", "call", "native", "list_models"],
                &["susi", "mcp", "call", "gmcp", "susi_solve"],
                &["susi", "mcp", "call", "", "reason"],
                &["susi", "mcp", "call", "filesystem", "read_file"],
            ],
        );
        assert_primes(
            Catalogs::NONE,
            &[
                &["susi", "mcp", "call", "http://127.0.0.1:9/mcp", "reason"],
                &["susi", "mcp", "call", "127.0.0.1:9", "susi_solve"],
            ],
        );
    }

    /// Readers of the registered agent capabilities prime the agent scans;
    /// crown also reads the selected model.
    #[test]
    fn capability_readers_prime_the_agent_scans() {
        assert_primes(Catalogs::AGENTS, &[&["susi", "substrate", "status"]]);
        assert_primes(
            Catalogs::AGENTS.union(Catalogs::CODING_MODELS),
            &[&["susi", "crown", "verify"], &["susi", "crown", "status"]],
        );
    }

    /// `auto prime`, missions and the commands kept at their pre-split value
    /// still prime every catalog, as before priming was split.
    #[test]
    fn auto_prime_missions_and_unclassified_commands_prime_everything() {
        assert_primes(
            Catalogs::ALL,
            &[
                &["susi", "auto", "prime"],
                &["susi", "automate", "x"],
                &["susi", "install"],
                &["susi", "gemi"],
                &["susi", "substrate-report"],
                &["susi", "sovereign-dashboard"],
                &["susi", "deep-scan"],
                &["susi", "uninstall"],
            ],
        );
    }

    /// D-SP-1: both predicates default to "nothing" (no daemon, no catalog)
    /// for a command added later. So every current command is pinned here,
    /// and the table must name exactly the subcommands clap knows, hidden
    /// ones included: a new command fails this test until it is classified.
    #[test]
    fn every_command_has_a_pinned_daemon_and_catalog_classification() {
        use Catalogs as C;
        let crown = C::AGENTS.union(C::CODING_MODELS);
        // (subcommand, argv after `susi`, requires the daemon, catalogs)
        let pins: &[(&str, &[&str], bool, Catalogs)] = &[
            ("agents", &["agents", "list"], false, C::NONE),
            ("frameworks", &["frameworks", "list"], false, C::NONE),
            ("start", &["start"], true, C::ALL),
            ("stop", &["stop"], false, C::ALL),
            ("restart", &["restart"], false, C::ALL),
            ("install", &["install"], true, C::ALL),
            (
                "install-release",
                &["install-release", "--status"],
                true,
                C::ALL,
            ),
            ("local", &["local", "build", "debug"], true, C::ALL),
            ("uninstall", &["uninstall"], false, C::ALL),
            ("mcp", &["mcp", "list"], false, C::NONE),
            ("extensions", &["extensions"], false, C::ALL),
            ("auto", &["auto", "status"], false, C::NONE),
            ("setup", &["setup"], false, C::ALL),
            ("workflow", &["workflow", "sync"], false, C::ALL),
            (
                "harness",
                &["harness", "status", "--ledger", "l"],
                true,
                C::ALL,
            ),
            ("tasks", &["tasks"], false, C::ALL),
            ("brain", &["brain"], false, C::ALL),
            ("ecosystem", &["ecosystem"], false, C::ALL),
            ("blackboard", &["blackboard"], false, C::ALL),
            ("context-graph", &["context-graph"], false, C::ALL),
            ("broker", &["broker"], false, C::ALL),
            ("telemetry", &["telemetry"], false, C::ALL),
            ("patch", &["patch", "p.json"], false, C::ALL),
            ("plan", &["plan", "goal"], false, C::ALL),
            ("privacy", &["privacy"], false, C::ALL),
            ("intent", &["intent"], false, C::ALL),
            ("ambient", &["ambient"], false, C::ALL),
            ("tx", &["tx"], false, C::ALL),
            ("substrate", &["substrate", "status"], false, C::AGENTS),
            ("crown", &["crown", "status"], false, crown),
            ("gemi", &["gemi"], true, C::ALL),
            ("status", &["status"], false, C::ALL),
            ("services", &["services"], false, C::ALL),
            ("commits", &["commits"], false, C::ALL),
            ("os", &["os"], false, C::ALL),
            ("peers", &["peers"], false, C::ALL),
            ("service-run", &["service-run", "susi-paths"], true, C::ALL),
            ("substrate-report", &["substrate-report"], true, C::ALL),
            ("bloat-audit", &["bloat-audit"], true, C::NONE),
            ("models", &["models"], false, C::CODING_MODELS),
            ("openweight", &["openweight"], false, C::NONE),
            ("frontier", &["frontier"], false, C::NONE),
            ("openrouter", &["openrouter"], false, C::ALL),
            ("keys", &["keys"], false, C::NONE),
            (
                "select-model",
                &["select-model", "m"],
                true,
                C::CODING_MODELS,
            ),
            ("deep-scan", &["deep-scan"], true, C::ALL),
            ("mcp-add", &["mcp-add", "n", "cmd"], false, C::ALL),
            (
                "verify-download-agent",
                &["verify-download-agent"],
                true,
                C::ALL,
            ),
            ("scout-model", &["scout-model", "u"], true, C::ALL),
            ("benchmark", &["benchmark"], true, C::ALL),
            ("pulse", &["pulse", "x"], false, C::ALL),
            ("automate", &["automate", "x"], true, C::ALL),
            ("review", &["review"], false, C::NONE),
            ("os-clean", &["os-clean"], false, C::NONE),
            ("audit", &["audit"], false, C::NONE),
            ("admin", &["admin", "lint"], false, C::ALL),
            ("clean", &["clean"], false, C::NONE),
            ("run", &["run", "hello"], false, C::ALL),
            ("top", &["top"], false, C::ALL),
            (
                "daemon-start",
                &["daemon-start", "--workspace", "x"],
                false,
                C::ALL,
            ),
        ];
        let mut pinned: Vec<&str> = pins.iter().map(|pin| pin.0).collect();
        pinned.sort_unstable();
        let unique = pinned.len();
        pinned.dedup();
        assert_eq!(unique, pinned.len(), "a command is pinned twice");
        let mut known: Vec<String> = Cli::command()
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .collect();
        known.sort_unstable();
        assert_eq!(
            pinned, known,
            "pin every command's daemon and catalog classification"
        );
        for &(name, argv, daemon, expected) in pins {
            let args: Vec<&str> = std::iter::once("susi")
                .chain(argv.iter().copied())
                .collect();
            let matches = Cli::command()
                .try_get_matches_from(&args)
                .unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert_eq!(matches.subcommand_name(), Some(name), "{args:?}");
            let command = Cli::try_parse_from(&args).unwrap().command.unwrap();
            assert_eq!(
                super::command_requires_daemon(&command),
                daemon,
                "daemon: {args:?}"
            );
            assert_eq!(command_catalogs(&command), expected, "catalogs: {args:?}");
        }
        // `admin` is classified per subcommand: only the compliance audit
        // ensures the daemon.
        let admin_audit = Cli::try_parse_from(["susi", "admin", "audit"])
            .unwrap()
            .command
            .unwrap();
        assert!(super::command_requires_daemon(&admin_audit));
        assert_eq!(command_catalogs(&admin_audit), C::ALL);
    }

    /// Lifecycle and status never run the CLI composition root: on a host
    /// whose daemon was down, that root alone held `susi start`, `susi
    /// status` and the daemon's own `daemon-start` for ~13 minutes.
    #[test]
    fn lifecycle_and_status_skip_the_cli_composition() {
        for cmd in [
            &["susi", "start"][..],
            &["susi", "stop"],
            &["susi", "restart"],
            &["susi", "status"],
            &["susi", "daemon-start", "--workspace", "x"],
        ] {
            assert!(skips(cmd), "{cmd:?} must skip composition");
        }
        assert!(
            !skips(&["susi", "mcp", "serve"]),
            "mcp serve composes the substrate"
        );
    }

    #[test]
    fn moved_dev_workflow_commands_refuse_without_composition_or_daemon() {
        for args in [
            &["susi", "admin", "release"][..],
            &["susi", "admin", "release", "--cut", "patch"],
            &["susi", "admin", "sync"],
            &["susi", "workflow", "guard", "commit"],
            &["susi", "workflow", "--help"],
            &["susi", "tasks"],
            &["susi", "tasks", "claim", "T-A-1", "--scope", "src"],
        ] {
            let command = Cli::try_parse_from(args).unwrap().command.unwrap();
            assert!(command_skips_composition(&command), "{args:?}");
            assert!(!super::command_requires_daemon(&command), "{args:?}");
        }
        let eval = Cli::try_parse_from([
            "susi",
            "tasks",
            "eval-corpus",
            "--corpus",
            "c.json",
            "--input",
            "i.json",
        ])
        .unwrap()
        .command
        .unwrap();
        assert!(!command_skips_composition(&eval));
    }

    #[test]
    fn audit_file_commands_skip_composition() {
        for args in [
            &["susi", "audit", "verify"][..],
            &["susi", "audit", "query", "--json"],
        ] {
            let command = Cli::try_parse_from(args).unwrap().command.unwrap();
            assert!(command_skips_composition(&command));
            assert!(!super::command_requires_daemon(&command));
        }
        assert!(!skips(&["susi", "audit"]));
    }

    /// Chat, the coding app, and one command per product are not OS commands.
    /// Peers stay on `agents` / `frameworks`. One completion stays on `run`.
    #[test]
    fn application_commands_are_not_os_commands() {
        for args in [
            &["susi", "chat"][..],
            &["susi", "code", "fix it"],
            &["susi", "aider"],
            &["susi", "openhands"],
            &["susi", "browser-use"],
            &["susi", "crewai"],
            &["susi", "haystack"],
            &["susi", "langgraph"],
            &["susi", "swe-agent"],
        ] {
            let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(cli.command.is_none(), "{args:?} must not be an OS command");
        }
        assert!(Cli::try_parse_from(["susi", "agents", "list"])
            .unwrap()
            .command
            .is_some());
        assert!(Cli::try_parse_from(["susi", "frameworks", "init", "crewai"]).is_err());
        assert!(
            Cli::try_parse_from(["susi", "frameworks", "setup", "crewai"])
                .unwrap()
                .command
                .is_some()
        );
        assert!(Cli::try_parse_from(["susi", "run", "hello"])
            .unwrap()
            .command
            .is_some());
    }

    /// A four-question quiz, a swarm narration of a web scout, and a swarm
    /// narration of the local model list are not commands. Listing local
    /// weights stays on `deep-scan`; the coding-model catalog stays on
    /// `models list`.
    #[test]
    fn narrated_scout_quiz_and_legacy_local_are_not_commands() {
        for args in [
            &["susi", "models", "local"][..],
            &["susi", "os", "manage", "model", "local"],
        ] {
            assert!(
                Cli::try_parse_from(args).is_err(),
                "{args:?} must not parse"
            );
        }
        for args in [&["susi", "eval"][..], &["susi", "mcp-scout"]] {
            let cli = Cli::try_parse_from(args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(cli.command.is_none(), "{args:?} must not be an OS command");
            assert!(
                super::super::intent_guard::refuse(
                    &<Cli as CommandFactory>::command(),
                    &cli.intent
                )
                .is_some(),
                "{args:?} must not run as a mission"
            );
        }
        assert!(Cli::try_parse_from(["susi", "deep-scan"])
            .unwrap()
            .command
            .is_some());
        assert!(Cli::try_parse_from(["susi", "models", "list"])
            .unwrap()
            .command
            .is_some());
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for rel in [
            "crates/susi-gemi/src/eval.rs",
            "crates/susi-gmcp/src/reflexes.rs",
        ] {
            assert!(
                !root.join(rel).exists(),
                "{rel} is a narrated or empty feature and must stay removed"
            );
        }
        let bundled = std::fs::read_to_string(root.join("config/config.default.json"))
            .unwrap_or_else(|e| panic!("read bundled config: {e}"));
        assert!(
            !bundled.contains("mcp_scout_pulse") && !bundled.contains("admin_pulses"),
            "the bundled scout pulse must stay removed"
        );
    }

    fn collect_help_snapshot(cmd: &mut clap::Command, path: &[String], out: &mut String) {
        let label = path.join(" ");
        let help = cmd.render_help().to_string();
        out.push_str(&format!("=== {label} ===\n{help}\n"));
        let names: Vec<String> = cmd
            .get_subcommands()
            .filter(|sub| sub.get_name() != "help")
            .map(|sub| sub.get_name().to_string())
            .collect();
        for name in names {
            let sub = cmd.find_subcommand_mut(&name).expect("subcommand exists");
            let mut next_path = path.to_vec();
            next_path.push(name);
            collect_help_snapshot(sub, &next_path, out);
        }
    }

    fn render_cli_help_snapshot() -> String {
        let mut root = <Cli as CommandFactory>::command().version(env!("CARGO_PKG_VERSION"));
        root.build();
        let raw_version = root.render_version();
        assert_eq!(
            raw_version,
            format!("susi {}\n", env!("CARGO_PKG_VERSION")),
            "--version output must report the binary's package version"
        );
        let version = raw_version.replace(env!("CARGO_PKG_VERSION"), "<VERSION>");
        let mut out = format!("=== version ===\n{version}\n");
        collect_help_snapshot(&mut root, &["susi".to_string()], &mut out);
        out.replace("\r\n", "\n")
    }

    #[test]
    fn cli_help_and_version_match_snapshot() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let snapshot_path = root.join("tests/fixtures/cli_help_snapshot.txt");
        let actual = render_cli_help_snapshot();
        let expected = std::fs::read_to_string(&snapshot_path)
            .unwrap_or_else(|e| panic!("read {}: {e}", snapshot_path.display()))
            .replace("\r\n", "\n");
        assert_eq!(
            actual, expected,
            "CLI help/version output drifted from tests/fixtures/cli_help_snapshot.txt"
        );
    }
}
