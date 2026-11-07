//! Post-boot mission dispatch: every `Commands` variant that reaches this
//! point either drives the swarm (AMA solve) or runs a substrate-local op
//! that still needs the daemon/tracing/boot sequence from `main`.

use super::admin_cli;
use super::commits_cli;
use super::control_plane_cli::{control_plane_start, control_plane_stop};
use super::defs::{AuditCommands, Commands};
use super::keys_cli;
use super::mcp_cli;
use super::mission_host::MissionHost;
use super::os_cli;
use super::peers_cli;
use super::services_cli;
use super::status_cli;

// env/PathBuf/Command serve the uninstall path's service deregistration blocks.
#[cfg(unix)]
use std::env;
use std::fs;
use std::io::{self};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(any(unix, target_os = "windows"))]
use std::process::Command;

use susi::SUSI_VERSION;
use susi_daemon::SusiDaemon;
use susi_gmcp::server::GmcpServer;
use susi_server::GemiServer;

/// `susi audit query`: redacted action history from the workspace audit
/// chain, as text lines or one JSON array.
pub(crate) fn audit_query(
    cwd: &std::path::Path,
    query: &susi_sandbox_client::audit_chain::AuditQuery,
    json: bool,
) -> std::process::ExitCode {
    let audit_file = cwd.join(".susi").join("audit.log");
    match susi_sandbox_client::audit_chain::query_chain(&audit_file, query) {
        Ok(records) if json => match serde_json::to_string_pretty(&records) {
            Ok(output) => println!("{output}"),
            Err(error) => {
                eprintln!("Audit query serialization failed: {error}");
                return std::process::ExitCode::FAILURE;
            }
        },
        Ok(records) => {
            for record in records {
                println!(
                    "{} {} actor={} mission={} kind={} outcome={} cost_micros={}",
                    record.ts,
                    record.event_type,
                    record.actor.as_deref().unwrap_or("-"),
                    record.mission.as_deref().unwrap_or("-"),
                    record.kind,
                    record.outcome.as_deref().unwrap_or("-"),
                    record
                        .cost_micros
                        .map_or_else(|| "-".to_string(), |cost| cost.to_string()),
                );
            }
        }
        Err(error) => {
            eprintln!("Audit query failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }
    std::process::ExitCode::SUCCESS
}

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
        | Commands::Harness { .. }
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
        | Commands::Run { .. }
        | Commands::Top { .. } => {}
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
        Commands::Install => return super::install_cli::execute(),
        command @ Commands::InstallRelease { .. } => {
            return super::install_cli::installed_channel::manage(&command);
        }
        Commands::Local { args } => return super::local_delivery_cli::execute(&args),
        Commands::Uninstall => {
            uninstall(global_dir);
        }
        Commands::Gemi => {
            // Seed bearer before binding so empty-token loopback is not an
            // unauthenticated open door on the GEMI CLI path.
            if let Err(error) =
                susi_sandbox_client::manager::SusiConfig::ensure_api_auth_token_seeded()
            {
                eprintln!("[GEMI] Failed to secure host API token: {error}");
                return std::process::ExitCode::FAILURE;
            }
            // Degrade to bundled defaults rather than panic if
            // config.json is torn by a concurrent writer.
            let gemi_cfg =
                susi_sandbox_client::manager::SusiConfig::load(global_dir).unwrap_or_default();
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
        Commands::Status { json } => {
            // `main::dispatch_local_report` answers this before substrate
            // boot; the arm stays so the post-boot dispatch remains total.
            if json {
                return status_cli::run_json(cwd, global_dir);
            }
            status_cli::print_identity(global_dir);
            status_cli::run(cwd, global_dir);
        }
        Commands::SubstrateReport => {
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
        // `susi_daemon::adapter_ops::mcp_add` admits and hot-plugs the
        // server; the MCP `mcp_add` tool calls the same function.
        Commands::McpAdd {
            name,
            command_or_url,
            args,
        } => match susi_daemon::adapter_ops::mcp_add(&name, &command_or_url, &args) {
            Ok(res) => println!("{}", res),
            Err(res) => {
                println!("{}", res);
                std::process::exit(1);
            }
        },
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
        // SubstrateReport below).
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
                match susi_sandbox_client::audit_chain::verify_chain_since(&audit_file, since) {
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
            // `main::dispatch_local_report` answers this before substrate
            // boot; the arm stays so the post-boot dispatch remains total.
            Some(AuditCommands::Query {
                actor,
                mission,
                kind,
                since,
                until,
                json,
            }) => {
                let query = susi_sandbox_client::audit_chain::AuditQuery {
                    actor,
                    mission,
                    kind,
                    since,
                    until,
                };
                return audit_query(cwd, &query, json);
            }
        },
        Commands::Keys { action } => keys_cli::run(action),
        Commands::Admin { subcommand } => admin_cli::run(subcommand, host),
        // `susi_daemon::provider_ops` builds the ladder report; the MCP
        // `verify_download_agent` tool returns the same document.
        Commands::VerifyDownloadAgent => {
            match susi_daemon::provider_ops::verify_download_agent(cwd) {
                Ok(report) => {
                    let text = |v: &serde_json::Value| {
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string())
                    };
                    println!("=== SUSI Model Download Agent & Network Verification Report ===");
                    println!("- Network Status: {}", text(&report["network_status"]));
                    println!(
                        "- Download Agent Active: {}",
                        text(&report["download_agent_active"])
                    );
                    println!(
                        "- Total Discovered Models on System: {}",
                        text(&report["total_discovered_on_system"])
                    );
                    println!("\nModel Provisioning Steps:");
                    let steps = report["steps"].as_array().cloned().unwrap_or_default();
                    for step in &steps {
                        println!(
                            "  [Step {}] {} ({})",
                            text(&step["step"]),
                            text(&step["model_label"]),
                            text(&step["hf_repo"])
                        );
                        println!("    - Status: {}", text(&step["status"]));
                        println!("    - Path: {}", text(&step["path"]));
                        println!(
                            "    - Bytes: {} / {} ({:.1}%)",
                            text(&step["bytes_downloaded"]),
                            text(&step["expected_bytes"]),
                            step["percentage"].as_f64().unwrap_or(0.0)
                        );
                    }
                    if report["all_verified"].as_bool() == Some(true) {
                        println!(
                            "\nSUCCESS: all {} download steps completed and verified.",
                            steps.len()
                        );
                    } else {
                        println!(
                            "\nINCOMPLETE: one or more provisioning steps are not COMPLETED_VERIFIED."
                        );
                        // Verification only: Susi never downloads by itself.
                        println!(
                            "No download was started. To install the local starter model, {}.",
                            susi_gemi::models::NoLocalModelInstalled::starter().install_hint()
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
        Commands::Benchmark => {
            // Same call as the MCP `benchmark_run` tool.
            println!("{}", susi_daemon::provider_ops::benchmark(cwd));
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
    #[cfg(target_os = "windows")]
    {
        let has_task = Command::new("schtasks")
            .args(["/Query", "/TN", "susi-daemon"])
            .output()
            .is_ok_and(|out| out.status.success());
        if has_task
            && Command::new("schtasks")
                .args(["/Delete", "/TN", "susi-daemon", "/F"])
                .status()
                .is_ok_and(|s| s.success())
        {
            steps.push("removed Windows scheduled task (susi-daemon)".to_string());
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
    // `susi_daemon::host_views::review` builds the summary; the MCP
    // `operator_review` tool returns the same document.
    let doc = susi_daemon::host_views::review(workspace, global_dir);
    for line in doc["lines"].as_array().into_iter().flatten() {
        println!("{}", line.as_str().unwrap_or_default());
    }
}
