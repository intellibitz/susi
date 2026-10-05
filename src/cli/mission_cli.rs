//! Post-boot mission dispatch: every `Commands` variant that reaches this
//! point either drives the swarm (AMA solve) or runs a substrate-local op
//! that still needs the daemon/tracing/boot sequence from `main`.

use super::admin_cli;
use super::commits_cli;
use super::control_plane_cli::{control_plane_start, control_plane_stop};
use super::defs::{AuditCommands, Commands};
use super::keys_cli;
use super::mcp_cli;
use super::os_cli;
use super::peers_cli;
use super::services_cli;
use super::shell_cli::{glass_box_callback, run_shell, MissionHost};
use super::status_cli;

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
            // The initializer itself (the swarm used to be asked to do it
            // and report back).
            match susi_sandbox::manager::SandboxManager::ensure_global_sandbox(global_dir) {
                Ok(()) => println!("- Substrate home ready: {}", global_dir.display()),
                Err(e) => {
                    eprintln!("Substrate initialization failed: {}", e);
                    std::process::exit(1);
                }
            }

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
            if let Err(error) = susi_sandbox::manager::SusiConfig::ensure_api_auth_token_seeded() {
                eprintln!("[GEMI] Failed to secure host API token: {error}");
                return std::process::ExitCode::FAILURE;
            }
            // Degrade to bundled defaults rather than panic if
            // config.json is torn by a concurrent writer.
            let gemi_cfg = susi_sandbox::manager::SusiConfig::load(global_dir).unwrap_or_default();
            let bind_address: String = gemi_cfg
                .get("bind_address")
                .unwrap_or_else(|| "127.0.0.1".to_string());
            // A specific non-loopback bind would strand internal callers
            // dialing 127.0.0.1 — keep a loopback socket alongside it.
            // Wildcard binds already cover loopback.
            let mut listeners = Vec::new();
            let wildcard = matches!(bind_address.as_str(), "0.0.0.0" | "::");
            if !wildcard && !susi_daemon::tls::is_loopback(&bind_address) {
                match std::net::TcpListener::bind(format!("127.0.0.1:{}", gemi_cfg.gemi_port())) {
                    Ok(l) => listeners.push(l),
                    Err(e) => eprintln!(
                        "[GEMI] Loopback bind on {} failed (external clients still served): {e}",
                        gemi_cfg.gemi_port()
                    ),
                }
            }
            match std::net::TcpListener::bind(format!("{}:{}", bind_address, gemi_cfg.gemi_port()))
            {
                Ok(l) => listeners.push(l),
                Err(e) => {
                    eprintln!("[GEMI] Failed to bind port {}: {e}", gemi_cfg.gemi_port());
                    return std::process::ExitCode::FAILURE;
                }
            };
            let tls = susi_daemon::tls::endpoint_acceptor(&bind_address, global_dir);
            let require_tls_remote = susi_daemon::tls::https_only();
            GemiServer::start_http_server((*cwd).to_path_buf(), listeners, tls, require_tls_remote);
        }
        Commands::Status => {
            let id_file = global_dir.join("identity.key");
            if id_file.exists() {
                if let Ok(id) = std::fs::read_to_string(id_file) {
                    println!("[SUBSTRATE IDENTITY]: {}", id.trim());
                }
            }
            status_cli::run(cwd, global_dir);
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
        // The AST auditor itself, not a swarm mission asked to "bloat_audit"
        // (which narrated a report instead of printing the auditor's).
        Commands::BloatAudit => match susi_gawd::bloat_audit::BloatAuditor::audit_workspace(cwd) {
            Ok(report) => println!(
                "{}",
                susi_gawd::bloat_audit::BloatAuditor::render_report(&report)
            ),
            Err(e) => {
                eprintln!("Bloat audit failed: {}", e);
                std::process::exit(1);
            }
        },
        // Writes the override directly (the swarm used to be asked to do it).
        Commands::SelectModel { model } => {
            match susi_gemi::models::ModelManager::set_selected_model(&model) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("select-model failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        // The scanner itself, not a swarm mission asked to scan.
        Commands::DeepScan => {
            match susi_gemi::models::ModelManager::deep_scan_home_and_register(global_dir) {
                Ok(text) => println!("{}", text),
                Err(e) => {
                    eprintln!("Deep scan failed: {}", e);
                    std::process::exit(1);
                }
            }
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
                susi_gemi::engines::mcp_provider::register_mcp_inference_providers(
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
        Commands::Review => {
            print_golden_rule_summary(cwd, global_dir);
        }
        // Alias of `susi os clean` — one implementation. This command used
        // to run a partial copy that removed only the rotated metrics file.
        Commands::OsClean => {
            if let Err(e) = crate::cli::os_cli::clean() {
                eprintln!("os clean failed: {e}");
                std::process::exit(1);
            }
        }
        // Same deterministic audit as `susi admin audit`. This used to ask
        // the swarm to narrate a compliance audit, which a model answers
        // with unverified figures (the Mandate 2 failure recorded on
        // SovereignDashboard below).
        Commands::Audit { action } => match action {
            None => match susi_gawd::admin::SusiAdmin::audit_compliance(cwd, Some("push")) {
                Ok(report) => println!("{}", report),
                Err(e) => {
                    eprintln!("Compliance audit failed: {}", e);
                    std::process::exit(1);
                }
            },
            Some(AuditCommands::Verify { since }) => {
                let audit_file = cwd.join(".susi").join("audit.log");
                match susi_sandbox::audit_chain::verify_chain_since(&audit_file, since) {
                    Ok(count) => {
                        if let Some(since) = since {
                            println!(
                                "Audit chain verified: {count} record(s) at or after Unix timestamp {since}"
                            );
                        } else {
                            println!("Audit chain verified: {count} record(s)");
                        }
                    }
                    Err(error) => {
                        eprintln!("Audit chain verification failed: {error}");
                        std::process::exit(1);
                    }
                }
            }
        },
        Commands::Keys { action } => keys_cli::run(action),
        Commands::Admin { subcommand } => admin_cli::run(subcommand, host),
        Commands::VerifyDownloadAgent => {
            match susi_gemi::models::ModelManager::verify_and_provision_model_ladder(cwd) {
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
                            "\nSUCCESS: all {} download steps completed and verified.",
                            report.steps.len()
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
            match susi_gemi::models::ModelManager::install_model_foreground(&url) {
                Ok(result) => println!("{result}"),
                Err(error) => {
                    eprintln!("Model installation failed: {error}");
                    std::process::exit(1);
                }
            }
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
        Commands::Os { json, action } => {
            // Same pre-boot-dispatch note as Services above.
            if let Err(e) = os_cli::execute(action, json, cwd) {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        Commands::Peers { action } => {
            // Same pre-boot-dispatch note as Services above.
            if let Err(e) = peers_cli::execute(action, cwd) {
                eprintln!("{e}");
                return std::process::ExitCode::FAILURE;
            }
        }
        Commands::ServiceRun { .. } => {
            // Consumed by the pre-boot dispatch before substrate boot —
            // a leaf service process must never reach mission dispatch.
            eprintln!("service-run is dispatched before substrate boot");
            return std::process::ExitCode::FAILURE;
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
    use susi_gemi::models::hardware::HardwareProfiler;

    println!("=== SUSI SUBSTRATE SUMMARY ===");
    let os_report = HardwareProfiler::audit_os_environment_care();

    match SusiDaemon::check_status(workspace, global_dir) {
        Some(pid) => println!("- Global Daemon: Active (PID: {})", pid),
        None => println!("- Global Daemon: Inactive"),
    }
    println!(
        "- Environment Care ({}) : Reclaimable {}",
        os_report.os_name, os_report.reclaimable_cache_formatted
    );
    println!("\nQuick Action Commands:");
    println!("  susi os-clean  # Reclaim OS-level cache bloat");
    println!("  susi status    # System health pulse");
}
