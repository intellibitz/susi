//! Per-command substrate catalog priming classification (`D-SP-1` / `D-SP-4`).
//!
//! Extracted from `cli::defs` so the clap surface in `susi-cli-defs` stays free
//! of `susi-daemon` (`Catalogs`) and command-execution modules.

use susi_daemon::discovery_pipeline::Catalogs;

use super::auto_cli;
use super::crown_cli;
use super::defs::Commands;
use super::frontier_cli;
use super::mcp_cli;
use super::model_cli;
use super::open_weight_cli;
use super::substrate_cli;

/// Which optional external catalogs `command` may read (and so may prime).
///
/// Priming probes host CLIs (`claude --version`, `python -c "import …"`,
/// Ollama `/api/tags`, OpenRouter `/models`). An explicit plane command
/// (`susi openweight list`, `susi agents doctor`, `susi mcp status`) already
/// runs its own live preflight; priming the other planes before it ran ~20
/// child processes that did not feed a single line of its output, and on
/// Windows those processes tripped the application-control popups.
///
/// `Catalogs::NONE` means `wire_cli_substrate_priming` spawns zero probe
/// processes. Used by the Ecosystem control planes and by the CLI
/// composition root (`susi_daemon::composition::wire_cli_substrate_priming`).
///
/// Every command is classified explicitly here, and
/// `defs::tests::every_command_has_a_pinned_daemon_and_catalog_classification`
/// pins each one. Commands that never prime (`status`, `brain`, `run`,
/// `mcp serve`, …) never ask; they keep `ALL`, the value every command had
/// before priming was split. The default is `NONE` (D-SP-1): a command
/// added later primes nothing until it is classified.
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
        Commands::Acp { .. } => Catalogs::NONE,
        // These three dispatch in `control_plane_cli::dispatch` before the
        // CLI composition root runs, and prime inside their own handlers
        // (`substrate_cli::PRIMED`, `crown_cli::PRIMED`, `auto prime`'s full
        // prime); these arms only describe what those handlers prime.
        Commands::Substrate { .. } => substrate_cli::PRIMED,
        Commands::Crown { .. } => crown_cli::PRIMED,
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
        // Missions, gemi, install and the rest: everything. Many of these
        // dispatch before the composition root or skip it and so never ask;
        // they keep the value they had before the default flipped.
        Commands::Start
        | Commands::Stop
        | Commands::Restart
        | Commands::Install
        | Commands::InstallRelease { .. }
        | Commands::Local { .. }
        | Commands::Uninstall
        | Commands::Extensions { .. }
        | Commands::Setup { .. }
        | Commands::Workflow { .. }
        | Commands::Harness { .. }
        | Commands::Tasks { .. }
        | Commands::Brain { .. }
        | Commands::Ecosystem { .. }
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
        | Commands::Gemi
        | Commands::Status { .. }
        | Commands::Services { .. }
        | Commands::Commits { .. }
        | Commands::Os { .. }
        | Commands::Peers { .. }
        | Commands::ServiceRun { .. }
        | Commands::SubstrateReport
        | Commands::OpenRouter { .. }
        | Commands::DeepScan
        | Commands::McpAdd { .. }
        | Commands::VerifyDownloadAgent
        | Commands::ScoutModel { .. }
        | Commands::Benchmark
        | Commands::Pulse { .. }
        | Commands::Automate { .. }
        | Commands::Admin { .. }
        | Commands::Run(..)
        | Commands::Top(..)
        | Commands::DaemonStart { .. } => Catalogs::ALL,
        // Every current variant is matched above; this arm is the default for
        // a variant added later, which primes nothing until it is classified
        // (and the pinning test fails until it is pinned).
        #[allow(unreachable_patterns)]
        _ => Catalogs::NONE,
    }
}

/// `susi models …`: only list, setup and bare `models` print the preferred
/// coding model that auto-prefer writes.
fn models_catalogs(action: Option<&model_cli::ModelCommands>) -> Catalogs {
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
fn frontier_catalogs(action: Option<&frontier_cli::FrontierCommands>) -> Catalogs {
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
fn open_weight_catalogs(action: Option<&open_weight_cli::OpenWeightCommands>) -> Catalogs {
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

/// `susi mcp …`: nothing primes an MCP catalog — core never enables a
/// server on its own (zero-UI plan A8), so list/status/doctor/setup/disable
/// read only what the user enabled. `serve` primes nothing (its client
/// waits on the handshake).
pub(crate) fn mcp_catalogs(action: Option<&mcp_cli::McpCommands>) -> Catalogs {
    use mcp_cli::McpCommands as M;
    match action {
        // A network peer reads nothing primed here. Any other target goes
        // through `call_tool_direct`, which dispatches in process whenever
        // the server is a native alias (`susi`, `native`, `gmcp`, `local`,
        // `builtin`, `core`, empty) or the tool is a registered native tool
        // (`susi_solve`, `reason`, `list_models`, ...) — work that reads the
        // selected model and the MCP set, as a mission does. Which tools are
        // native is only known at dispatch, so every non-network call
        // primes everything, as missions do.
        Some(M::Call { addr, .. }) if !mcp_cli::is_network_endpoint(addr) => Catalogs::ALL,
        Some(M::List)
        | Some(M::Status)
        | Some(M::Doctor { .. })
        | Some(M::Setup { .. })
        | Some(M::Disable { .. })
        | Some(M::Call { .. })
        | Some(M::Tools { .. })
        | Some(M::Ledger { .. })
        | Some(M::Enable { .. })
        | Some(M::Configure { .. })
        | Some(M::Reset { .. })
        | Some(M::Serve)
        | None => Catalogs::NONE,
    }
}
