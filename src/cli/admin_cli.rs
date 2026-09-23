//! `susi admin` — version sync, compliance audit, release orchestration,
//! config hot-reload, and the admin-pulse commands.

use super::defs::AdminCommands;
use super::shell_cli::MissionHost;

use susi::SUSI_VERSION;

pub(crate) fn run(subcommand: AdminCommands, host: &MissionHost) {
    let (cwd, global_dir) = (host.cwd, host.global_dir);
    let cfg = &host.cfg;
    match subcommand {
        AdminCommands::Sync => {
            match susi_gawd::admin::SusiAdmin::enforce_version_consistency(cwd) {
                Ok(v) => println!("Version synchronization complete: v{}", v),
                Err(e) => {
                    eprintln!("Sync failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        AdminCommands::Pulse { intent } => {
            let intent_str = intent.join(" ");
            match susi_gawd::admin::SusiAdmin::ingest_natural_intent(cwd, &intent_str) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("Pulse ingestion failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        AdminCommands::Audit => {
            // Use fast static compliance audit instead of LLM agent swarm
            match susi_gawd::admin::SusiAdmin::audit_compliance(cwd, Some("push")) {
                Ok(report) => println!("{}", report),
                Err(e) => {
                    eprintln!("Compliance audit failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        AdminCommands::Verify => {
            let answer = host
                .ama
                .solve_clean(&cfg.admin_pulses().verify_pulse, cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        AdminCommands::Release { cut } => {
            match susi_gawd::admin::SusiAdmin::execute_release(cwd, cut) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("Release failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        AdminCommands::Lint => {
            let answer = host
                .ama
                .solve_clean(&cfg.admin_pulses().lint_pulse, cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        AdminCommands::AuditDeps => {
            let answer =
                host.ama
                    .solve_clean(&cfg.admin_pulses().audit_deps_pulse, cwd, SUSI_VERSION);
            println!("{}", answer);
        }
        AdminCommands::Reload => match susi_sandbox::manager::SusiConfig::reload(global_dir) {
            Ok(reloaded) => {
                println!(
                    "Dynamic configuration reloaded successfully from {}.",
                    global_dir.join("config.json").display()
                );
                println!("- Engine: {}", reloaded.default_engine());
                println!("- Model: {}", reloaded.default_model());
                // Ladder resolution lives in gemi-models and takes that crate's
                // vendored `SusiConfig` (same JSON on disk as the sandbox service).
                let ladder_cfg =
                    susi_gemi::models::susi_sandbox::manager::SusiConfig::load(global_dir)
                        .unwrap_or_default();
                println!(
                    "- Model Ladder Steps: {}",
                    susi_gemi::hf_discovery::resolve_model_ladder(&ladder_cfg).len()
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
        },
    }
}
