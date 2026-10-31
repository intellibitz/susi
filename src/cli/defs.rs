//! Clap surface for the susi CLI: top-level parser, command/subcommand enums,
//! and the daemon-requirement table. Dispatch lives in `control_plane_cli`
//! (pre-boot) and `mission_cli` (post-boot).

use std::path::PathBuf;
use susi::SUSI_VERSION;
use susi_daemon::discovery_pipeline::Catalogs;

use clap::{Parser, Subcommand};

use super::agent_cli;
use super::ambient_cli;
use super::auto_cli;
use super::blackboard_cli;
use super::brain_cli;
use super::broker_cli;
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
use super::workflow_cli;

#[derive(Parser)]
#[command(name = "susi")]
#[command(version = SUSI_VERSION)]
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
    /// One end-to-end ecosystem setup: detect, consent, apply (safe to rerun)
    Setup {
        #[command(subcommand)]
        action: Option<setup_cli::SetupCommands>,
    },
    /// Check that this checkout follows the susi workflow — run this first
    Workflow {
        #[command(subcommand)]
        action: workflow_cli::WorkflowCommands,
    },
    /// Harness orchestration: author a plan, run a mission, read the cockpit
    Harness {
        #[command(subcommand)]
        action: harness_cli::HarnessCommands,
    },
    /// The shared task queue: add, claim (atomic), close (only when the check passes)
    Tasks {
        #[command(subcommand)]
        // Boxed: `add` carries several strings and would grow every `Commands`.
        action: Option<Box<tasks_cli::TaskCommands>>,
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
    Status,
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
    SovereignDashboard,
    /// Audit every Rust source in the workspace (AST-based, production code only) and target/ for bloat and hardcoded secrets, rayon-parallel across all cores
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
    /// Synchronize version consistency
    Sync,
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
    /// Provider contract checks: fixture-backed checks always;
    /// credentialed live probes when SUSI_PROVIDER_PROBE=1
    #[command(name = "provider-contract")]
    ProviderContract,
    /// The graded user-intent ladder: print the last scorecard (default),
    /// run the full ladder hermetically (--run), or just the bounded
    /// start-up slice (--slice). The daemon records failing rungs in the
    /// host outbox (--gaps); an agent adopts one as a repair task in its
    /// own checkout (--adopt-gap).
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
        /// Author the outbox gap for this intent id as a repair task in the
        /// current checkout, and mark it adopted
        #[arg(long, value_name = "ID")]
        adopt_gap: Option<String>,
    },
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
            | Commands::Status
            | Commands::Audit {
                action: Some(AuditCommands::Verify { .. } | AuditCommands::Query { .. }),
            }
    )
}

/// The release gate needs the plane bus and audit hooks, but not the CLI's
/// background indexer or catalog discovery; smoke missions compose separately.
pub(crate) fn command_uses_release_composition(command: &Commands) -> bool {
    matches!(
        command,
        Commands::Admin {
            subcommand: AdminCommands::Release { .. }
        }
    )
}

/// Mandate 32: only ensure the daemon for commands that need the background
/// substrate. Local-only admin/workspace ops must return without blocking on
/// binary integrity checks or daemon restart.
#[allow(clippy::wildcard_enum_match_arm)]
pub(crate) fn command_requires_daemon(command: &Commands) -> bool {
    match command {
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
        | Commands::Status
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
            matches!(
                subcommand,
                AdminCommands::Release { .. } | AdminCommands::Audit
            )
        }
        _ => true,
    }
}

/// The catalogs a priming command primes: exactly those whose state its
/// output (or its work) reads, and none when it reads none. Each one is
/// primed fresh in the command's own process; nothing is reused across
/// processes. Used by the Ecosystem control planes and by the CLI
/// composition root (`susi_daemon::composition::wire_cli_substrate_priming`).
///
/// A command not classified here primes every catalog, as all of them did
/// before priming was split. Commands that never prime (`status`, `brain`,
/// `run`, `mcp serve`, …) never ask.
#[allow(clippy::wildcard_enum_match_arm)]
pub(crate) fn command_catalogs(command: &Commands) -> Catalogs {
    match command {
        // Print the static catalogs, live preflights, run records and
        // their own pins; the hidden `agents worker` / `frameworks worker`
        // (every detached run, python-engine planes included) execute
        // from cloud.env and the environment. No primed state is read.
        Commands::Agents { .. } | Commands::Frameworks { .. } => Catalogs::NONE,
        Commands::OpenWeight { action } => open_weight_catalogs(action.as_ref()),
        Commands::Frontier { action } => frontier_catalogs(action.as_ref()),
        Commands::Models { action } => models_catalogs(action.as_ref()),
        Commands::Mcp { action } => mcp_catalogs(action.as_ref()),
        // These three dispatch in `control_plane_cli::dispatch` before the
        // CLI composition root runs, and prime inside their own handlers
        // (`substrate_cli::PRIMED`, `crown_cli::PRIMED`, `auto prime`'s full
        // prime); these arms only describe what those handlers prime.
        Commands::Substrate { .. } => super::substrate_cli::PRIMED,
        Commands::Crown { .. } => super::crown_cli::PRIMED,
        Commands::Auto {
            action: Some(auto_cli::AutoCommands::Prime),
        } => Catalogs::ALL,
        // A snapshot of what other processes primed.
        Commands::Auto { .. } => Catalogs::NONE,
        // `select-model` writes `selected_model_override.txt`. Coding-model
        // auto-prefer must run first: it pins a ready preferred model, so
        // every later prime keeps the user's pick instead of `prefer()`-ing
        // over it (as the daemon this command starts would at boot).
        Commands::SelectModel { .. } => Catalogs::CODING_MODELS,
        // Key presence and routing prefs; the daemon/OS summary; the
        // workspace audit log and compliance scan; the AST auditor;
        // `target/` and OS cache cleanup.
        Commands::Keys { .. }
        | Commands::Review
        | Commands::Audit { .. }
        | Commands::BloatAudit
        | Commands::Clean
        | Commands::OsClean => Catalogs::NONE,
        // Missions, the shell, gemi, install and the rest: everything.
        _ => Catalogs::ALL,
    }
}

/// `susi models …`: only list, setup and bare `models` print the preferred
/// coding model that auto-prefer writes.
pub(crate) fn models_catalogs(action: Option<&model_cli::ModelCommands>) -> Catalogs {
    use model_cli::ModelCommands as M;
    match action {
        None | Some(M::List) | Some(M::Setup { .. }) => Catalogs::CODING_MODELS,
        Some(M::Doctor { .. })
        | Some(M::Configure { .. })
        | Some(M::Reset { .. })
        | Some(M::Prefer { .. })
        | Some(M::Probe { .. }) => Catalogs::NONE,
    }
}

/// `susi frontier …`: `prefer` writes `selected_model_override.txt`
/// without writing `preferred_coding_model.txt`, so it primes coding models
/// first (else the next coding-model prime overwrites the pin). The rest
/// print the static catalog, live preflights and the plane's own pin.
pub(crate) fn frontier_catalogs(action: Option<&frontier_cli::FrontierCommands>) -> Catalogs {
    use frontier_cli::FrontierCommands as F;
    match action {
        Some(F::Prefer { .. }) => Catalogs::CODING_MODELS,
        None
        | Some(F::List)
        | Some(F::Doctor { .. })
        | Some(F::Setup { .. })
        | Some(F::Configure { .. })
        | Some(F::Reset { .. })
        | Some(F::Probe { .. }) => Catalogs::NONE,
    }
}

/// `susi openweight …`: as [`frontier_catalogs`] — `prefer` writes the
/// override, so it primes coding models first; the rest prime nothing.
pub(crate) fn open_weight_catalogs(
    action: Option<&open_weight_cli::OpenWeightCommands>,
) -> Catalogs {
    use open_weight_cli::OpenWeightCommands as O;
    match action {
        Some(O::Prefer { .. }) => Catalogs::CODING_MODELS,
        None
        | Some(O::List)
        | Some(O::Doctor { .. })
        | Some(O::Setup { .. })
        | Some(O::Pull { .. })
        | Some(O::Configure { .. })
        | Some(O::Reset { .. })
        | Some(O::Probe { .. }) => Catalogs::NONE,
    }
}

/// `susi mcp …`: the subcommands whose output reads the enabled set that
/// leading MCP auto-enable writes prime it; the rest prime nothing.
/// `disable` reports whether it removed an entry, which an auto-enable just
/// before decides. `serve` primes nothing (its client waits on the
/// handshake).
pub(crate) fn mcp_catalogs(action: Option<&mcp_cli::McpCommands>) -> Catalogs {
    use mcp_cli::McpCommands as M;
    match action {
        Some(M::List)
        | Some(M::Status)
        | Some(M::Doctor { .. })
        | Some(M::Setup { .. })
        | Some(M::Disable { .. }) => Catalogs::LEADING_MCP,
        // A network peer reads nothing primed here. Any other target goes
        // through `call_tool_direct`, which dispatches in process whenever
        // the server is a native alias (`susi`, `native`, `gmcp`, `local`,
        // `builtin`, `core`, empty) or the tool is a registered native tool
        // (`susi_solve`, `reason`, `list_models`, ...) — work that reads the
        // selected model and the MCP set, as a mission does. Which tools are
        // native is only known at dispatch, so every non-network call
        // primes everything, as missions do.
        Some(M::Call { addr, .. }) if !mcp_cli::is_network_endpoint(addr) => Catalogs::ALL,
        Some(M::Call { .. })
        | Some(M::Tools { .. })
        | Some(M::Ledger { .. })
        | Some(M::Enable { .. })
        | Some(M::Configure { .. })
        | Some(M::Reset { .. })
        | Some(M::Serve)
        | None => Catalogs::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::{command_catalogs, command_skips_composition, Catalogs, Cli};
    use clap::{CommandFactory, Parser};

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

    /// Commands that print or use the enabled MCP set prime only MCP.
    #[test]
    fn enabled_mcp_readers_prime_only_leading_mcp() {
        assert_primes(
            Catalogs::LEADING_MCP,
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

    /// `auto prime`, missions and every command not classified still prime
    /// every catalog, as before priming was split.
    #[test]
    fn auto_prime_missions_and_unclassified_commands_prime_everything() {
        assert_primes(
            Catalogs::ALL,
            &[
                &["susi", "auto", "prime"],
                &["susi", "models", "local"],
                &["susi", "automate", "x"],
                &["susi", "install"],
                &["susi", "gemi"],
                &["susi", "sovereign-dashboard"],
                &["susi", "deep-scan"],
                &["susi", "uninstall"],
            ],
        );
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
        for cmd in [&["susi", "mcp", "serve"][..], &["susi", "models", "local"]] {
            assert!(!skips(cmd), "{cmd:?} composes the substrate");
        }
    }

    #[test]
    fn release_skips_cli_catalog_composition() {
        for args in [
            &["susi", "admin", "release"][..],
            &["susi", "admin", "release", "--cut", "patch"],
        ] {
            let command = Cli::try_parse_from(args).unwrap().command.unwrap();
            assert!(super::command_uses_release_composition(&command));
            assert!(!command_skips_composition(&command));
            assert!(super::command_requires_daemon(&command));
        }
        let audit = Cli::try_parse_from(["susi", "admin", "audit"])
            .unwrap()
            .command
            .unwrap();
        assert!(!super::command_uses_release_composition(&audit));
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
}
