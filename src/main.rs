#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! SUSI Engine CLI
//!
//! Command-line interface for the susi local-first AI orchestration system.
//!
//! Module map: `cli::defs` holds the clap surface; `cli::control_plane_cli`
//! dispatches pre-boot commands; `cli::mission_cli` dispatches post-boot commands;
//! `cli::plane_cli`/`cli::shell_cli`/`cli::keys_cli`/`cli::admin_cli` hold shared plumbing; the
//! remaining `cli::*_cli` modules own per-domain subcommands.

#![allow(unexpected_cfgs)]
#![allow(missing_docs)]

mod cli;
mod cli_json;

use cli::control_plane_cli::{dispatch, dispatch_mcp};
use cli::defs::{command_requires_daemon, Cli};
use cli::mission_cli::dispatch as dispatch_mission;
use cli::shell_cli::{glass_box_callback, run_shell, MissionHost};

use susi::SUSI_VERSION;
use susi_daemon::SusiDaemon;
use susi_gawd::ama::SusiMasterAgent;

use clap::Parser;
use std::env;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;
use tracing::{error, info, warn};

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

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    // Control-plane planes return early; Models::Local and Mcp serve fall through.
    let command = match dispatch(cli.command) {
        Ok(code) => return code,
        Err(command) => command,
    };
    let command = match dispatch_mcp(command) {
        Ok(code) => return code,
        Err(command) => command,
    };

    // Composition root (CLI): hooks → packs → cloud.env → auto-prime.
    // See ARCHITECTURE.md and susi_daemon::composition.
    susi_sandbox::auto_install::push_to_hardware_if_dev_build();
    let substrate = susi_paths::SusiDirs::substrate_home();
    susi_daemon::composition::wire_cli_substrate(&substrate);
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

    let needs_daemon = match &command {
        Some(cmd) => command_requires_daemon(cmd),
        None => true, // bare intent, stdin pipe, or interactive shell
    };
    if needs_daemon {
        SusiDaemon::ensure_daemon_running(&cwd, &global_dir);
    }

    if let Some(command) = command {
        let ama = SusiMasterAgent::new();
        let cfg = susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let host = MissionHost {
            cwd: &cwd,
            global_dir: &global_dir,
            ama: &ama,
            cfg: &cfg,
        };
        exit_code = dispatch_mission(command, &host);
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
