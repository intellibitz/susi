//! Install an exact promoted Windows release through the release-sync process guard.
use sha2::{Digest, Sha256};
use std::{env, error::Error, fs, path::PathBuf, process::Command};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn qualified_path(report: &str, id: &str) -> Result<PathBuf> {
    if id.len() != 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err("expected a lowercase SHA256 release ID".into());
    }
    let prefix = format!("refs/susi/releases/current: {id} ");
    let path = report
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .ok_or("requested ID is not the current verified release")?;
    Ok(PathBuf::from(path))
}

fn run() -> Result<()> {
    if !cfg!(windows) {
        return Err("installed-upgrade requires Windows".into());
    }
    let args: Vec<_> = env::args().skip(1).collect();
    let (id, check_only) = match args.as_slice() {
        [id] => (id, false),
        [flag, id] if flag == "--check" => (id, true),
        _ => return Err("usage: installed-upgrade [--check] <qualified-release-id>".into()),
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("missing repository root")?
        .to_owned();
    let report = Command::new("cargo")
        .args(["local-release", "status"])
        .current_dir(&root)
        .output()?;
    if !report.status.success() {
        return Err(String::from_utf8_lossy(&report.stderr).into_owned().into());
    }
    let binary = qualified_path(&String::from_utf8(report.stdout)?, id)?;
    let raw = fs::read(binary.with_file_name("manifest.txt"))?;
    if Sha256::digest(&raw)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        != *id
    {
        return Err("qualified manifest changed".into());
    }
    let manifest = std::str::from_utf8(&raw)?;
    let field = |name: &str| {
        manifest
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .ok_or("missing manifest field")
    };
    if check_only {
        println!(
            "Verified release {id}, source {}, executable SHA256 {}",
            field("source=")?,
            field("artifact-sha256=")?
        );
        return Ok(());
    }
    let bash = PathBuf::from(env::var_os("ProgramFiles").ok_or("missing ProgramFiles")?)
        .join("Git/bin/bash.exe");
    // Use the same host lock and the same canonical release home as release-sync.
    let script = r#"set -euo pipefail
. scripts/posix-compat.sh
export SUSI_HOME="$(cygpath -aw "$HOME/.susi")"
exec 9>"$SUSI_HOME/release-sync.lock"
flock -n 9 || { echo 'release updater busy' >&2; exit 75; }
trap 'flock -u 9' EXIT
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File scripts/upgrade-qualified-windows.ps1 "$1" "$2" "$3"
"#;
    let status = Command::new(bash)
        .current_dir(root)
        .args(["-c", script, "installed-upgrade"])
        .arg(binary)
        .arg(field("artifact-sha256=")?)
        .arg(field("source=")?)
        .status()?;
    if !status.success() {
        return Err(format!("installed upgrade failed ({status})").into());
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_exact_verified_current_id() -> Result<()> {
        let id = "a".repeat(64);
        let report = format!("refs/susi/releases/current: {id} directory with spaces/susi.exe\n");
        assert_eq!(
            qualified_path(&report, &id)?,
            PathBuf::from("directory with spaces/susi.exe")
        );
        assert!(qualified_path(&report, &"b".repeat(64)).is_err());
        assert!(qualified_path(&report, "../escape").is_err());
        assert!(qualified_path(&report.replace("current", "previous"), &id).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn isolated_installed_upgrade() -> Result<()> {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or("missing repository root")?
            .to_owned();
        if let Some(home) = env::var_os("USERPROFILE") {
            fs::create_dir_all(PathBuf::from(home).join("AppData/Local"))?;
        }
        let cache = env::temp_dir().join(format!("susi-ps-module-cache-{}", std::process::id()));
        let status = Command::new("powershell.exe")
            .current_dir(root)
            .env("PSModuleAnalysisCachePath", &cache)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "scripts/test-qualified-upgrade.ps1",
            ])
            .status()?;
        let _ = fs::remove_file(cache);
        assert!(status.success());
        Ok(())
    }
}
