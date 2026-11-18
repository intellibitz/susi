//! The storage guard is served by Susi Commercial Apps.
//!
//! `local-snapshot guard` and the witness after every publish and ship hand
//! the check to `susi-commercial workflow storage-guard`, which audits the
//! storage remote with this clone as its witness and records what it saw
//! under `refs/susi-guard/` ([docs](../../docs/git-storage-publisher.md)).
//! Core keeps no copy of the audit: without Susi Commercial Apps the check
//! does not run, and every caller says so.
use std::path::Path;
use std::process::Command;
use susi_config::vault_exec::commercial_executable;

/// `1` skips the witness after a publish or ship.
pub const SKIP_ENV: &str = "SUSI_NO_STORAGE_GUARD";

const ABSENT: &str = "the storage guard is served by Susi Commercial Apps: install `susi-commercial`, or name it with SUSI_COMMERCIAL_BIN";

fn guard(root: &Path, remote: &str, witness: bool) -> Result<Command, String> {
    let program = commercial_executable().ok_or_else(|| ABSENT.to_owned())?;
    let mut command = Command::new(program);
    command
        .current_dir(root)
        .args(["workflow", "storage-guard"]);
    if witness {
        command.arg("--witness");
    }
    command.arg(remote);
    Ok(command)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

/// Audit `remote`: its summary when nothing was lost, otherwise the summary
/// and every finding.
pub fn audit(root: &Path, remote: &str) -> Result<String, String> {
    let out = guard(root, remote, false)?
        .output()
        .map_err(|error| format!("storage guard could not run: {error}"))?;
    let (stdout, stderr) = (text(&out.stdout), text(&out.stderr));
    if out.status.success() {
        Ok(stdout)
    } else {
        Err([stdout, stderr]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n"))
    }
}

/// After a publish or ship: the advisory to print, if any. It never changes
/// the caller's result.
pub fn witness(root: &Path, remote: &str) -> Option<String> {
    if std::env::var_os(SKIP_ENV).is_some_and(|value| value == "1") {
        return None;
    }
    let out = match guard(root, remote, true) {
        Ok(mut command) => command.output(),
        Err(absent) => return Some(format!("storage guard not run: {absent}")),
    };
    match out {
        Ok(out) => Some(text(&out.stderr)).filter(|advisory| !advisory.is_empty()),
        Err(error) => Some(format!("storage guard could not run: {error}")),
    }
}
