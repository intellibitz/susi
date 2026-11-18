//! Clap surface for the susi CLI: top-level parser, command/subcommand enums,
//! retained at the public launcher. Implementations live in Susi Commercial Apps.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use susi_cli_defs::acp as acp_cli;
use susi_cli_defs::agent as agent_cli;
use susi_cli_defs::ambient as ambient_cli;
use susi_cli_defs::auto as auto_cli;
use susi_cli_defs::blackboard as blackboard_cli;
use susi_cli_defs::board as board_cli;
use susi_cli_defs::brain as brain_cli;
use susi_cli_defs::broker as broker_cli;
use susi_cli_defs::commits as commits_cli;
use susi_cli_defs::context_graph as context_graph_cli;
use susi_cli_defs::crown as crown_cli;
use susi_cli_defs::ecosystem as ecosystem_cli;
use susi_cli_defs::extensions as extensions_cli;
use susi_cli_defs::framework as framework_cli;
use susi_cli_defs::frontier as frontier_cli;
use susi_cli_defs::harness as harness_cli;
use susi_cli_defs::intent as intent_cli;
use susi_cli_defs::mcp as mcp_cli;
use susi_cli_defs::model as model_cli;
use susi_cli_defs::open_weight as open_weight_cli;
use susi_cli_defs::openrouter as openrouter_cli;
use susi_cli_defs::os as os_cli;
use susi_cli_defs::patch as patch_cli;
use susi_cli_defs::peers as peers_cli;
use susi_cli_defs::plan as plan_cli;
use susi_cli_defs::privacy as privacy_cli;
use susi_cli_defs::services as services_cli;
use susi_cli_defs::setup as setup_cli;
use susi_cli_defs::substrate as substrate_cli;
use susi_cli_defs::tasks as tasks_cli;
use susi_cli_defs::telemetry as telemetry_cli;
use susi_cli_defs::tx as tx_cli;

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

// Root help needs each command's public name, aliases, visibility and documentation,
// but none of its argument parsers. Generate both views from the same declarations.
macro_rules! public_commands {
    ($( $(#[$attribute:meta])* $variant:ident $({$($fields:tt)*})? $(($($args:tt)*))?, )*) => {
        #[derive(Subcommand)]
        #[allow(clippy::large_enum_variant)]
        pub(crate) enum Commands {
            $( $(#[$attribute])* $variant $({$($fields)*})? $(($($args)*))?, )*
        }

        #[derive(Subcommand)]
        pub(crate) enum CommandSummaries {
            $( $(#[$attribute])* $variant, )*
        }
    };
}

public_commands! {
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
    /// Clean up sandboxed .susi environment
    Uninstall,
    /// Manage leading MCP tool servers, or serve SUSI's native MCP stdio endpoint
    Mcp {
        #[command(subcommand)]
        action: Option<mcp_cli::McpCommands>,
    },
    /// Serve SUSI's native ACP JSON-RPC 2.0 stdio endpoint, or inspect ACP discovery
    Acp {
        #[command(subcommand)]
        action: Option<acp_cli::AcpCommands>,
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
    /// Inspect or post to the mission blackboard (posts mirror to Susi Native Board)
    #[command(name = "blackboard", visible_alias = "bb")]
    Blackboard {
        #[command(subcommand)]
        action: Option<blackboard_cli::BlackboardCommands>,
    },
    /// Collaborate through the Susi Native Board and enforce worktree isolation
    #[command(name = "board")]
    Board {
        #[command(subcommand)]
        action: board_cli::BoardCommands,
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
    Run(susi_cli_defs::run::RunArgs),
    /// Unified operator control surface: workload, resources, providers, config, federation, and evidence
    #[command(name = "top")]
    Top(susi_cli_defs::top::TopArgs),
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
    /// Cross-OS unattended encrypted vault & KeePassXC + Google Drive sync
    Vault {
        #[command(subcommand)]
        action: VaultCommands,
    },
}

#[derive(Subcommand)]
pub(crate) enum VaultCommands {
    /// Inspect vault state, OS unlock backend, Google Drive mirror, and plaintext risks (never prints secrets)
    Status {
        /// Output structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Enroll unattended host unlock (systemd-creds / DPAPI / secret-tool / keyfile) and optional KeePassXC DB/GDrive paths
    Enroll {
        /// Unlock backend: auto, systemd-creds, dpapi, secret-tool, or keyfile
        #[arg(long)]
        backend: Option<String>,
        /// KeePassXC .kdbx database path (Personal vaults refused)
        #[arg(long)]
        db: Option<std::path::PathBuf>,
        /// Portable KeePassXC .keyx / .key file path
        #[arg(long)]
        key_file: Option<std::path::PathBuf>,
        /// Local Google Drive mirror .kdbx path for two-way sync
        #[arg(long)]
        gdrive_path: Option<std::path::PathBuf>,
        /// Optional rclone remote path (e.g. gdrive:Vault/Automation.kdbx)
        #[arg(long)]
        rclone_remote: Option<String>,
        /// Also enroll KeePassXC passphrase on this host (reads stdin if piped, or prompts once)
        #[arg(long)]
        enroll_kdbx_passphrase: bool,
    },
    /// Sync Google Drive <-> local .kdbx and seal entries into ~/.susi/vault.sealed.json
    Sync {
        /// Override KeePassXC .kdbx path
        #[arg(long)]
        db: Option<std::path::PathBuf>,
        /// Google Drive sync direction: auto, pull, or push
        #[arg(long, default_value = "auto")]
        direction: String,
        /// Preview sync actions without modifying files
        #[arg(long)]
        dry_run: bool,
        /// Output structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Migrate plaintext ~/.susi/cloud.env entries into ~/.susi/vault.sealed.json
    MigrateCloudEnv {
        /// Keep plaintext cloud.env instead of clearing migrated keys
        #[arg(long)]
        keep_plaintext: bool,
    },
    /// Write a cloud/container bootstrap key (0600) for SUSI_VAULT_KEY_FILE / Kubernetes Secret
    InitCloudKey {
        /// Path to write the 32-byte hex key (0600); if omitted writes ~/.susi/cloud-bootstrap.key
        #[arg(long)]
        key_out: Option<std::path::PathBuf>,
    },
    /// Run a child command with scoped secrets from the sealed vault injected directly into its env
    Exec {
        /// Client allowlist profile: claude, codex, gemini, gh, hf, vibe, cloud, commercial, or all
        #[arg(long)]
        client: Option<String>,
        /// Command and arguments to execute (pass after `--`)
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Diagnose cross-OS KeePassXC, keystore, D-Bus, Qt headless, GPG, and SSH signing readiness
    Doctor {
        /// Output structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Prime GPG and SSH signing agents from the sealed vault (`signing` scope)
    PrimeSigning {
        /// Only preset GPG passphrase without touching SSH agent
        #[arg(long)]
        gpg_only: bool,
        /// Output structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Install the cross-OS `susi-gpg` wrapper and remediate Git `.gitconfig` signing paths
    SetupGit {
        /// Also enable `commit.gpgsign = true` in global Git config
        #[arg(long)]
        gpgsign: bool,
        /// Output structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Re-key an existing .kdbx database (e.g. Windows DPAPI) into a portable .keyx/passphrase vault
    Rekey {
        /// KeePassXC .kdbx database path
        #[arg(long)]
        db: Option<std::path::PathBuf>,
        /// Destination path for the portable `.keyx` sidecar keyfile
        #[arg(long)]
        key_file_out: Option<std::path::PathBuf>,
        /// Re-key to portable passphrase only without generating a `.keyx` keyfile
        #[arg(long)]
        no_key_file: bool,
        /// Set or rotate the portable passphrase (reads SUSI_NEW_KDBX_PASSPHRASE, stdin, or prompt)
        #[arg(long)]
        with_passphrase: bool,
        /// Output structured JSON
        #[arg(long)]
        json: bool,
    },
    /// Act as Git's `gpg.program` (auto-primes GPG passphrase before executing real gpg)
    GpgProgram {
        /// Arguments forwarded verbatim to real `gpg`
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
    #[allow(
        dead_code,
        reason = "the public parser validates these words before their unchanged argv reaches the runtime"
    )]
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
