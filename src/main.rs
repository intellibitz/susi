#![allow(unexpected_cfgs)]
use susi_engine::daemon::SusiDaemon;
use susi_engine::gawd::ama::SusiMasterAgent;
use susi_engine::gemi::server::GemiServer;
use susi_engine::gmcp::server::GmcpServer;
use susi_engine::SUSI_VERSION;

use clap::{Parser, Subcommand};
use std::env;
use std::io::{self, Read, Write, IsTerminal};
use std::path::PathBuf;
use tracing::{info, warn, error};

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
    /// List available models
    Models,
    /// Select or override active model
    SelectModel { model: String },
    /// Parallel deep scan of substrate home for local models
    DeepScan,
    /// Autonomous web-scouting of open-source MCP servers
    McpScout,
    /// Ingest a natural language intent into sovereign memory (EVIDENCE.md)
    Pulse {
        #[arg(trailing_var_arg = true)]
        intent: Vec<String>
    },
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
    DaemonStart { workspace: String },
}

#[derive(Subcommand)]
enum AdminCommands {
    /// Synchronize version consistency
    Sync,
    /// Ingest pulse via admin
    Pulse {
        #[arg(trailing_var_arg = true)]
        intent: Vec<String>
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
    let timeout_secs = cfg.stdin_timeout_secs;

    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        let fd = stdin.as_raw_fd();
        let timeout = libc::timeval { tv_sec: timeout_secs as _, tv_usec: 0 };
        unsafe {
            libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVTIMEO,
                &timeout as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::timeval>() as u32);
        }
    }
    let mut buffer = Vec::new();
    let mut limited = stdin.take(max_size as u64);
    limited.read_to_end(&mut buffer)?;
    if buffer.len() >= max_size {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("Input exceeds {} bytes limit", max_size)));
    }
    let content = String::from_utf8(buffer).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let trimmed = content.trim();
    Ok(if trimmed.is_empty() { None } else { Some(trimmed.to_string()) })
}

fn get_home_dir() -> PathBuf {
    env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

fn run_shell(workspace: &std::path::Path) {
    use susi_engine::gawd::queue::SubstratePulseQueue;
    let queue = SubstratePulseQueue::global();
    let ama = SusiMasterAgent::new();

    println!("SUSI Pulse Shell v{} (Glass Box Telemetry Mode Active)", SUSI_VERSION);
    println!("Enter pulses to interact with the substrate. Pulses are queued and processed in order.");
    println!("Type 'exit' to quit.");

    let w = workspace.to_path_buf();
    std::thread::spawn(move || {
        loop {
            if let Some(pulse) = queue.pop() {
                let _ = ama.solve_stream(&pulse.intent, &w, SUSI_VERSION);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    });

    loop {
        print!("susi> ");
        let _ = io::stdout().flush();
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() {
            let trimmed = input.trim();
            if trimmed.is_empty() { continue; }
            if trimmed == "exit" || trimmed == "quit" { break; }
            let _ = queue.ingest(trimmed, workspace, SUSI_VERSION);
        } else {
            break;
        }
    }
}

fn main() {
    #[cfg(tokio_unstable)]
    console_subscriber::init();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("debug"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stdout)
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .init();
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = get_home_dir();
    let global_dir = home.join(".susi");

    // Boot dynamic kernel assembly (Dynamic Self-Assembly Axiom)
    let _ = susi_engine::gawd::kernel_loader::SubstrateKernelLoader::boot_kernel(&cwd);

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
                let answer = ama.solve_clean(&cfg.admin_templates.install_mission, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Uninstall => {
                let answer = ama.solve_clean(&cfg.admin_templates.uninstall_mission, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Mcp => GmcpServer::run_stdio(&cwd, SUSI_VERSION),
            Commands::Gemi => {
                let gemi_cfg = susi_engine::sandbox::manager::SusiConfig::load(&global_dir).expect("Fatal: Malformed configuration");
                let server = tiny_http::Server::http(format!("127.0.0.1:{}", gemi_cfg.gemi_port)).expect("Failed to bind GEMI port");
                GemiServer::start_http_server(cwd.clone(), server);
            }
            Commands::Status => {
                let answer = ama.solve_clean("status", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Models => {
                let answer = ama.solve_clean("models", &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::SelectModel { model } => {
                let intent = cfg.admin_templates.select_model_mission.replace("{}", &model);
                let answer = ama.solve_clean(&intent, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::DeepScan => {
                let answer = ama.solve_clean(&cfg.admin_templates.deep_scan_mission, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::McpScout => {
                let answer = ama.solve_clean(&cfg.admin_templates.mcp_scout_mission, &cwd, SUSI_VERSION);
                println!("{}", answer);
            }
            Commands::Pulse { intent } => {
                let intent_str = intent.join(" ");
                match susi_engine::daemon::admin::SusiAdmin::ingest_natural_intent(&cwd, &intent_str) {
                    Ok(msg) => println!("{}", msg),
                    Err(e) => eprintln!("Pulse ingestion failed: {}", e),
                }
            }
            Commands::Audit => {
                let answer = ama.solve_clean(&cfg.admin_templates.audit_mission, &cwd, SUSI_VERSION);
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
                        let answer = ama.solve_clean(&cfg.admin_templates.audit_mission, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::Verify => {
                        let answer = ama.solve_clean(&cfg.admin_templates.verify_mission, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::Release => {
                        let answer = ama.solve_clean(&cfg.admin_templates.release_mission, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::Lint => {
                        let answer = ama.solve_clean(&cfg.admin_templates.lint_mission, &cwd, SUSI_VERSION);
                        println!("{}", answer);
                    }
                    AdminCommands::AuditDeps => {
                        let answer = ama.solve_clean(&cfg.admin_templates.audit_deps_mission, &cwd, SUSI_VERSION);
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
                let _ = ama.solve_stream(&goal, &cwd, SUSI_VERSION);
                std::io::stdout().flush().ok();
                unsafe { libc::_exit(0); }
            }
            Err(e) => {
                warn!("Natural intent ingestion failed: {}. Falling back to direct swarm solving.", e);
                let _ = ama.solve_stream(&goal, &cwd, SUSI_VERSION);
                std::io::stdout().flush().ok();
                unsafe { libc::_exit(0); }
            }
        }
    } else if !io::stdin().is_terminal() {
        match read_stdin_bounded() {
            Ok(Some(input)) => {
                let ama = SusiMasterAgent::new();
                let _ = ama.solve_stream(&input, &cwd, SUSI_VERSION);
                std::io::stdout().flush().ok();
                unsafe { libc::_exit(0); }
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
