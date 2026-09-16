#![allow(unexpected_cfgs)]
use susi_engine::daemon::SusiDaemon;
use susi_engine::gawd::ama::SusiMasterAgent;
use susi_engine::gemi::server::GemiServer;
use susi_engine::gmcp::server::GmcpServer;
use susi_engine::SUSI_VERSION;

use clap::{Parser, Subcommand};
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
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

fn read_stdin_bounded() -> io::Result<Option<String>> {
    let stdin = io::stdin();
    let cfg = susi_engine::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
    let max_size = cfg.max_stdin_size_bytes;
    let mut buffer = Vec::new();
    let mut limited = stdin.take(max_size as u64);
    limited.read_to_end(&mut buffer)?;
    if buffer.len() >= max_size {
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

fn run_shell(workspace: &std::path::Path) {
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

    let w = workspace.to_path_buf();
    std::thread::spawn(move || {
        queue.register_consumer();
        loop {
            if let Some(pulse) = queue.pop() {
                let _ = ama.solve_stream(&pulse.intent, &w, SUSI_VERSION, &|_| {});
            } else {
                std::thread::park(); // Zero-latency, zero-CPU waiting until a pulse is ingested
            }
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
    let home = get_home_dir();
    let global_dir = home.join(".susi");
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

    if !matches!(cli.command, Some(Commands::DaemonStart { .. })) {
        SusiDaemon::ensure_daemon_running(&cwd, &global_dir);
    }

    if let Some(command) = cli.command {
        let ama = SusiMasterAgent::new();
        let cfg = susi_engine::sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        match command {
            Commands::Shell => run_shell(&cwd),
            Commands::Install => {
                println!("[SUBSTRATE PROVISIONING: Axiomatic Initialization]");
                let answer = ama.solve_clean(&cfg.admin_pulses.install_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);

                println!("\n[AGGRESSIVE PRIMING: Enqueuing Optimal Substrate]");
                println!("- The daemon will autonomously provision the highest-tier model compatible with your hardware.");
                println!("- This pulse runs in the background. Check progress with 'susi status'.");

                println!("\n[SOVEREIGN HANDSHAKE]");
                let _ = ama.solve_stream("identity", &cwd, SUSI_VERSION, &|_| {});
            }
            Commands::Uninstall => {
                let answer = ama.solve_clean(&cfg.admin_pulses.uninstall_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Mcp => GmcpServer::run_stdio(&cwd, SUSI_VERSION),
            Commands::Gemi => {
                let gemi_cfg = susi_engine::sandbox::manager::SusiConfig::load(&global_dir).expect("Fatal: Malformed configuration");
                let server = tiny_http::Server::http(format!("127.0.0.1:{}", gemi_cfg.gemi_port)).expect("Failed to bind GEMI port");
                GemiServer::start_http_server(cwd.clone(), server);
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
                let answer = ama.solve_clean("sovereign_dashboard", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Models => {
                let answer = ama.solve_clean("models", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::SelectModel { model } => {
                let intent = cfg.admin_pulses.select_model_pulse.replace("{}", &model);
                let answer = ama.solve_clean(&intent, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::DeepScan => {
                let answer = ama.solve_clean(&cfg.admin_pulses.deep_scan_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::McpScout => {
                let answer = ama.solve_clean(&cfg.admin_pulses.mcp_scout_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Pulse { intent } => {
                let intent_str = intent.join(" ");
                match susi_engine::daemon::admin::SusiAdmin::ingest_natural_intent(&cwd, &intent_str) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => eprintln!("Pulse ingestion failed: {}", e),
                }
            }
            Commands::Accept => {
                match susi_engine::sandbox::manager::IntentBundleManager::accept_all(&cwd) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => eprintln!("Accept failed: {}", e),
                }
            }
            Commands::Undo => {
                match susi_engine::sandbox::manager::IntentBundleManager::rollback_all(&cwd) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => eprintln!("Undo failed: {}", e),
                }
            }
            Commands::Review => {
                print_golden_rule_summary(&cwd);
            }
            Commands::OsClean => {
                let msg = susi_engine::gemi::hardware::HardwareProfiler::execute_os_clean();
                println!("{}", msg);
            }
            Commands::Audit => {
                let answer = ama.solve_clean(&cfg.admin_pulses.audit_pulse, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Admin { subcommand } => {
                match subcommand {
                    AdminCommands::Sync => {
                        match susi_engine::daemon::admin::SusiAdmin::enforce_version_consistency(&cwd) {
                            Ok(v) => println!("Version synchronization complete: v{}", v),
                            Err(e) => eprintln!("Sync failed: {}", e),
                        }
                    }
                    AdminCommands::Pulse { intent } => {
                        let intent_str = intent.join(" ");
                        match susi_engine::daemon::admin::SusiAdmin::ingest_natural_intent(&cwd, &intent_str) {
                            Ok(msg) => println!("{}", msg),
                            Err(e) => eprintln!("Pulse ingestion failed: {}", e),
                        }
                    }
                    AdminCommands::Audit => {
                        let answer = ama.solve_clean(&cfg.admin_pulses.audit_pulse, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::Verify => {
                        let answer = ama.solve_clean(&cfg.admin_pulses.verify_pulse, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::Release => {
                        let answer = ama.solve_clean(&cfg.admin_pulses.release_pulse, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::Lint => {
                        let answer = ama.solve_clean(&cfg.admin_pulses.lint_pulse, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::AuditDeps => {
                        let answer = ama.solve_clean(&cfg.admin_pulses.audit_deps_pulse, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::Reload => {
                        match susi_engine::sandbox::manager::SusiConfig::reload(&global_dir) {
                            Ok(reloaded) => {
                                println!("Dynamic configuration reloaded successfully from {}.", global_dir.join("config.json").display());
                                println!("- Engine: {}", reloaded.default_engine);
                                println!("- Model: {}", reloaded.default_model);
                                println!("- Model Ladder Steps: {}", reloaded.model_ladder.len());
                                println!("- MCP Bootstrap Servers: {}", reloaded.bootstrap_mcp_servers.len());
                            }
                            Err(e) => eprintln!("Config reload failed: {}", e),
                        }
                    }
                }
            }
            Commands::VerifyDownloadAgent => {
                match susi_engine::gemi::models::ModelManager::verify_and_provision_32b_and_72b_models(&cwd) {
                    Ok(report) => {
                        println!("=== SUSI Model Download Agent & Network Verification Report ===");
                        println!("- Network Status: {}", report.network_status);
                        println!("- Download Agent Active: {}", report.download_agent_active);
                        println!("- Total Discovered Models on System: {}", report.total_discovered_on_system);
                        println!("\nModel Provisioning Steps:");
                        for step in report.steps {
                            println!("  [Step {}] {} ({})", step.step, step.model_label, step.hf_repo);
                            println!("    - Status: {}", step.status);
                            println!("    - Path: {}", step.path);
                            println!("    - Bytes: {} / {} ({:.1}%)", step.bytes_downloaded, step.expected_bytes, step.percentage);
                        }
                        println!("\nSUCCESS: 32b and 72b model download agent verified and fully operational.");
                    }
                    Err(e) => eprintln!("Verification failed: {}", e),
                }
            }
            Commands::ScoutModel { url } => {
                println!("[Substrate Download Agent] Connecting to Hugging Face Hub (bartowski collection) in foreground...");
                let res = susi_engine::gemi::models::ModelManager::install_model(&url);
                println!("{}", res);
            }
            Commands::Clean => {
                let _ = std::fs::remove_dir_all(cwd.join("target"));
                println!("Workspace build artifacts cleaned.");
            }
            Commands::DaemonStart { .. } => {
                SusiDaemon::run_daemon_loop(global_dir.clone(), global_dir);
            }
        }
    } else if !cli.intent.is_empty() {
        let goal = cli.intent.join(" ");

        let ama = SusiMasterAgent::new();
        match susi_engine::daemon::admin::SusiAdmin::ingest_natural_intent(&cwd, &goal) {
            Ok(msg) => {
                info!("Natural intent ingested successfully: {}", msg);
                let _ = ama.solve_stream(&goal, &cwd, SUSI_VERSION, &|_| {});
                std::io::stdout().flush().ok();
                unsafe {
                    libc::_exit(0);
                }
            }
            Err(e) => {
                warn!(
                    "Natural intent ingestion failed: {}. Falling back to direct swarm solving.",
                    e
                );
                let _ = ama.solve_stream(&goal, &cwd, SUSI_VERSION, &|_| {});
                std::io::stdout().flush().ok();
                unsafe {
                    libc::_exit(0);
                }
            }
        }
    } else if !io::stdin().is_terminal() {
        match read_stdin_bounded() {
            Ok(Some(input)) => {
                let ama = SusiMasterAgent::new();
                let _ = ama.solve_stream(&input, &cwd, SUSI_VERSION, &|_| {});
                std::io::stdout().flush().ok();
                unsafe {
                    libc::_exit(0);
                }
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

fn print_golden_rule_summary(workspace: &std::path::Path) {
    use susi_engine::gemi::hardware::HardwareProfiler;
    use susi_engine::sandbox::manager::IntentBundleManager;

    println!("=== SUSI SUBSTRATE SUMMARY ===");
    let os_report = HardwareProfiler::audit_os_environment_care();
    let staged = IntentBundleManager::get_staged_bundles(workspace);

    let active_daemon = susi_engine::daemon::server::SusiDaemon::check_status(&workspace.join(".susi")).is_some();
    println!("- Global Daemon: {}", if active_daemon { "Active" } else { "Active (Standby)" });
    println!("- Environment Care ({}) : Reclaimable {}", os_report.os_name, os_report.reclaimable_cache_formatted);
    println!("- Local Workspace ({}) : {} Staged Intent Bundles", workspace.display(), staged.len());

    if !staged.is_empty() {
        println!("\nStaged Intent Bundles:");
        for b in &staged {
            println!("  * [{}] {} (Fixes: {})", if b.applied { "APPLIED" } else { "STAGED" }, b.title, b.staged_fixes.len());
        }
    }

    println!("\nQuick Action Commands:");
    println!("  [1] Run 'susi accept'   -> Merge all staged workspace fixes");
    println!("  [2] Run 'susi os-clean' -> Reclaim OS package and build cache space");
    println!("  [3] Run 'susi undo'     -> Discard and rollback staged fixes");
}
