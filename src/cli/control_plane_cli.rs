//! Pre-boot control-plane dispatch: commands that manage config, packs, peers,
//! and model surfaces run *before* substrate boot and return their exit code
//! early. Also the daemon start/stop primitives used by `start`/`stop`/
//! `restart` after boot.

use super::agent_cli;
use super::aider_cli;
use super::ambient_cli;
use super::auto_cli;
use super::blackboard_cli;
use super::broker_cli;
use super::browser_use_cli;
use super::commits_cli;
use super::context_graph_cli;
use super::crown_cli;
use super::deerflow_cli;
use super::defs::Commands;
use super::extensions_cli;
use super::framework_cli;
use super::frontier_cli;
use super::gemini_cli;
use super::intent_cli;
use super::mcp_cli;
use super::model_cli;
use super::open_weight_cli;
use super::openclaw_cli;
use super::openhands_cli;
use super::openrouter_cli;
use super::openviking_cli;
use super::patch_cli;
use super::plan_cli;
use super::plane_cli::{apply_plane_prep, plane_exit, run_plane, run_plane_cwd, PlanePrep};
use super::privacy_cli;
use super::python_engine_cli;
use super::services_cli;
use super::substrate_cli;
use super::swe_agent_cli;
use super::telemetry_cli;
use super::tx_cli;

use std::env;
use std::path::Path;
use susi_daemon::SusiDaemon;

/// Dispatch control-plane commands that must run before substrate boot.
/// Returns `Ok(exit_code)` when the command was handled; `Err(command)` hands
/// the unconsumed command back so the caller can continue to substrate boot +
/// mission dispatch.
pub(crate) fn dispatch(
    command: Option<Commands>,
) -> Result<std::process::ExitCode, Option<Commands>> {
    if let Some(Commands::Extensions { action }) = command {
        return Ok(run_plane(PlanePrep::None, || {
            extensions_cli::execute(action)
        }));
    }
    if let Some(Commands::Auto { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::CloudSubstrate, |cwd| {
            auto_cli::execute(action, cwd)
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
        return Ok(run_plane_cwd(PlanePrep::CloudSubstrate, |cwd| {
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
    if let Some(Commands::Agents { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::CloudEcosystem, |cwd| {
            agent_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Frameworks { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::CloudEcosystem, |cwd| {
            framework_cli::execute(action, cwd)
        }));
    }
    match &command {
        Some(Commands::Models {
            action: Some(model_cli::ModelCommands::Local),
        }) => {
            // Fall through to substrate boot + mission path in main.
        }
        Some(Commands::Models { .. }) => {
            if let Some(Commands::Models { action }) = command {
                return Ok(run_plane_cwd(PlanePrep::CloudEcosystem, |cwd| {
                    model_cli::execute(action, cwd)
                }));
            }
        }
        _ => {}
    }
    if let Some(Commands::OpenWeight { action }) = command {
        return Ok(run_plane(PlanePrep::CloudEcosystem, || {
            open_weight_cli::execute(action)
        }));
    }
    if let Some(Commands::Frontier { action }) = command {
        return Ok(run_plane(PlanePrep::CloudEcosystem, || {
            frontier_cli::execute(action)
        }));
    }
    if let Some(Commands::OpenRouter { action }) = command {
        return Ok(run_plane(PlanePrep::Cloud, || {
            openrouter_cli::execute(action)
        }));
    }
    if let Some(Commands::OpenHands { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            openhands_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Gemini { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            gemini_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::Aider { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            aider_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::SweAgent { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            swe_agent_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::OpenClaw { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            openclaw_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::BrowserUse { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            browser_use_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::OpenViking { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            openviking_cli::execute(action, cwd)
        }));
    }
    if let Some(Commands::DeerFlow { action }) = command {
        return Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
            deerflow_cli::execute(action, cwd)
        }));
    }
    let (profile, action) = match command {
        Some(Commands::LangGraph { action }) => (&susi_agents::external::LANGGRAPH_PROFILE, action),
        Some(Commands::OpenAiAgents { action }) => {
            (&susi_agents::external::OPENAI_AGENTS_PROFILE, action)
        }
        Some(Commands::AutoGen { action }) => (&susi_agents::external::AUTOGEN_PROFILE, action),
        Some(Commands::SmolAgents { action }) => {
            (&susi_agents::external::SMOLAGENTS_PROFILE, action)
        }
        Some(Commands::CrewAi { action }) => (&susi_agents::external::CREWAI_PROFILE, action),
        Some(Commands::LlamaIndex { action }) => {
            (&susi_agents::external::LLAMAINDEX_PROFILE, action)
        }
        Some(Commands::Temporal { action }) => (&susi_agents::external::TEMPORAL_PROFILE, action),
        Some(Commands::E2b { action }) => (&susi_agents::external::E2B_PROFILE, action),
        Some(Commands::Haystack { action }) => (&susi_agents::external::HAYSTACK_PROFILE, action),
        Some(Commands::N8n { action }) => (&susi_agents::external::N8N_PROFILE, action),
        other => return Err(other),
    };
    Ok(run_plane_cwd(PlanePrep::Cloud, |cwd| {
        python_engine_cli::execute(profile, action, cwd)
    }))
}

/// Handle the `mcp` command pre-boot branch. Returns `Ok(exit_code)` when
/// handled; `Err(command)` hands it back for the substrate-boot path
/// (`mcp serve` / bare `mcp` serve SUSI's stdio endpoint post-boot).
pub(crate) fn dispatch_mcp(
    command: Option<Commands>,
) -> Result<std::process::ExitCode, Option<Commands>> {
    match command {
        Some(Commands::Mcp {
            action: Some(mcp_cli::McpCommands::Serve),
        })
        | Some(Commands::Mcp { action: None }) => Err(command),
        Some(Commands::Mcp { action }) => {
            apply_plane_prep(PlanePrep::CloudEcosystem);
            Ok(plane_exit(
                env::current_dir()
                    .map_err(anyhow::Error::from)
                    .and_then(|cwd| mcp_cli::execute(action, &cwd).map(|_| ())),
            ))
        }
        other => Err(other),
    }
}

pub(crate) fn control_plane_stop(global_dir: &Path) {
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

pub(crate) fn control_plane_start(cwd: &Path, global_dir: &Path) {
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
