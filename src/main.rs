//! SUSI Engine CLI
//!
//! Command-line interface for the susi local-first AI orchestration system.

#![allow(unexpected_cfgs)]
#![allow(missing_docs)]

mod agent_cli;
mod auto_cli;
mod blackboard_cli;
mod crown_cli;
mod extensions_cli;
mod framework_cli;
mod mcp_cli;
mod model_cli;
mod substrate_cli;

use susi::SUSI_VERSION;
use susi_daemon::SusiDaemon;
use susi_gawd::ama::SusiMasterAgent;
use susi_gmcp::server::GmcpServer;
use susi_server::GemiServer;

use clap::{Parser, Subcommand};
use std::env;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "susi")]
#[command(version = SUSI_VERSION)]
#[command(about = "susi: evidence-gated AI agent substrate — GAWD, GEMI & GMCP", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Natural language intent or pulse
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    intent: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
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
enum KeyCommands {
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
enum AdminCommands {
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
fn command_requires_daemon(command: &Commands) -> bool {
    match command {
        Commands::Agents { .. } | Commands::Frameworks { .. } | Commands::Models { .. }
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

fn print_keys_status() {
    println!("Cloud API key status (values never shown):");
    for (vendor, env, present) in susi_gemi::http_provider::list_api_key_status() {
        println!(
            "  {:<12} {:<22} {}",
            vendor,
            env,
            if present { "set" } else { "missing" }
        );
    }
    println!(
        "\n{}",
        susi_gemi::routing::InferenceRouter::preference_status()
    );
    println!(
        "\nSet:    susi keys set <vendor>\nPrefer: susi keys prefer <vendor>\nFile:   {}",
        susi_gemi::http_provider::cloud_env_path().display()
    );
}

fn prompt_api_key(vendor: &str) -> io::Result<String> {
    let env_hint = susi_gemi::http_provider::resolve_vendor_env_name(vendor)
        .unwrap_or_else(|| "API_KEY".to_string());
    if !io::stdin().is_terminal() {
        // Piped: `printf '%s' "$DEEPSEEK_API_KEY" | susi keys set deepseek`
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf)?;
        let key = buf.trim().to_string();
        if key.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty API key on stdin",
            ));
        }
        return Ok(key);
    }
    eprint!("Enter API key for {} ({}): ", vendor, env_hint);
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    let key = line.trim().to_string();
    if key.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "API key must not be empty",
        ));
    }
    Ok(key)
}

fn read_stdin_bounded() -> io::Result<Option<String>> {
    let stdin = io::stdin();
    let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let max_size = cfg.max_stdin_size_bytes();
    // Read one past the limit so exact-sized payloads are accepted and oversize
    // inputs are distinguishable from a full-but-valid buffer.
    let mut buffer = Vec::new();
    let mut limited = stdin.take((max_size as u64).saturating_add(1));
    limited.read_to_end(&mut buffer)?;
    if buffer.len() > max_size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("Input exceeds {} bytes limit", max_size),
        ));
    }
    let content =
        String::from_utf8(buffer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let trimmed = content.trim();
    Ok(if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    })
}

fn get_home_dir() -> PathBuf {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn glass_box_callback(piece: String) {
    print!("{}", piece);
    let _ = io::stdout().flush();
}

fn run_shell(workspace: &Path) {
    use susi_gawd::queue::SubstratePulseQueue;
    let queue = SubstratePulseQueue::global();
    let ama = SusiMasterAgent::new();

    println!(
        "SUSI Pulse Shell v{} (Glass Box Telemetry Mode Active)",
        SUSI_VERSION
    );
    println!(
        "Enter pulses to interact with the substrate. Pulses are queued and processed in order."
    );
    println!("Type 'exit' to quit.");

    std::thread::spawn(move || {
        queue.register_consumer();
        loop {
            let pulse = queue.pop_blocking();
            let _ = ama.solve_stream(
                &pulse.intent,
                &pulse.workspace,
                &pulse.version,
                &glass_box_callback,
            );
        }
    });

    loop {
        print!("susi> ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "exit" || trimmed == "quit" {
                break;
            }
            let _ = queue.ingest(trimmed, workspace, SUSI_VERSION);
        } else {
            break;
        }
    }
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    // External executors/frameworks do not need model provisioning, daemon boot, or self-deployment.
    if let Some(Commands::Extensions { action }) = cli.command {
        return match extensions_cli::execute(action) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Auto { action }) = cli.command {
        susi_gemi::http_provider::apply_cloud_env_file();
        let substrate = susi_paths::SusiDirs::substrate_home();
        let _ = std::fs::create_dir_all(&substrate);
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| auto_cli::execute(action, &cwd))
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Blackboard { action }) = cli.command {
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| blackboard_cli::execute(action, &cwd))
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Substrate { action }) = cli.command {
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| substrate_cli::execute(action, &cwd))
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Crown { action }) = cli.command {
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| crown_cli::execute(action, &cwd))
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Agents { action }) = cli.command {
        susi_gemi::http_provider::apply_cloud_env_file();
        let substrate = susi_paths::SusiDirs::substrate_home();
        let _ = std::fs::create_dir_all(&substrate);
        susi_daemon::auto_discovery::auto_prime_ecosystem(&substrate);
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| agent_cli::execute(action, &cwd))
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Frameworks { action }) = cli.command {
        susi_gemi::http_provider::apply_cloud_env_file();
        let substrate = susi_paths::SusiDirs::substrate_home();
        let _ = std::fs::create_dir_all(&substrate);
        susi_daemon::auto_discovery::auto_prime_ecosystem(&substrate);
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| framework_cli::execute(action, &cwd))
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Models {
        action: Some(model_cli::ModelCommands::Local),
    }) = &cli.command
    {
        // Fall through to substrate boot + mission path below.
    } else if let Some(Commands::Models { action }) = cli.command {
        susi_gemi::http_provider::apply_cloud_env_file();
        let substrate = susi_paths::SusiDirs::substrate_home();
        let _ = std::fs::create_dir_all(&substrate);
        susi_daemon::auto_discovery::auto_prime_ecosystem(&substrate);
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| model_cli::execute(action, &cwd))
        {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }
    if let Some(Commands::Mcp {
        action: Some(mcp_cli::McpCommands::Serve) | None,
    }) = &cli.command
    {
        // Fall through to substrate boot + stdio MCP serve.
    } else if let Some(Commands::Mcp { action }) = cli.command {
        susi_gemi::http_provider::apply_cloud_env_file();
        let substrate = susi_paths::SusiDirs::substrate_home();
        let _ = std::fs::create_dir_all(&substrate);
        susi_daemon::auto_discovery::auto_prime_ecosystem(&substrate);
        return match env::current_dir()
            .map_err(anyhow::Error::from)
            .and_then(|cwd| mcp_cli::execute(action, &cwd))
        {
            Ok(true) => {
                // Should not happen for manage commands.
                std::process::ExitCode::SUCCESS
            }
            Ok(false) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("{}", susi_agents::external::redact(&e.to_string()));
                std::process::ExitCode::FAILURE
            }
        };
    }

    // Must run before anything can touch susi_tools::ToolRegistry (which
    // panics on first use if this hasn't happened yet) - see
    // gmcp::tools::SusiEngineHooks and susi_tools::hooks for why this
    // indirection exists instead of a direct dependency.
    susi_tools::hooks::init(Box::new(susi::hooks::SusiEngineHooks));

    susi_sandbox::auto_install::push_to_hardware_if_dev_build();
    // Zero-config: seed/load extension packs before catalogs or vendor resolution.
    let _ = susi_sandbox::extensions::ensure_extensions_substrate();
    // Zero-config: load ~/.susi/cloud.env before any inference/routing so
    // vendor keys work without editing config.json (and without a login shell).
    susi_gemi::http_provider::apply_cloud_env_file();
    // Zero-config: auto-enable ready MCP / prefer coding models / admit peers.
    let substrate = susi_paths::SusiDirs::substrate_home();
    let _ = std::fs::create_dir_all(&substrate);
    susi_daemon::auto_discovery::auto_prime_ecosystem(&substrate);
    #[cfg(all(feature = "tokio-console", tokio_unstable))]
    console_subscriber::init();

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let _home = get_home_dir();
    let global_dir = susi_paths::SusiDirs::config_dir();
    let _ = std::fs::create_dir_all(&global_dir);

    let file_appender = tracing_appender::rolling::never(&global_dir, "audit.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,susi=debug"));

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(true)
        .with_line_number(true);

    let json_file_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking);

    let _ = tracing_subscriber::registry()
        .with(env_filter)
        .with(stdout_layer)
        .with(json_file_layer)
        .try_init();

    // Boot dynamic kernel assembly (Dynamic Self-Assembly Axiom)
    let _ = susi_gawd::kernel_loader::SubstrateKernelLoader::boot_kernel(&cwd);

    // Mandate 12: Hardware Authority - Force initialize Rayon thread pool to saturate all cores
    let num_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(num_cpus)
        .build_global();
    info!(
        "[Hardware Authority] Rayon thread pool initialized with {} threads.",
        num_cpus
    );

    let mut exit_code = std::process::ExitCode::SUCCESS;

    let needs_daemon = match &cli.command {
        Some(cmd) => command_requires_daemon(cmd),
        None => true, // bare intent, stdin pipe, or interactive shell
    };
    if needs_daemon {
        SusiDaemon::ensure_daemon_running(&cwd, &global_dir);
    }

    if let Some(command) = cli.command {
        let ama = SusiMasterAgent::new();
        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        match command {
            Commands::Agents { .. }
            | Commands::Frameworks { .. }
            | Commands::Extensions { .. }
            | Commands::Auto { .. }
            | Commands::Blackboard { .. }
            | Commands::Substrate { .. }
            | Commands::Crown { .. } => {
                // Handled before substrate boot above.
            }
            Commands::Start => {
                control_plane_start(&cwd, &global_dir);
            }
            Commands::Stop => {
                control_plane_stop(&global_dir);
            }
            Commands::Restart => {
                println!("[SUSI Daemon] Restarting host contract...");
                control_plane_stop(&global_dir);
                // ensure_daemon_running was skipped for Restart; bring it back up.
                SusiDaemon::ensure_daemon_running(&cwd, &global_dir);
                control_plane_start(&cwd, &global_dir);
            }
            Commands::Shell => run_shell(&cwd),
            Commands::Install => {
                println!("[SUBSTRATE PROVISIONING: Axiomatic Initialization]");
                let answer = ama.solve_clean(&cfg.admin_pulses().install_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);

                println!("\n[AGGRESSIVE PRIMING: Enqueuing Optimal Substrate]");
                println!(
                    "- The daemon will autonomously provision the highest-tier model compatible with your hardware."
                );
                println!("- This pulse runs in the background. Check progress with 'susi status'.");

                println!("\n[SOVEREIGN HANDSHAKE]");
                exit_code = ama
                    .solve_stream_report("identity", &cwd, SUSI_VERSION, &glass_box_callback)
                    .exit_code();
            }
            Commands::Uninstall => {
                // Deterministic teardown — an agent pulse cannot guarantee
                // the binary/service/state are actually gone, and reinstall
                // must work after every uninstall.
                let bin_dir = global_dir.join("bin");
                let mut steps: Vec<String> = Vec::new();

                let daemons_stopped = SusiDaemon::stop_all_daemons(&global_dir);
                if daemons_stopped > 0 {
                    steps.push(format!("stopped {} daemon process(es)", daemons_stopped));
                }

                // Boot-persistent service registrations (SUSI_ALWAYS_ON path).
                #[cfg(target_os = "linux")]
                {
                    let unit = env::var("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default()
                        .join(".config/systemd/user/susi.service");
                    if unit.exists() {
                        let _ = Command::new("systemctl")
                            .args(["--user", "disable", "--now", "susi.service"])
                            .status();
                        let _ = fs::remove_file(&unit);
                        let _ = Command::new("systemctl")
                            .args(["--user", "daemon-reload"])
                            .status();
                        steps.push("removed systemd user service".to_string());
                    }
                }
                #[cfg(target_os = "macos")]
                {
                    let plist = env::var("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_default()
                        .join("Library/LaunchAgents/com.susi.daemon.plist");
                    if plist.exists() {
                        let _ = Command::new("launchctl").arg("unload").arg(&plist).status();
                        let _ = fs::remove_file(&plist);
                        steps.push("removed launchd agent".to_string());
                    }
                }

                // The binary itself plus runtime state that would make a
                // reinstall see a ghost install.
                for stale in ["substrate.lock", "binary.hash", "binary.hash.cache"] {
                    let _ = fs::remove_file(global_dir.join(stale));
                }
                if bin_dir.exists() {
                    match fs::remove_dir_all(&bin_dir) {
                        Ok(()) => steps.push(format!("removed {}", bin_dir.display())),
                        Err(e) => {
                            steps.push(format!("could not remove {}: {}", bin_dir.display(), e))
                        }
                    }
                }

                if steps.is_empty() {
                    println!("susi is not installed (nothing to remove).");
                } else {
                    println!("susi uninstalled:");
                    for s in &steps {
                        println!("  - {}", s);
                    }
                    println!(
                        "Preserved user data in {} (config, models, logs) — delete it manually for a full purge.",
                        global_dir.display()
                    );
                }
            }
            Commands::Mcp {
                action: Some(mcp_cli::McpCommands::Serve) | None,
            } => GmcpServer::run_stdio(&cwd, SUSI_VERSION),
            Commands::Mcp { .. } => {
                // Leading MCP manage commands handled before substrate boot.
            }
            Commands::Gemi => {
                // Degrade to bundled defaults rather than panic if
                // config.json is torn by a concurrent writer.
                let gemi_cfg =
                    susi_sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();
                let bind_address: String = gemi_cfg
                    .get("bind_address")
                    .unwrap_or_else(|| "127.0.0.1".to_string());
                let listener = std::net::TcpListener::bind(format!(
                    "{}:{}",
                    bind_address,
                    gemi_cfg.gemi_port()
                ))
                .expect("Failed to bind GEMI port");
                GemiServer::start_http_server(cwd.clone(), listener);
            }
            Commands::Status => {
                let id_file = global_dir.join("identity.key");
                if id_file.exists() {
                    if let Ok(id) = std::fs::read_to_string(id_file) {
                        println!("[SUBSTRATE IDENTITY]: {}", id.trim());
                    }
                }
                let answer = ama.solve_clean("status", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::SovereignDashboard => {
                // Calls the compiled-constants report (`AlphaSelf::RULES`/
                // `COMPONENTS`, baked into the binary) directly, not
                // `solve_clean`'s generic swarm/LLM pipeline: this command's
                // whole purpose is to report the substrate's own state, and
                // an LLM asked to narrate "how healthy is the substrate"
                // will produce plausible-sounding but unverified figures
                // (observed live: fabricated "100%" subsystem health
                // claims with no measurement behind them - a direct
                // violation of Mandate 2, No Hallucinations). This function
                // existed but was never wired to any command before now.
                match ama.generate_substrate_report(&cwd) {
                    Ok(report) => println!("{}", report),
                    Err(e) => eprintln!("[SUSI] Failed to generate substrate report: {}", e),
                }
            }
            Commands::BloatAudit => {
                let answer = ama.solve_clean("bloat_audit", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Models {
                action: Some(model_cli::ModelCommands::Local),
            } => {
                let answer = ama.solve_clean("models", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Models { .. } => {
                // Control-plane models commands handled before substrate boot.
            }
            Commands::SelectModel { model } => {
                let intent = cfg.admin_pulses().select_model_pulse.replace("{}", &model);
                let answer = ama.solve_clean(&intent, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::DeepScan => {
                let answer =
                    ama.solve_clean(&cfg.admin_pulses().deep_scan_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::McpScout => {
                let answer =
                    ama.solve_clean(&cfg.admin_pulses().mcp_scout_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::McpAdd {
                name,
                command_or_url,
                args,
            } => {
                let res = susi_tools::GmcpClient::admit_mcp_server(&name, &command_or_url, &args);
                println!("{}", res);
                if res.starts_with("SUCCESS") {
                    // Hot-plug into the live capability registry when possible.
                    let registry = susi_core::registry::CapabilityRegistry::global();
                    susi_gmcp::mcp_wrapper::auto_discover_mcp(registry);
                    susi_gemi::mcp_provider::register_mcp_inference_providers(registry);
                } else {
                    std::process::exit(1);
                }
            }
            Commands::Pulse { intent } => {
                let intent_str = intent.join(" ");
                match susi_gawd::admin::SusiAdmin::ingest_natural_intent(&cwd, &intent_str) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => {
                        eprintln!("Pulse ingestion failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::Automate { intent } => {
                let intent_str = intent.join(" ");
                if intent_str.trim().is_empty() {
                    eprintln!("Usage: susi automate <intent…>");
                    std::process::exit(1);
                }
                // First-class automation surface: evidence-gated swarm solve (not pulse ingest).
                let answer = ama.solve_clean(&intent_str, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Accept => {
                match susi_sandbox::manager::IntentBundleManager::accept_all(&cwd) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => {
                        eprintln!("Accept failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::Undo => {
                match susi_sandbox::manager::IntentBundleManager::rollback_all(&cwd) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => {
                        eprintln!("Undo failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::Review => {
                print_golden_rule_summary(&cwd, &global_dir);
            }
            Commands::OsClean => {
                let msg = susi_gemi::hardware::HardwareProfiler::execute_os_clean();
                println!("{}", msg);
            }
            Commands::Audit => {
                let answer = ama.solve_clean(&cfg.admin_pulses().audit_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Keys { action } => match action {
                None | Some(KeyCommands::List) => print_keys_status(),
                Some(KeyCommands::Set { vendor, api_key }) => {
                    let key = match api_key {
                        Some(k) if !k.trim().is_empty() => k,
                        _ => match prompt_api_key(&vendor) {
                            Ok(k) => k,
                            Err(e) => {
                                eprintln!("Failed to read API key: {}", e);
                                std::process::exit(1);
                            }
                        },
                    };
                    match susi_gemi::http_provider::register_api_key(&vendor, &key) {
                        Ok(msg) => println!("{}", msg),
                        Err(e) => {
                            eprintln!("Key registration failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Some(KeyCommands::Prefer { vendor, clear }) => {
                    if clear {
                        match susi_gemi::routing::InferenceRouter::clear_preferred_cloud() {
                            Ok(msg) => println!("{}", msg),
                            Err(e) => {
                                eprintln!("{}", e);
                                std::process::exit(1);
                            }
                        }
                    } else if let Some(v) = vendor {
                        match susi_gemi::routing::InferenceRouter::set_preferred_cloud(&v) {
                            Ok(msg) => println!("{}", msg),
                            Err(e) => {
                                eprintln!("{}", e);
                                std::process::exit(1);
                            }
                        }
                    } else {
                        println!(
                            "{}",
                            susi_gemi::routing::InferenceRouter::preference_status()
                        );
                    }
                }
                Some(KeyCommands::Remove { vendor }) => {
                    match susi_gemi::http_provider::remove_api_key(&vendor) {
                        Ok(msg) => println!("{}", msg),
                        Err(e) => {
                            eprintln!("Key removal failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            },
            Commands::Admin { subcommand } => match subcommand {
                AdminCommands::Sync => {
                    match susi_gawd::admin::SusiAdmin::enforce_version_consistency(&cwd) {
                        Ok(v) => println!("Version synchronization complete: v{}", v),
                        Err(e) => {
                            eprintln!("Sync failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                AdminCommands::Pulse { intent } => {
                    let intent_str = intent.join(" ");
                    match susi_gawd::admin::SusiAdmin::ingest_natural_intent(&cwd, &intent_str) {
                        Ok(msg) => println!("{}", msg),
                        Err(e) => {
                            eprintln!("Pulse ingestion failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                AdminCommands::Audit => {
                    // Use fast static compliance audit instead of LLM agent swarm
                    match susi_gawd::admin::SusiAdmin::audit_compliance(&cwd, Some("push")) {
                        Ok(report) => println!("{}", report),
                        Err(e) => {
                            eprintln!("Compliance audit failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                AdminCommands::Verify => {
                    let answer =
                        ama.solve_clean(&cfg.admin_pulses().verify_pulse, &cwd, SUSI_VERSION);
                    println!("{}", answer);
                }
                AdminCommands::Release { cut } => {
                    match susi_gawd::admin::SusiAdmin::execute_release(&cwd, cut) {
                        Ok(msg) => println!("{}", msg),
                        Err(e) => {
                            eprintln!("Release failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                AdminCommands::Lint => {
                    let answer =
                        ama.solve_clean(&cfg.admin_pulses().lint_pulse, &cwd, SUSI_VERSION);
                    println!("{}", answer);
                }
                AdminCommands::AuditDeps => {
                    let answer =
                        ama.solve_clean(&cfg.admin_pulses().audit_deps_pulse, &cwd, SUSI_VERSION);
                    println!("{}", answer);
                }
                AdminCommands::Reload => {
                    match susi_sandbox::manager::SusiConfig::reload(&global_dir) {
                        Ok(reloaded) => {
                            println!(
                                "Dynamic configuration reloaded successfully from {}.",
                                global_dir.join("config.json").display()
                            );
                            println!("- Engine: {}", reloaded.default_engine());
                            println!("- Model: {}", reloaded.default_model());
                            println!(
                                "- Model Ladder Steps: {}",
                                susi_gemi::hf_discovery::resolve_model_ladder(&reloaded).len()
                            );
                            println!(
                                "- MCP Bootstrap Servers: {}",
                                reloaded
                                    .bootstrap_mcp_servers::<Vec<serde_json::Value>>()
                                    .len()
                            );
                        }
                        Err(e) => {
                            eprintln!("Config reload failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            },
            Commands::VerifyDownloadAgent => {
                match susi_gemi::models::ModelManager::verify_and_provision_32b_and_72b_models(&cwd)
                {
                    Ok(report) => {
                        println!("=== SUSI Model Download Agent & Network Verification Report ===");
                        println!("- Network Status: {}", report.network_status);
                        println!("- Download Agent Active: {}", report.download_agent_active);
                        println!(
                            "- Total Discovered Models on System: {}",
                            report.total_discovered_on_system
                        );
                        println!("\nModel Provisioning Steps:");
                        let mut all_verified = !report.steps.is_empty();
                        for step in &report.steps {
                            println!(
                                "  [Step {}] {} ({})",
                                step.step, step.model_label, step.hf_repo
                            );
                            println!("    - Status: {}", step.status);
                            println!("    - Path: {}", step.path);
                            println!(
                                "    - Bytes: {} / {} ({:.1}%)",
                                step.bytes_downloaded, step.expected_bytes, step.percentage
                            );
                            if !step.status.contains("COMPLETED") {
                                all_verified = false;
                            }
                        }
                        if all_verified {
                            println!(
                                "\nSUCCESS: 32b and 72b model download agent verified and fully operational."
                            );
                        } else {
                            println!(
                                "\nINCOMPLETE: one or more provisioning steps are not COMPLETED_VERIFIED."
                            );
                            std::process::exit(1);
                        }
                    }
                    Err(e) => {
                        eprintln!("Verification failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::ScoutModel { url } => {
                println!(
                    "[Substrate Download Agent] Connecting to {} in foreground...",
                    cfg.hf_base_url()
                );
                let res = susi_gemi::models::ModelManager::install_model(&url);
                println!("{}", res);
            }
            Commands::Eval => {
                let report = susi_gemi::eval::EvalRunner::run_evaluations(&cwd);
                println!("{}", report);
            }
            Commands::Benchmark => {
                let report = susi_gemi::benchmark::BenchmarkRunner::run_and_render(&cwd);
                println!("{}", report);
            }
            Commands::Clean => match std::fs::remove_dir_all(cwd.join("target")) {
                Ok(()) => println!("Workspace build artifacts cleaned."),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    println!("No target/ directory to clean.");
                }
                Err(e) => {
                    eprintln!("Clean failed: {}", e);
                    std::process::exit(1);
                }
            },
            Commands::DaemonStart { workspace: _ } => {
                // Always host-scoped; ignore any project cwd passed for compat.
                SusiDaemon::run_daemon_loop(susi_paths::SusiDirs::substrate_home(), global_dir);
            }
        }
    } else if !cli.intent.is_empty() {
        let goal = cli.intent.join(" ");

        let ama = SusiMasterAgent::new();
        match susi_gawd::admin::SusiAdmin::ingest_natural_intent(&cwd, &goal) {
            Ok(msg) => {
                info!("Natural intent ingested successfully: {}", msg);
            }
            Err(e) => {
                warn!(
                    "Natural intent ingestion failed: {}. Falling back to direct swarm solving.",
                    e
                );
            }
        }
        exit_code = ama
            .solve_stream_report(&goal, &cwd, SUSI_VERSION, &glass_box_callback)
            .exit_code();
        let _ = io::stdout().flush();
    } else if !io::stdin().is_terminal() {
        match read_stdin_bounded() {
            Ok(Some(input)) => {
                let ama = SusiMasterAgent::new();
                exit_code = ama
                    .solve_stream_report(&input, &cwd, SUSI_VERSION, &glass_box_callback)
                    .exit_code();
                let _ = io::stdout().flush();
            }
            Ok(None) => (),
            Err(e) => {
                error!("stdin error: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        // No command and no intent provided -> Launch persistent SUSI Pulse Shell
        run_shell(&cwd);
    }
    exit_code
}

fn control_plane_stop(global_dir: &Path) {
    let killed = SusiDaemon::stop_all_daemons(global_dir);
    // Give listeners a moment to release the host-contract ports.
    for _ in 0..20 {
        if !SusiDaemon::host_contract_tcp_ready() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if killed > 0 {
        println!(
            "[SUSI Daemon] Stopped {} process(es). Host-contract ports released.",
            killed
        );
    } else if SusiDaemon::host_contract_tcp_ready() {
        eprintln!(
            "[SUSI Daemon] No lock/unit found, but ports 9090/9091/9093 are still listening."
        );
        std::process::exit(1);
    } else {
        println!("[SUSI Daemon] Already stopped.");
    }
}

fn control_plane_start(cwd: &Path, global_dir: &Path) {
    match SusiDaemon::check_status(cwd, global_dir) {
        Some(pid) if SusiDaemon::host_contract_ready() => {
            println!("[SUSI Daemon] Running (PID: {}).", pid);
            println!("{}", SusiDaemon::host_contract_endpoints_report());
            let _ = susi_sandbox::manager::SusiConfig::ensure_api_auth_token_seeded();
            println!(
                "Auth: Bearer token in {} (required for HTTP clients)",
                susi_paths::SusiDirs::config_dir()
                    .join("api_token")
                    .display()
            );
        }
        Some(pid) => {
            // PID alive but ports not yet (or no longer) bound.
            if SusiDaemon::wait_for_host_contract(std::time::Duration::from_secs(5)) {
                println!("[SUSI Daemon] Running (PID: {}).", pid);
                println!("{}", SusiDaemon::host_contract_endpoints_report());
                let _ = susi_sandbox::manager::SusiConfig::ensure_api_auth_token_seeded();
                println!(
                    "Auth: Bearer token in {} (required for HTTP clients)",
                    susi_paths::SusiDirs::config_dir()
                        .join("api_token")
                        .display()
                );
            } else {
                eprintln!(
                    "[SUSI Daemon] Process {} is up but host-contract ports 9090–9093 are not ready.",
                    pid
                );
                std::process::exit(1);
            }
        }
        None => {
            eprintln!(
                "[SUSI Daemon] Failed to start. Host-contract ports 9090–9093 are not listening."
            );
            std::process::exit(1);
        }
    }
}

fn print_golden_rule_summary(workspace: &Path, global_dir: &Path) {
    use susi_gemi::hardware::HardwareProfiler;
    use susi_sandbox::manager::IntentBundleManager;

    println!("=== SUSI SUBSTRATE SUMMARY ===");
    let os_report = HardwareProfiler::audit_os_environment_care();
    let staged = IntentBundleManager::get_staged_bundles(workspace);

    match SusiDaemon::check_status(workspace, global_dir) {
        Some(pid) => println!("- Global Daemon: Active (PID: {})", pid),
        None => println!("- Global Daemon: Inactive"),
    }
    println!(
        "- Environment Care ({}) : Reclaimable {}",
        os_report.os_name, os_report.reclaimable_cache_formatted
    );
    println!(
        "- Local Workspace ({}) : {} Staged Intent Bundles",
        workspace.display(),
        staged.len()
    );

    if !staged.is_empty() {
        println!("\nStaged Intent Bundles:");
        for b in &staged {
            println!(
                "  * [{}] {} (Fixes: {})",
                if b.is_applied() { "APPLIED" } else { "STAGED" },
                b.title(),
                b.staged_fixes.len()
            );
        }
    }

    println!("\nQuick Action Commands:");
    println!("  [1] Run 'susi accept'   -> Merge all staged workspace fixes");
    println!("  [2] Run 'susi os-clean' -> Reclaim OS package and build cache space");
    println!("  [3] Run 'susi undo'     -> Discard and rollback staged fixes");
}
