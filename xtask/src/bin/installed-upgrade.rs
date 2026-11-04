//! Install an exact promoted release through the release-sync process guard.
use sha2::{Digest, Sha256};
use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

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

fn verify_qualified_manifest(binary: &Path, id: &str) -> Result<(String, String)> {
    let raw = fs::read(binary.with_file_name("manifest.txt"))?;
    if sha256_hex(&raw) != id {
        return Err("qualified manifest changed".into());
    }
    let manifest = std::str::from_utf8(&raw)?;
    let field = |name: &str| {
        manifest
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .ok_or("missing manifest field")
    };
    Ok((
        field("source=")?.to_string(),
        field("artifact-sha256=")?.to_string(),
    ))
}

#[cfg_attr(windows, allow(dead_code))]
fn parse_staged_version_line(stdout: &str) -> Result<(String, String)> {
    let trimmed = stdout.trim();
    let version_number = trimmed
        .strip_prefix("susi ")
        .filter(|ver| !ver.trim().is_empty())
        .ok_or("Staged version probe returned unexpected format")?
        .to_string();
    Ok((trimmed.to_string(), version_number))
}

#[cfg_attr(windows, allow(dead_code))]
fn format_release_marker_json(version_number: &str, source: &str, installed_at: u64) -> String {
    format!(
        r#"{{"profile":"release","accelerator":null,"channel":"release","version":"{version_number}","commit":"{source}","installed_at":{installed_at}}}"#
    )
}

fn parse_upgrade_args(args: &[String]) -> Result<(String, bool, bool)> {
    match args {
        [id] if !id.starts_with('-') => Ok((id.clone(), false, false)),
        [flag, id] if flag == "--check" && !id.starts_with('-') => Ok((id.clone(), true, false)),
        [flag, id] if flag == "--verify-startup" && !id.starts_with('-') => {
            Ok((id.clone(), false, true))
        }
        _ => Err(
            "usage: installed-upgrade [--check | --verify-startup] <qualified-release-id>".into(),
        ),
    }
}

fn installed_binary_path_from(
    susi_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    let root = susi_home
        .or_else(|| home.map(|h| h.join(".susi")))
        .ok_or("missing SUSI_HOME or HOME")?;
    let exe = if cfg!(windows) { "susi.exe" } else { "susi" };
    Ok(root.join("bin").join(exe))
}

fn installed_binary_path() -> Result<PathBuf> {
    let susi_home = env::var_os("SUSI_HOME").map(PathBuf::from);
    let home = env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    installed_binary_path_from(susi_home, home)
}

fn run_startup_gate(root: &Path, binary: &Path) -> Result<()> {
    let status = Command::new("cargo")
        .current_dir(root)
        .args([
            "run",
            "--locked",
            "--offline",
            "-p",
            "xtask",
            "--bin",
            "startup-gate",
            "--",
            "--susi",
        ])
        .arg(binary)
        .arg("--skip-daemon-up")
        .status()?;
    if !status.success() {
        return Err(format!("post-upgrade startup-gate failed ({status})").into());
    }
    Ok(())
}

#[cfg(windows)]
fn execute_upgrade(root: &Path, binary: &Path, artifact_sha256: &str, source: &str) -> Result<()> {
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
        .arg(artifact_sha256)
        .arg(source)
        .status()?;
    if !status.success() {
        return Err(format!("installed upgrade failed ({status})").into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn execute_upgrade(_root: &Path, binary: &Path, artifact_sha256: &str, source: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    let candidate_bytes = fs::read(binary)?;
    if sha256_hex(&candidate_bytes) != artifact_sha256 {
        return Err("Candidate SHA256 mismatch".into());
    }
    let susi_home = env::var_os("SUSI_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".susi")))
        .ok_or("missing SUSI_HOME or HOME")?;
    let bin_dir = susi_home.join("bin");
    fs::create_dir_all(&bin_dir)?;

    let scratch = bin_dir.join(format!(".qualified-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    fs::create_dir_all(&scratch)?;

    let result = (|| -> Result<()> {
        let stage_bin = scratch.join("susi");
        fs::copy(binary, &stage_bin)?;
        fs::set_permissions(&stage_bin, fs::Permissions::from_mode(0o755))?;
        if sha256_hex(&fs::read(&stage_bin)?) != artifact_sha256 {
            return Err("Staged SHA256 mismatch".into());
        }
        let version_out = Command::new(&stage_bin).arg("--version").output()?;
        if !version_out.status.success() {
            return Err("Staged version probe failed".into());
        }
        let (version_line, version_number) =
            parse_staged_version_line(&String::from_utf8(version_out.stdout)?)?;

        let target_bin = bin_dir.join("susi");
        let prev_bin = bin_dir.join("susi.prev");
        let marker = bin_dir.join("susi.build.json");
        let prev_marker = bin_dir.join("susi.build.json.prev");

        if target_bin.exists() {
            fs::copy(&target_bin, &prev_bin)?;
        }
        if marker.exists() {
            fs::copy(&marker, &prev_marker)?;
        }

        let installed_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let stage_marker = scratch.join("marker.json");
        let marker_json = format_release_marker_json(&version_number, source, installed_at);
        fs::write(&stage_marker, marker_json)?;

        if let Err(err) = fs::rename(&stage_bin, &target_bin) {
            return Err(format!("Failed to swap staged binary: {err}").into());
        }
        if let Err(err) = fs::rename(&stage_marker, &marker) {
            if prev_bin.exists() {
                let _ = fs::copy(&prev_bin, &target_bin);
            }
            return Err(format!("Failed to update release marker: {err}").into());
        }
        println!("{version_line} installed; SHA256 {artifact_sha256}");
        Ok(())
    })();

    let _ = fs::remove_dir_all(&scratch);
    result
}

fn run() -> Result<()> {
    let args: Vec<_> = env::args().skip(1).collect();
    let (id, check_only, verify_startup) = parse_upgrade_args(&args)?;
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
    let binary = qualified_path(&String::from_utf8(report.stdout)?, &id)?;
    let (source, artifact_sha256) = verify_qualified_manifest(&binary, &id)?;
    if check_only {
        println!("Verified release {id}, source {source}, executable SHA256 {artifact_sha256}");
        return Ok(());
    }
    execute_upgrade(&root, &binary, &artifact_sha256, &source)?;
    if verify_startup {
        run_startup_gate(&root, &installed_binary_path()?)?;
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

    #[test]
    fn verifies_qualified_manifest_and_detects_tampering() -> Result<()> {
        let dir = env::temp_dir().join(format!("susi-manifest-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir)?;
        let binary = dir.join("susi");
        let manifest_path = dir.join("manifest.txt");
        let manifest_body = "source=0123456789abcdef0123456789abcdef01234567\nartifact-sha256=fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210\n";
        fs::write(&manifest_path, manifest_body)?;
        let id = sha256_hex(manifest_body.as_bytes());

        let (source, artifact_sha256) = verify_qualified_manifest(&binary, &id)?;
        assert_eq!(source, "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(
            artifact_sha256,
            "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210"
        );

        fs::write(&manifest_path, format!("{manifest_body}tampered=1\n"))?;
        assert!(verify_qualified_manifest(&binary, &id).is_err());

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn parses_staged_version_and_formats_release_marker() -> Result<()> {
        let (line, ver) = parse_staged_version_line("susi 0.21.47\n")?;
        assert_eq!(line, "susi 0.21.47");
        assert_eq!(ver, "0.21.47");
        assert!(parse_staged_version_line("unexpected 0.21.47").is_err());
        assert!(parse_staged_version_line("susi   ").is_err());

        let marker = format_release_marker_json(
            &ver,
            "0123456789abcdef0123456789abcdef01234567",
            1_762_250_000,
        );
        let parsed: serde_json::Value = serde_json::from_str(&marker)?;
        assert_eq!(
            parsed.get("profile").and_then(serde_json::Value::as_str),
            Some("release")
        );
        assert_eq!(
            parsed.get("channel").and_then(serde_json::Value::as_str),
            Some("release")
        );
        assert_eq!(
            parsed.get("version").and_then(serde_json::Value::as_str),
            Some("0.21.47")
        );
        assert_eq!(
            parsed.get("commit").and_then(serde_json::Value::as_str),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(
            parsed
                .get("installed_at")
                .and_then(serde_json::Value::as_u64),
            Some(1_762_250_000)
        );
        Ok(())
    }

    #[test]
    fn parses_upgrade_args_and_resolves_installed_binary_path() -> Result<()> {
        let id = "a".repeat(64);
        assert_eq!(
            parse_upgrade_args(std::slice::from_ref(&id))?,
            (id.clone(), false, false)
        );
        assert_eq!(
            parse_upgrade_args(&["--check".to_string(), id.clone()])?,
            (id.clone(), true, false)
        );
        assert_eq!(
            parse_upgrade_args(&["--verify-startup".to_string(), id.clone()])?,
            (id.clone(), false, true)
        );
        assert!(parse_upgrade_args(&[]).is_err());
        assert!(parse_upgrade_args(&["--unknown".to_string(), id]).is_err());
        assert!(
            parse_upgrade_args(&["--check".to_string(), "--verify-startup".to_string()]).is_err()
        );

        let expected_exe = if cfg!(windows) { "susi.exe" } else { "susi" };
        assert_eq!(
            installed_binary_path_from(Some(PathBuf::from("/custom/susi")), None)?,
            PathBuf::from("/custom/susi").join("bin").join(expected_exe)
        );
        assert_eq!(
            installed_binary_path_from(None, Some(PathBuf::from("/home/alice")))?,
            PathBuf::from("/home/alice")
                .join(".susi")
                .join("bin")
                .join(expected_exe)
        );
        assert!(installed_binary_path_from(None, None).is_err());
        assert!(installed_binary_path()?.ends_with(expected_exe));
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
