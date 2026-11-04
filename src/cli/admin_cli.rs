//! `susi admin` — version sync, compliance audit, release orchestration,
//! config hot-reload, and the admin-pulse commands.

use super::defs::{AdminCommands, AuditEvidenceCommands};
use super::mission_host::MissionHost;

use std::collections::BTreeMap;
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
            // Windows cannot replace a running image: the release gate's
            // cargo test/clippy rebuild `target\debug\susi.exe` while this
            // process still holds it open. Stage a copy and exit, same as
            // `workflow finish` (see workflow_cli::hand_off_finish).
            #[cfg(windows)]
            if hand_off_release(cwd) {
                return;
            }
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
        // `susi_daemon::host_views` builds these documents; the MCP
        // `provider_contract`, `config_reload` and `intent_ladder` tools
        // return the same ones.
        AdminCommands::ProviderContract => {
            let _ = crate::cli_json::print_json(&susi_daemon::host_views::provider_contract());
        }
        AdminCommands::Reload => match susi_daemon::host_views::config_reload(global_dir) {
            Ok(doc) => {
                for line in doc["lines"].as_array().into_iter().flatten() {
                    println!("{}", line.as_str().unwrap_or_default());
                }
            }
            Err(e) => {
                eprintln!("Config reload failed: {}", e);
                std::process::exit(1);
            }
        },
        AdminCommands::IntentLadder {
            run,
            slice,
            verify,
            gaps,
            adopt_gap,
        } => {
            use susi_daemon::host_views::IntentLadder;
            let action = if let Some(id) = adopt_gap {
                IntentLadder::AdoptGap(id)
            } else if gaps {
                IntentLadder::Gaps
            } else if let Some(id) = verify {
                IntentLadder::Verify(id)
            } else if slice {
                IntentLadder::Slice
            } else if run {
                IntentLadder::Run
            } else {
                IntentLadder::Scorecard
            };
            match susi_daemon::host_views::intent_ladder(&action, cwd) {
                Ok(doc) => {
                    let _ = crate::cli_json::print_json(&doc);
                    if matches!(action, IntentLadder::Verify(_)) && doc["verdict"] != "pass" {
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
        }
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
    susi_gawd::audit_evidence::read_export(path)
}

/// One implementation with the MCP `audit_evidence_export` tool
/// (`susi_gawd::audit_evidence::export_to_file`).
fn export_audit_evidence(input: &Path, output: &Path) -> Result<(), String> {
    let segments = susi_gawd::audit_evidence::export_to_file(input, output)?;
    println!(
        "Audit evidence export written: {} segments to {}",
        segments,
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

/// Set on the staged copy so it runs the release in place instead of
/// staging itself again.
#[cfg(windows)]
const RELEASE_STAGED_ENV: &str = "SUSI_RELEASE_STAGED";

/// Copy this process out of `target\` and re-run the same `admin release`
/// argv from the copy. Returns `true` when the caller must exit so cargo
/// can relink `target\debug\susi.exe`.
#[cfg(windows)]
fn hand_off_release(cwd: &Path) -> bool {
    if std::env::var_os(RELEASE_STAGED_ENV).is_some() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let in_target = exe.components().any(|component| {
        component
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case("target")
    });
    if !in_target {
        return false;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "susi-release-{}-{}-{}",
        std::process::id(),
        stamp,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ));
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let name = exe
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("susi.exe"));
    let staged = dir.join(name);
    if std::fs::copy(&exe, &staged).is_err() {
        return false;
    }
    let Ok(mut child) = std::process::Command::new(&staged)
        .args(std::env::args_os().skip(1))
        .current_dir(cwd)
        .env(RELEASE_STAGED_ENV, "1")
        .spawn()
    else {
        return false;
    };
    std::thread::sleep(std::time::Duration::from_millis(250));
    match child.try_wait() {
        Ok(Some(status)) => {
            eprintln!(
                "susi: the staged release copy {} exited immediately ({status})",
                staged.display()
            );
            false
        }
        Ok(None) => {
            eprintln!(
                "susi: running this release from the staged copy {} so the gate can relink {}",
                staged.display(),
                exe.display()
            );
            eprintln!(
                "susi: this process exits now; the release continues in the staged copy above"
            );
            true
        }
        Err(_) => false,
    }
}

/// Why `susi admin release --cut` must not start, asked by `main` before the
/// release runs: a version that is released, has an open release pull
/// request, or is reserved by another branch is refused at the cost of a
/// `git ls-remote` (`SusiAdmin::release_preflight`). Any other command: `None`.
pub(crate) fn release_preflight(subcommand: &AdminCommands, cwd: &Path) -> Option<String> {
    if let AdminCommands::Release { cut: Some(level) } = subcommand {
        susi_gawd::admin::SusiAdmin::release_preflight(cwd, *level)
    } else {
        None
    }
}
