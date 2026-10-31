//! Pre-boot control-plane dispatch: commands that manage config, packs, peers,
//! and model surfaces run *before* substrate boot and return their exit code
//! early. Also the daemon start/stop primitives used by `start`/`stop`/
//! `restart` after boot.

use super::agent_cli;
use super::ambient_cli;
use super::auto_cli;
use super::blackboard_cli;
use super::brain_cli;
use super::broker_cli;
use super::commits_cli;
use super::context_graph_cli;
use super::crown_cli;
use super::defs::Commands;
use super::ecosystem_cli;
use super::extensions_cli;
use super::framework_cli;
use super::frontier_cli;
use super::harness_cli;
use super::intent_cli;
use super::mcp_cli;
use super::model_cli;
use super::open_weight_cli;
use super::openrouter_cli;
use super::os_cli;
use super::patch_cli;
use super::peers_cli;
use super::plan_cli;
use super::plane_cli::{apply_plane_prep, plane_exit, run_plane, run_plane_cwd, PlanePrep};
use super::privacy_cli;
use super::run_cli;
use super::services_cli;
use super::setup_cli;
use super::substrate_cli;
use super::tasks_cli;
use super::telemetry_cli;
use super::tx_cli;
use super::workflow_cli;

use std::env;
use std::path::Path;
use susi_daemon::discovery_pipeline::Catalogs;
use susi_daemon::SusiDaemon;

/// Dispatch control-plane commands that must run before substrate boot.
/// Returns `Ok(exit_code)` when the command was handled; `Err(command)` hands
/// the unconsumed command back so the caller can continue to substrate boot +
/// mission dispatch.
#[allow(clippy::result_large_err)] // Err is the whole unconsumed `Commands`, handed back by design
pub(crate) fn dispatch(
    command: Option<Commands>,
) -> Result<std::process::ExitCode, Option<Commands>> {
    // The catalogs this command reads, taken before its arm consumes it:
    // an Ecosystem plane primes exactly these and nothing else.
    let ecosystem = PlanePrep::Ecosystem(
        command
            .as_ref()
            .map_or(Catalogs::ALL, super::defs::command_catalogs),
    );
    // Leaf-service mode: the daemon re-execs this binary as
    // `service-run <name>`; it must serve before anything else touches
    // the substrate (no daemon ensure, no mission pipeline).
    if let Some(Commands::ServiceRun { name, port }) = command {
        return Ok(run_plane(PlanePrep::None, || service_run(&name, port)));
    }
    if let Some(Commands::Run(args)) = command {
        return Ok(run_plane_cwd(PlanePrep::Substrate, |cwd| {
            run_cli::execute(args, cwd)
        }));
    }
    if let Some(Commands::Extensions { action }) = command {
        return Ok(run_plane(PlanePrep::None, || {
            extensions_cli::execute(action)
        }));
    }
    if let Some(Commands::Auto { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Substrate, |cwd| {
            auto_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Setup { action }) = command {
        return Ok(run_plane(PlanePrep::None, || setup_cli::execute(action)));
    }
    if let Some(Commands::Workflow { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            workflow_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Harness { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            harness_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Tasks { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            tasks_cli::execute(action.map(|a| *a), cwd)
        }));
    }
    if let Some(Commands::Brain { action }) = command {
        return Ok(run_plane(PlanePrep::None, || brain_cli::execute(action)));
    }
    if let Some(Commands::Ecosystem { action }) = command {
        return Ok(run_plane(PlanePrep::None, || {
            ecosystem_cli::execute(action)
        }));
    }
    if let Some(Commands::Blackboard { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            blackboard_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::ContextGraph { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            context_graph_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Broker { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            broker_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Telemetry { action }) = command {
        return Ok(run_plane(PlanePrep::None, || {
            telemetry_cli::execute(action)
        }));
    }
    if let Some(Commands::Patch { args }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            patch_cli::execute(args, cwd)
        }));
    }
    if let Some(Commands::Plan { args }) = command {
        return Ok(run_plane_cwd(PlanePrep::Substrate, |cwd| {
            plan_cli::execute(args, cwd)
        }));
    }
    if let Some(Commands::Privacy { action }) = command {
        return Ok(run_plane(PlanePrep::None, || privacy_cli::execute(action)));
    }
    if let Some(Commands::Intent { action }) = command {
        return Ok(run_plane(PlanePrep::None, || intent_cli::execute(action)));
    }
    if let Some(Commands::Ambient { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            ambient_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Tx { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            tx_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Substrate { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            substrate_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Crown { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            crown_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Services { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            services_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Commits { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            commits_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Os { json, action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            os_cli::execute(action, json, cwd)
        }));
    }
    if let Some(Commands::Peers { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::None, |cwd| {
            peers_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Agents { action }) = command {
        return Ok(run_plane_cwd(ecosystem, |cwd| {
            agent_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Frameworks { action }) = command {
        return Ok(run_plane_cwd(ecosystem, |cwd| {
            framework_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Models { action }) = command {
        return Ok(run_plane_cwd(ecosystem, |cwd| {
            model_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::OpenWeight { action }) = command {
        return Ok(run_plane(ecosystem, || open_weight_cli::execute(action)));
    }
    if let Some(Commands::Frontier { action }) = command {
        return Ok(run_plane(ecosystem, || frontier_cli::execute(action)));
    }
    if let Some(Commands::OpenRouter { action }) = command {
        return Ok(run_plane(PlanePrep::None, || {
            openrouter_cli::execute(action)
        }));
    }
    Err(command)
}

/// Handle the `mcp` command pre-boot branch. Returns `Ok(exit_code)` when
/// handled; `Err(command)` hands it back for the substrate-boot path
/// (`mcp serve` / bare `mcp` serve SUSI's stdio endpoint post-boot).
#[allow(clippy::result_large_err)] // Err is the whole unconsumed `Commands`, handed back by design
pub(crate) fn dispatch_mcp(
    command: Option<Commands>,
) -> Result<std::process::ExitCode, Option<Commands>> {
    match command {
        Some(Commands::Mcp {
            action: Some(mcp_cli::McpCommands::Serve),
        })
        | Some(Commands::Mcp { action: None }) => Err(command),
        Some(Commands::Mcp { action }) => {
            let catalogs = super::defs::mcp_catalogs(action.as_ref());
            apply_plane_prep(PlanePrep::Ecosystem(catalogs));
            Ok(plane_exit(
                env::current_dir()
                    .map_err(anyhow::Error::from)
                    .and_then(|cwd| mcp_cli::execute(action, &cwd).map(|_| ())),
            ))
        }
        other => Err(other),
    }
}

/// How long `susi start` waits for a spawned daemon to expose the full host
/// contract. A first boot after a binary change can take ~90 s to bind
/// 9090/9091/9093/9094 (here 919x for the dev instance), so the wait is
/// deliberately generous: it returns as soon as the contract answers, and
/// only a deadline expiry is reported as a real failure.
const START_READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// Readiness poll interval; `host_contract_ready` already bounds its probes.
const START_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

pub(crate) fn control_plane_stop(global_dir: &Path) {
    // Instance-scoped stop: under SUSI_HOME only this instance's unit,
    // lock, orphan daemons, and contract-port holders are signalled —
    // siblings keep running. The daemon-side wait escalates to a forced
    // process-tree kill when a survivor outlives the grace period, and
    // reports whether the ports actually released.
    let outcome = SusiDaemon::stop_instance_daemons(global_dir);
    if outcome.released {
        if outcome.signalled > 0 {
            println!(
                "[SUSI Daemon] Stopped {} process(es). Host-contract ports released.",
                outcome.signalled
            );
        } else {
            println!("[SUSI Daemon] Already stopped.");
        }
    } else {
        eprintln!(
            "[SUSI Daemon] Stop failed: host-contract ports are still listening after {}s \
             ({} process(es) signalled). A process that is not part of this instance may \
             hold them; check `netstat -ano` for the port owner.",
            SusiDaemon::STOP_DEADLINE.as_secs(),
            outcome.signalled
        );
        std::process::exit(1);
    }
}

pub(crate) fn control_plane_start(cwd: &Path, global_dir: &Path) {
    let deadline = std::time::Instant::now() + START_READY_TIMEOUT;

    // Fast path: a live daemon whose full contract (TCP + UDP discovery)
    // already answers. Nothing waits.
    let fast_pid =
        SusiDaemon::check_status(cwd, global_dir).filter(|_| SusiDaemon::host_contract_ready());
    if let Some(pid) = fast_pid {
        println!("[SUSI Daemon] Running (PID: {}).", pid);
        println!("{}", SusiDaemon::host_contract_endpoints_report());
        report_auth_token();
        return;
    }

    // Slow path: the spawn from `ensure_daemon_running` may not have written
    // its lock yet, and after a binary change the daemon can spend ~90 s on
    // boot work before it binds the host contract. Poll both the pid and the
    // ports until the deadline; the pid alone is not readiness, and an early
    // poll is not failure.
    let mut last_pid = SusiDaemon::check_status(cwd, global_dir);
    loop {
        if let Some(pid) = SusiDaemon::check_status(cwd, global_dir) {
            last_pid = Some(pid);
        }
        if SusiDaemon::host_contract_ready() {
            match last_pid {
                Some(pid) => println!("[SUSI Daemon] Running (PID: {}).", pid),
                None => println!(
                    "[SUSI Daemon] Running (no lock file, but the host contract is listening)."
                ),
            }
            println!("{}", SusiDaemon::host_contract_endpoints_report());
            report_auth_token();
            return;
        }
        if std::time::Instant::now() >= deadline {
            // A live daemon that has not bound yet is still booting, not a
            // failed start — say which, so nobody kills a daemon mid-boot.
            let booting = last_pid.filter(|pid| SusiDaemon::process_alive(*pid));
            if let Some(pid) = booting.filter(|_| !SusiDaemon::host_contract_tcp_ready()) {
                eprintln!(
                    "[SUSI Daemon] Not ready: daemon PID {pid} is alive but the host-contract \
                     ports are not listening after {}s. It is still booting — the boot \
                     phases are logged to the audit log (set SUSI_BOOT_TRACE=1 on the \
                     daemon for a per-phase trace).",
                    START_READY_TIMEOUT.as_secs()
                );
                std::process::exit(1);
            }
            if SusiDaemon::host_contract_tcp_ready() {
                eprintln!(
                    "[SUSI Daemon] Failed to start. Host-contract TCP ports are listening but \
                     UDP discovery did not answer within {}s.",
                    START_READY_TIMEOUT.as_secs()
                );
            } else {
                eprintln!(
                    "[SUSI Daemon] Failed to start. Host-contract ports are not listening after {}s.",
                    START_READY_TIMEOUT.as_secs()
                );
            }
            std::process::exit(1);
        }
        std::thread::sleep(START_POLL_INTERVAL);
    }
}

fn report_auth_token() {
    let path = susi_paths::SusiDirs::config_dir().join("api_token");
    if susi_paths::host_token().is_some() {
        println!(
            "Auth: Bearer token in {} (required for HTTP clients)",
            path.display()
        );
    } else {
        eprintln!(
            "[SUSI Daemon] Host API token is unavailable; authenticated endpoints fail closed."
        );
    }
}

/// Leaf-service dispatch for `susi service-run <name>` — the daemon's
/// spawn mode. Each arm is the same `serve` entry the standalone
/// `susi-<name>` binary calls, so the staged `susi` binary can fill any
/// leaf role without sibling binaries on disk.
fn service_run(name: &str, port: Option<u16>) -> anyhow::Result<()> {
    let Some(svc) = susi_core::service_table::LEAF_SERVICES
        .iter()
        .find(|s| s.name == name)
    else {
        return Err(anyhow::anyhow!(
            "unknown leaf service '{name}' — expected one of {}",
            susi_core::service_table::LEAF_SERVICES
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };
    let port = port.unwrap_or_else(|| svc.port());
    susi_leaf_services::serve(name, port).map_err(anyhow::Error::from)
}
