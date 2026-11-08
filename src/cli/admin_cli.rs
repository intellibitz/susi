//! `susi admin` — compliance audit, audit evidence, config hot-reload, the
//! intent ladder and the admin-pulse commands. Version sync, version
//! verification and the release gate (`sync`, `verify`, `release`) and
//! intent-gap adoption moved to Susi Commercial Apps' dev workflow
//! (`susi-commercial admin ...`); they are refused here naming that command.

use super::defs::{AdminCommands, AuditEvidenceCommands};
use super::mission_host::MissionHost;

use std::collections::BTreeMap;
use std::path::Path;

/// Admin verbs that moved to Susi Commercial Apps' dev workflow.
const MOVED_VERBS: [&str; 3] = ["sync", "verify", "release"];

/// The refusal for `susi admin <words>`: a moved verb names its commercial
/// command; anything else is not an admin command.
pub(crate) fn moved_message(words: &[String]) -> String {
    match words.first().map(String::as_str) {
        Some(verb) if MOVED_VERBS.contains(&verb) => format!(
            "`susi admin {verb}` moved to Susi Commercial Apps: run `susi-commercial admin {verb}` \
             in the checkout"
        ),
        Some(other) => {
            format!("`susi admin {other}` is not an admin command (see `susi admin --help`)")
        }
        None => "`susi admin` needs a command (see `susi admin --help`)".to_string(),
    }
}

/// The refusal for `susi admin intent-ladder --adopt-gap <id>`.
pub(crate) fn adopt_gap_message(id: &str) -> String {
    format!(
        "adopting an intent gap authors a task-queue record, which moved to Susi Commercial          Apps: run `susi-commercial admin intent-ladder --adopt-gap {id}` in the checkout"
    )
}

pub(crate) fn run(subcommand: AdminCommands, host: &MissionHost) {
    let (cwd, global_dir) = (host.cwd, host.global_dir);
    match subcommand {
        AdminCommands::Pulse { intent } => {
            let intent_str = intent.join(" ");
            match susi_gawd::compliance::SusiAdmin::ingest_natural_intent(cwd, &intent_str) {
                Ok(msg) => println!("{}", msg),
                Err(e) => {
                    eprintln!("Pulse ingestion failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        AdminCommands::Audit => {
            // Use fast static compliance audit instead of LLM agent swarm
            match susi_gawd::compliance::SusiAdmin::audit_compliance(cwd, Some("push")) {
                Ok(report) => println!("{}", report),
                Err(e) => {
                    eprintln!("Compliance audit failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
        AdminCommands::AuditEvidence { action } => run_audit_evidence(action),
        // Deterministic checks and real tools, not model narration: the
        // swarm used to be asked to "run clippy" and "run cargo audit" and
        // would report outcomes nobody measured.
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
            if let Some(id) = adopt_gap {
                eprintln!("{}", adopt_gap_message(&id));
                std::process::exit(1);
            }
            let action = if gaps {
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
        AdminCommands::Moved(words) => {
            eprintln!("{}", moved_message(&words));
            std::process::exit(
                if words
                    .first()
                    .is_some_and(|w| MOVED_VERBS.contains(&w.as_str()))
                {
                    1
                } else {
                    2
                },
            );
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

#[cfg(test)]
mod moved_tests {
    use super::{adopt_gap_message, moved_message};

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn moved_admin_verbs_name_their_commercial_command() {
        for verb in ["sync", "verify", "release"] {
            let msg = moved_message(&words(&[verb, "--cut", "patch"]));
            assert!(
                msg.contains(&format!("`susi-commercial admin {verb}`")),
                "{msg}"
            );
            assert!(!msg.contains("rebuild"), "{msg}");
        }
        assert!(moved_message(&words(&["frobnicate"])).contains("not an admin command"));
        assert!(adopt_gap_message("r1")
            .contains("`susi-commercial admin intent-ladder --adopt-gap r1`"));
    }
}
