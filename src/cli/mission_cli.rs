//! Post-boot mission dispatch: every `Commands` variant that reaches this
//! point either drives the swarm (AMA solve) or runs a substrate-local op
//! that still needs the daemon/tracing/boot sequence from `main`.

use super::admin_cli;
use super::commits_cli;
use super::control_plane_cli::{control_plane_start, control_plane_stop};
use super::defs::Commands;
use super::keys_cli;
use super::mcp_cli;
use super::services_cli;
use super::shell_cli::{glass_box_callback, run_shell, MissionHost};

use std::env;
use std::fs;
use std::io::{self};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use susi::SUSI_VERSION;
use susi_daemon::SusiDaemon;
use susi_gmcp::server::GmcpServer;
use susi_server::GemiServer;

/// Run one post-boot command. Returns the process exit code (most commands
/// exit SUCCESS; mission paths propagate the swarm report code).
pub(crate) fn dispatch(command: Commands, host: &MissionHost) -> std::process::ExitCode {
    let MissionHost {
        cwd,
        global_dir,
        ama,
        cfg,
    } = host;
    match command {
        // Handled by control_plane_cli before substrate boot.
        Commands::Agents { .. }
        | Commands::Frameworks { .. }
        | Commands::Extensions { .. }
        | Commands::Auto { .. }
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
        | Commands::OpenWeight { .. }
        | Commands::Frontier { .. }
        | Commands::OpenRouter { .. }
        | Commands::OpenHands { .. }
        | Commands::Gemini { .. }
        | Commands::Aider { .. }
        | Commands::SweAgent { .. }
        | Commands::OpenClaw { .. }
        | Commands::BrowserUse { .. }
        | Commands::OpenViking { .. }
        | Commands::DeerFlow { .. }
        | Commands::LangGraph { .. }
        | Commands::OpenAiAgents { .. }
        | Commands::AutoGen { .. }
        | Commands::SmolAgents { .. }
        | Commands::CrewAi { .. }
        | Commands::LlamaIndex { .. }
        | Commands::Temporal { .. }
        | Commands::E2b { .. }
        | Commands::Haystack { .. }
        | Commands::N8n { .. } => {}
        Commands::Models {
            action: Some(super::model_cli::ModelCommands::Local),
        } => {
            let answer = ama.solve_clean("models", cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        Commands::Models { .. } => {
            // Control-plane models commands handled before substrate boot.
        }
        Commands::Mcp {
            action: Some(mcp_cli::McpCommands::Serve) | None,
        } => GmcpServer::run_stdio(cwd, SUSI_VERSION),
        Commands::Mcp { .. } => {
            // Leading MCP manage commands handled before substrate boot.
        }
        Commands::Start => {
            control_plane_start(cwd, global_dir);
        }
        Commands::Stop => {
            control_plane_stop(global_dir);
        }
        Commands::Restart => {
            println!("[SUSI Daemon] Restarting host contract...");
            control_plane_stop(global_dir);
            // ensure_daemon_running was skipped for Restart; bring it back up.
            SusiDaemon::ensure_daemon_running(cwd, global_dir);
            control_plane_start(cwd, global_dir);
        }
        Commands::Shell => run_shell(cwd),
        Commands::Install => {
            println!("[SUBSTRATE PROVISIONING: Axiomatic Initialization]");
            let answer = ama.solve_clean(&cfg.admin_pulses().install_pulse, cwd, SUSI_VERSION);
            println!("{}", answer);

            println!("\n[AGGRESSIVE PRIMING: Enqueuing Optimal Substrate]");
            println!(
                "- The daemon will autonomously provision the highest-tier model compatible with your hardware."
            );
            println!("- This pulse runs in the background. Check progress with 'susi status'.");

            println!("\n[SOVEREIGN HANDSHAKE]");
            return ama
                .solve_stream_report("identity", cwd, SUSI_VERSION, &glass_box_callback)
                .exit_code();
        }
        Commands::Uninstall => {
            uninstall(global_dir);
        }
        Commands::Gemi => {
            // Seed bearer before binding so empty-token loopback is not an
            // unauthenticated open door on the GEMI CLI path.
            let _ = susi_sandbox::manager::SusiConfig::ensure_api_auth_token_seeded();
            // Degrade to bundled defaults rather than panic if
            // config.json is torn by a concurrent writer.
            let gemi_cfg = susi_sandbox::manager::SusiConfig::load(global_dir).unwrap_or_default();
            let bind_address: String = gemi_cfg
                .get("bind_address")
                .unwrap_or_else(|| "127.0.0.1".to_string());
            let listener = match std::net::TcpListener::bind(format!(
                "{}:{}",
                bind_address,
                gemi_cfg.gemi_port()
            )) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[GEMI] Failed to bind port {}: {e}", gemi_cfg.gemi_port());
                    return std::process::ExitCode::FAILURE;
                }
            };
            GemiServer::start_http_server((*cwd).to_path_buf(), listener);
        }
        Commands::Status => {
            let id_file = global_dir.join("identity.key");
            if id_file.exists() {
                if let Ok(id) = std::fs::read_to_string(id_file) {
                    println!("[SUBSTRATE IDENTITY]: {}", id.trim());
                }
            }
            let answer = ama.solve_clean("status", cwd, SUSI_VERSION);
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
            match ama.generate_substrate_report(cwd) {
                Ok(report) => println!("{}", report),
                Err(e) => eprintln!("[SUSI] Failed to generate substrate report: {}", e),
            }
        }
        Commands::BloatAudit => {
            let answer = ama.solve_clean("bloat_audit", cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        Commands::SelectModel { model } => {
            let intent = cfg.admin_pulses().select_model_pulse.replace("{}", &model);
            let answer = ama.solve_clean(&intent, cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        Commands::DeepScan => {
            let answer = ama.solve_clean(&cfg.admin_pulses().deep_scan_pulse, cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        Commands::McpScout => {
            let answer = ama.solve_clean(&cfg.admin_pulses().mcp_scout_pulse, cwd, SUSI_VERSION);
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
                // The vendored global is the same catalog as susi_core's —
                // both ride the shared bus rendezvous.
                susi_gmcp::mcp_wrapper::auto_discover_mcp();
                susi_gemi::mcp_provider::register_mcp_inference_providers(
                    susi_gemi::susi_core::registry::CapabilityRegistry::global(),
                );
            } else {
                std::process::exit(1);
            }
        }
        Commands::Pulse { intent } => {
            let intent_str = intent.join(" ");
            match susi_gawd::admin::SusiAdmin::ingest_natural_intent(cwd, &intent_str) {
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
            let answer = ama.solve_clean(&intent_str, cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        Commands::Accept => match susi_sandbox::manager::IntentBundleManager::accept_all(cwd) {
            Ok(msg) => println!("{}", msg),
            Err(e) => {
                eprintln!("Accept failed: {}", e);
                std::process::exit(1);
            }
        },
        Commands::Undo => match susi_sandbox::manager::IntentBundleManager::rollback_all(cwd) {
            Ok(msg) => println!("{}", msg),
            Err(e) => {
                eprintln!("Undo failed: {}", e);
                std::process::exit(1);
            }
        },
        Commands::Review => {
            print_golden_rule_summary(cwd, global_dir);
        }
        Commands::OsClean => {
            let msg = susi_gemi::hardware::HardwareProfiler::execute_os_clean();
            println!("{}", msg);
        }
        Commands::Audit => {
            let answer = ama.solve_clean(&cfg.admin_pulses().audit_pulse, cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        Commands::Keys { action } => keys_cli::run(action),
        Commands::Admin { subcommand } => admin_cli::run(subcommand, host),
        Commands::VerifyDownloadAgent => {
            match susi_gemi::models::ModelManager::verify_and_provision_32b_and_72b_models(cwd) {
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
            let report = susi_gemi::eval::EvalRunner::run_evaluations(cwd);
            println!("{}", report);
        }
        Commands::Benchmark => {
            let report = susi_gemi::benchmark::BenchmarkRunner::run_and_render(cwd);
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
            SusiDaemon::run_daemon_loop(
                susi_paths::SusiDirs::substrate_home(),
                global_dir.to_path_buf(),
            );
        }
        Commands::Services { action } => {
            // Pre-boot dispatch in control_plane_cli normally wins first;
            // this arm keeps the match exhaustive and covers any path that
            // reaches post-boot dispatch with the command intact.
            if let Err(e) = services_cli::execute(action, cwd) {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        Commands::Commits { action } => {
            // Same pre-boot-dispatch note as Services above.
            if let Err(e) = commits_cli::execute(action, cwd) {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    std::process::ExitCode::SUCCESS
}

/// Deterministic teardown — an agent pulse cannot guarantee the
/// binary/service/state are actually gone, and reinstall must work after
/// every uninstall.
fn uninstall(global_dir: &Path) {
    let bin_dir = global_dir.join("bin");
    let mut steps: Vec<String> = Vec::new();

    let daemons_stopped = SusiDaemon::stop_all_daemons(global_dir);
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
            Err(e) => steps.push(format!("could not remove {}: {}", bin_dir.display(), e)),
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
    println!("  susi accept    # Apply all staged intent fixes");
    println!("  susi undo      # Revert all staged intent fixes");
    println!("  susi os-clean  # Reclaim OS-level cache bloat");
    println!("  susi status    # System health pulse");
}
