//! `susi admin` — version sync, compliance audit, release orchestration,
//! config hot-reload, and the admin-pulse commands.

use super::defs::{AdminCommands, AuditEvidenceCommands};
use super::shell_cli::MissionHost;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use susi::SUSI_VERSION;

pub(crate) fn run(subcommand: AdminCommands, host: &MissionHost) {
    let (cwd, global_dir) = (host.cwd, host.global_dir);
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
        AdminCommands::AuditEvidence { action } => run_audit_evidence(action),
        // Deterministic checks and real tools, not model narration: the
        // swarm used to be asked to "verify", "run clippy", and "run cargo
        // audit" and would report outcomes nobody measured.
        AdminCommands::Verify => match susi_gawd::admin::SusiAdmin::verify_version_alignment(cwd) {
            Ok(()) => println!("Version alignment verified: v{SUSI_VERSION}"),
            Err(e) => {
                eprintln!("Version alignment check failed: {}", e);
                std::process::exit(1);
            }
        },
        AdminCommands::Release { cut } => {
            match susi_gawd::admin::SusiAdmin::execute_release(cwd, cut) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("Release failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        AdminCommands::Lint => run_cargo(
            cwd,
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ),
        AdminCommands::AuditDeps => run_cargo(cwd, &["audit"]),
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
                    susi_gemi::models::hf_discovery::resolve_model_ladder(&ladder_cfg).len()
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

fn run_audit_evidence(action: AuditEvidenceCommands) {
    let result = match action {
        AuditEvidenceCommands::Export { input, output } => export_audit_evidence(&input, &output),
        AuditEvidenceCommands::Verify { input, keys } => verify_audit_evidence(&input, &keys),
    };
    if let Err(error) = result {
        eprintln!("Audit evidence operation failed: {error}");
        std::process::exit(1);
    }
}

fn read_audit_export(path: &Path) -> Result<susi_gawd::audit_evidence::AuditExport, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("could not read {}: {error}", path.display()))?;
    if let Ok(export) = serde_json::from_slice(&bytes) {
        return Ok(export);
    }
    let segments: Vec<susi_gawd::audit_evidence::AuditSegment> = serde_json::from_slice(&bytes)
        .map_err(|error| {
            format!(
                "{} is not a valid audit evidence export: {error}",
                path.display()
            )
        })?;
    Ok(susi_gawd::audit_evidence::AuditExport {
        format: "susi-audit-evidence/v1".to_string(),
        expected_len: segments.len(),
        segments,
    })
}

fn export_audit_evidence(input: &Path, output: &Path) -> Result<(), String> {
    let export = read_audit_export(input)?;
    let encoded = serde_json::to_vec_pretty(&export)
        .map_err(|error| format!("could not serialize audit evidence: {error}"))?;
    fs::write(output, encoded)
        .map_err(|error| format!("could not write {}: {error}", output.display()))?;
    println!(
        "Audit evidence export written: {} segments to {}",
        export.segments.len(),
        output.display()
    );
    Ok(())
}

fn verify_audit_evidence(input: &Path, raw_keys: &[String]) -> Result<(), String> {
    let export = read_audit_export(input)?;
    let mut keys = BTreeMap::new();
    for raw in raw_keys {
        let Some((key_id, secret)) = raw.split_once('=') else {
            return Err(format!("invalid key {raw:?}; expected key-id=secret"));
        };
        if key_id.is_empty() || secret.is_empty() {
            return Err("key-id and secret must not be empty".to_string());
        }
        keys.insert(key_id.to_string(), secret.to_string());
    }
    export.verify(&keys).map_err(|failure| {
        let name = match failure {
            susi_gawd::audit_evidence::VerifyFailure::Truncation => "truncation",
            susi_gawd::audit_evidence::VerifyFailure::Tampering => "tampering",
            susi_gawd::audit_evidence::VerifyFailure::MissingSegment => "missing segment",
            susi_gawd::audit_evidence::VerifyFailure::UnknownKey => "unknown key",
        };
        format!("verification failed: {name}")
    })?;
    println!(
        "Audit evidence verified: {} segments",
        export.segments.len()
    );
    Ok(())
}

/// Run `cargo <args>` in `cwd` with inherited output and exit with its
/// status, so the operator sees the tool's real verdict.
fn run_cargo(cwd: &std::path::Path, args: &[&str]) {
    match std::process::Command::new("cargo")
        .args(args)
        .current_dir(cwd)
        .status()
    {
        Ok(status) if status.success() => {}
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("could not run `cargo {}`: {e}", args.join(" "));
            std::process::exit(1);
        }
    }
}
