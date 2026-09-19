#![allow(unexpected_cfgs)]
use susi_engine::daemon::SusiDaemon;
use susi_engine::gawd::ama::SusiMasterAgent;
use susi_engine::gemi::server::GemiServer;
use susi_engine::gmcp::server::GmcpServer;
use susi_engine::SUSI_VERSION;

use clap::{Parser, Subcommand};
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

#[derive(Parser)]
#[command(name = "susi")]
#[command(version = SUSI_VERSION)]
#[command(about = "EAI: Exponential Intelligence for Any AI - GAWD, GEMI & GMCP Multi-Agent Engine", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Natural language intent or pulse
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    intent: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Ensure the global susi daemon is running and report its status
    Start,
    /// Start persistent SUSI Pulse Shell
    Shell,
    /// Initialize sandboxed .susi environment
    Install,
    /// Clean up sandboxed .susi environment
    Uninstall,
    /// Start native MCP server
    Mcp,
    /// Start GEMI REST server
    Gemi,
    /// Inspect workspace health report
    Status,
    /// Report on autonomous invisible work performed by the substrate
    SovereignDashboard,
    /// Recursively audit src/ (AST-based) and target/ for bloat and hardcoded secrets, rayon-parallel across all cores
    #[command(name = "bloat-audit")]
    BloatAudit,
    /// List available models
    Models,
    /// Select or override active model
    SelectModel { model: String },
    /// Parallel deep scan of substrate home for local models
    DeepScan,
    /// Autonomous web-scouting of open-source MCP servers
    McpScout,
    /// Verify model download agent, network status, and 32b/72b model provisioning
    #[command(name = "verify-download-agent")]
    VerifyDownloadAgent,
    /// Scout or install model substrate via live foreground network stream
    #[command(name = "scout-model")]
    ScoutModel { url: String },
    /// Measure real local inference latency/tokens-per-sec, and compare
    /// against a cloud endpoint if SUSI_BENCH_CLOUD_API_BASE is set
    Benchmark,
    /// Ingest a natural language intent into sovereign memory (EVIDENCE.md)
    Pulse {
        #[arg(trailing_var_arg = true)]
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
    /// Internal daemon start (Called by ensure_daemon_running)
    DaemonStart {
        #[arg(long)]
        workspace: String,
    },
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
    /// Full release orchestration
    Release,
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
        Commands::Clean
        | Commands::Review
        | Commands::Accept
        | Commands::Undo
        | Commands::OsClean
        | Commands::Pulse { .. }
        | Commands::DaemonStart { .. } => false,
        Commands::Admin { subcommand } => {
            matches!(subcommand, AdminCommands::Release | AdminCommands::Audit)
        }
        _ => true,
    }
}

fn read_stdin_bounded() -> io::Result<Option<String>> {
    let stdin = io::stdin();
    let cfg = susi_engine::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
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
    use susi_engine::gawd::queue::SubstratePulseQueue;
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

fn main() {
    #[cfg(tokio_unstable)]
    console_subscriber::init();

    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let _home = get_home_dir();
    let global_dir = susi_engine::sandbox::xdg::SusiDirs::config_dir();
    let _ = std::fs::create_dir_all(&global_dir);

    let file_appender = tracing_appender::rolling::never(&global_dir, "audit.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,susi_engine=debug"));

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
    let _ = susi_engine::gawd::kernel_loader::SubstrateKernelLoader::boot_kernel(&cwd);

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

    let cli = Cli::parse();

    let needs_daemon = match &cli.command {
        Some(cmd) => command_requires_daemon(cmd),
        None => true, // bare intent, stdin pipe, or interactive shell
    };
    if needs_daemon {
        SusiDaemon::ensure_daemon_running(&cwd, &global_dir);
    }

    if let Some(command) = cli.command {
        let ama = SusiMasterAgent::new();
        let cfg = susi_engine::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        match command {
            Commands::Start => match SusiDaemon::check_status(&cwd, &global_dir) {
                Some(pid) => println!("[SUSI Daemon] Running (PID: {}).", pid),
                None => println!(
                    "[SUSI Daemon] Failed to start. Check ~/.susi/audit.log for details."
                ),
            },
            Commands::Shell => run_shell(&cwd),
            Commands::Install => {
                println!("[SUBSTRATE PROVISIONING: Axiomatic Initialization]");
                let answer = ama.solve_clean(&cfg.admin_pulses().install_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);

                println!("\n[AGGRESSIVE PRIMING: Enqueuing Optimal Substrate]");
                println!("- The daemon will autonomously provision the highest-tier model compatible with your hardware.");
                println!("- This pulse runs in the background. Check progress with 'susi status'.");

                println!("\n[SOVEREIGN HANDSHAKE]");
                let _ = ama.solve_stream("identity", &cwd, SUSI_VERSION, &glass_box_callback);
            }
            Commands::Uninstall => {
                let answer =
                    ama.solve_clean(&cfg.admin_pulses().uninstall_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Mcp => GmcpServer::run_stdio(&cwd, SUSI_VERSION),
            Commands::Gemi => {
                // Degrade to bundled defaults rather than panic if
                // config.json is torn by a concurrent writer.
                let gemi_cfg =
                    susi_engine::sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();
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
            Commands::Models => {
                let answer = ama.solve_clean("models", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::SelectModel { model } => {
                let intent = cfg.admin_pulses().select_model_pulse.replace("{}", &model);
                let answer = ama.solve_clean(&intent, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::DeepScan => {
                let answer = ama.solve_clean(&cfg.admin_pulses().deep_scan_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::McpScout => {
                let answer = ama.solve_clean(&cfg.admin_pulses().mcp_scout_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Pulse { intent } => {
                let intent_str = intent.join(" ");
                match susi_engine::daemon::admin::SusiAdmin::ingest_natural_intent(
                    &cwd,
                    &intent_str,
                ) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => {
                        eprintln!("Pulse ingestion failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::Accept => {
                match susi_engine::sandbox::manager::IntentBundleManager::accept_all(&cwd) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => {
                        eprintln!("Accept failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Commands::Undo => {
                match susi_engine::sandbox::manager::IntentBundleManager::rollback_all(&cwd) {
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
                let msg = susi_engine::gemi::hardware::HardwareProfiler::execute_os_clean();
                println!("{}", msg);
            }
            Commands::Audit => {
                let answer = ama.solve_clean(&cfg.admin_pulses().audit_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Admin { subcommand } => match subcommand {
                AdminCommands::Sync => {
                    match susi_engine::daemon::admin::SusiAdmin::enforce_version_consistency(&cwd)
                    {
                        Ok(v) => println!("Version synchronization complete: v{}", v),
                        Err(e) => {
                            eprintln!("Sync failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                AdminCommands::Pulse { intent } => {
                    let intent_str = intent.join(" ");
                    match susi_engine::daemon::admin::SusiAdmin::ingest_natural_intent(
                        &cwd,
                        &intent_str,
                    ) {
                        Ok(msg) => println!("{}", msg),
                        Err(e) => {
                            eprintln!("Pulse ingestion failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                AdminCommands::Audit => {
                    let answer =
                        ama.solve_clean(&cfg.admin_pulses().audit_pulse, &cwd, SUSI_VERSION);
                    println!("{}", answer);
                }
                AdminCommands::Verify => {
                    let answer =
                        ama.solve_clean(&cfg.admin_pulses().verify_pulse, &cwd, SUSI_VERSION);
                    println!("{}", answer);
                }
                AdminCommands::Release => {
                    match susi_engine::daemon::admin::SusiAdmin::execute_release(&cwd) {
                        Ok(msg) => println!("{}", msg),
                        Err(e) => {
                            eprintln!("Release failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                AdminCommands::Lint => {
                    let answer = ama.solve_clean(&cfg.admin_pulses().lint_pulse, &cwd, SUSI_VERSION);
                    println!("{}", answer);
                }
                AdminCommands::AuditDeps => {
                    let answer =
                        ama.solve_clean(&cfg.admin_pulses().audit_deps_pulse, &cwd, SUSI_VERSION);
                    println!("{}", answer);
                }
                AdminCommands::Reload => {
                    match susi_engine::sandbox::manager::SusiConfig::reload(&global_dir) {
                        Ok(reloaded) => {
                            println!(
                                "Dynamic configuration reloaded successfully from {}.",
                                global_dir.join("config.json").display()
                            );
                            println!("- Engine: {}", reloaded.default_engine());
                            println!("- Model: {}", reloaded.default_model());
                            println!("- Model Ladder Steps: {}", reloaded.model_ladder().len());
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
                match susi_engine::gemi::models::ModelManager::verify_and_provision_32b_and_72b_models(
                    &cwd,
                ) {
                    Ok(report) => {
                        println!(
                            "=== SUSI Model Download Agent & Network Verification Report ==="
                        );
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
                let res = susi_engine::gemi::models::ModelManager::install_model(&url);
                println!("{}", res);
            }
            Commands::Benchmark => {
                let report = susi_engine::gemi::benchmark::BenchmarkRunner::run_and_render(&cwd);
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
            Commands::DaemonStart { workspace } => {
                let ws = PathBuf::from(workspace);
                SusiDaemon::run_daemon_loop(ws, global_dir);
            }
        }
    } else if !cli.intent.is_empty() {
        let goal = cli.intent.join(" ");

        let ama = SusiMasterAgent::new();
        match susi_engine::daemon::admin::SusiAdmin::ingest_natural_intent(&cwd, &goal) {
            Ok(msg) => {
                info!("Natural intent ingested successfully: {}", msg);
                let _ = ama.solve_stream(&goal, &cwd, SUSI_VERSION, &glass_box_callback);
                let _ = io::stdout().flush();
            }
            Err(e) => {
                warn!(
                    "Natural intent ingestion failed: {}. Falling back to direct swarm solving.",
                    e
                );
                let _ = ama.solve_stream(&goal, &cwd, SUSI_VERSION, &glass_box_callback);
                let _ = io::stdout().flush();
            }
        }
    } else if !io::stdin().is_terminal() {
        match read_stdin_bounded() {
            Ok(Some(input)) => {
                let ama = SusiMasterAgent::new();
                let _ = ama.solve_stream(&input, &cwd, SUSI_VERSION, &glass_box_callback);
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
}

fn print_golden_rule_summary(workspace: &Path, global_dir: &Path) {
    use susi_engine::gemi::hardware::HardwareProfiler;
    use susi_engine::sandbox::manager::IntentBundleManager;

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
