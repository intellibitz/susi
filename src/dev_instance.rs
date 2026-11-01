//! Dev-build instance isolation.
//!
//! The installed `~/.susi/bin/susi` is the release instance: it owns
//! `~/.susi` and the canonical ports, and it builds the next susi. Any other
//! `susi` binary — `target/debug`, `target/release`, a release-sync build
//! checking its `--version` — is a dev build and runs as its own instance:
//! `SUSI_HOME=~/.susi-dev` with `SUSI_PORT_OFFSET=100` (ports 9190–9194), so
//! it starts and talks to its *own* daemon running the dev binary, never the
//! release one. Both variables propagate to every child (the daemon is
//! spawned through `systemd-run`, which forwards `SUSI_*`). An explicit
//! `SUSI_HOME` wins, so `SUSI_HOME=~/.susi target/release/susi` still reaches
//! the release instance on purpose.
//!
//! The release instance's model weights and `cloud.env` are symlinked in,
//! not copied: tens of GB of GGUF weights and the operator's provider keys
//! are shared; config, audit chain, locks, and ports are not.

use std::env;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub(crate) const DEV_HOME_DIR: &str = ".susi-dev";
pub(crate) const DEV_PORT_OFFSET: &str = "100";

fn installed_binary(home: &Path) -> PathBuf {
    home.join(".susi")
        .join("bin")
        .join(if cfg!(windows) { "susi.exe" } else { "susi" })
}

/// True when `exe` is the installed release binary. A daemon still running
/// after its binary was swapped reports `… (deleted)`; that is still the
/// release instance.
fn is_installed_binary(exe: &Path, home: &Path) -> bool {
    let exe = exe.to_string_lossy();
    let exe = Path::new(exe.strip_suffix(" (deleted)").unwrap_or(&exe));
    let installed = installed_binary(home);
    exe == installed
        || std::fs::canonicalize(exe)
            .ok()
            .zip(std::fs::canonicalize(&installed).ok())
            .is_some_and(|(a, b)| a == b)
}

/// Selects the dev instance for any binary other than the installed one.
/// Must run first in `main`, before any thread exists: environment mutation
/// is only sound while the process is single-threaded.
pub(crate) fn isolate_if_dev_build(home: &Path) {
    if env::var_os("SUSI_HOME").is_some() {
        return;
    }
    let Ok(exe) = env::current_exe() else {
        return;
    };
    match initialize_local_channel(&exe, home) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("Cannot open the installed local Susi channel: {error}");
            std::process::exit(1);
        }
    }
    if is_installed_binary(&exe, home) {
        return;
    }
    // Resolved before SUSI_HOME is set, so these are the release instance's.
    let release_models = susi_paths::SusiDirs::data_dir().join("models");
    let release_cloud_env = susi_paths::SusiDirs::config_dir().join("cloud.env");

    let root = home.join(DEV_HOME_DIR);
    env::set_var("SUSI_HOME", &root);
    if env::var_os("SUSI_PORT_OFFSET").is_none() {
        env::set_var("SUSI_PORT_OFFSET", DEV_PORT_OFFSET);
    }
    share(&release_models, &root.join("models"));
    share(&release_cloud_env, &root.join("cloud.env"));
}

/// A direct Start Menu launch uses the existing native startup hook, avoiding
/// a second interpreter. Explicit SUSI_HOME above still wins for test isolation.
fn initialize_local_channel(exe: &Path, home: &Path) -> io::Result<bool> {
    let Some(slot) = exe.parent() else {
        return Ok(false);
    };
    let Some(builds) = slot.parent() else {
        return Ok(false);
    };
    let Some(channel) = builds.parent() else {
        return Ok(false);
    };
    let name = channel.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let slot_hash = slot.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if builds.file_name().is_none_or(|s| s != "builds")
        || !matches!(name, "dev" | "production")
        || slot_hash.len() != 64
        || !slot_hash.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(false);
    }
    let invalid = |message| io::Error::new(io::ErrorKind::InvalidData, message);
    let manifest = read_channel_json(&channel.join("channel.json"))?;
    let current = manifest
        .get("current")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("channel manifest has no current executable".to_string()))?;
    let expected = manifest
        .get("sha256")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if std::fs::canonicalize(current)? != std::fs::canonicalize(exe)?
        || !expected.eq_ignore_ascii_case(slot_hash)
        || manifest.get("channel").and_then(|v| v.as_str()) != Some(name)
    {
        return Err(invalid(
            "executable is not the active verified channel build".to_string(),
        ));
    }
    let mut file = std::fs::File::open(exe)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if !hex::encode(hash.finalize()).eq_ignore_ascii_case(expected) {
        return Err(invalid("installed executable checksum changed".to_string()));
    }

    let canonical = home.join(".susi");
    let root = channel.join("home");
    std::fs::create_dir_all(&root)?;
    let models = canonical.join("models");
    if models.is_dir() {
        let path = root.join("config.json");
        let mut config: serde_json::Value = if path.is_file() {
            read_channel_json(&path)?
        } else {
            serde_json::json!({})
        };
        let object = config
            .as_object_mut()
            .ok_or_else(|| invalid("local channel configuration is not an object".to_string()))?;
        let paths = object
            .entry("local_scan_paths")
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or_else(|| invalid("local_scan_paths is not an array".to_string()))?;
        let model_path = serde_json::Value::String(models.to_string_lossy().into_owned());
        if !paths.contains(&model_path) {
            paths.push(model_path);
            let temporary = path.with_extension(format!("json.{}.new", std::process::id()));
            std::fs::write(&temporary, serde_json::to_vec_pretty(&config)?)?;
            std::fs::rename(temporary, path)?;
        }
    }
    if let Ok(text) = std::fs::read_to_string(canonical.join("cloud.env")) {
        for (key, value) in susi_config::cloud_env::parse_env_file(&text) {
            if provider_connection_variable(&key) && env::var_os(&key).is_none_or(|v| v.is_empty())
            {
                env::set_var(key, value);
            }
        }
    }
    env::set_var("SUSI_HOME", root);
    env::set_var(
        "SUSI_PORT_OFFSET",
        if name == "dev" { "200" } else { "300" },
    );
    Ok(true)
}

fn read_channel_json(path: &Path) -> io::Result<serde_json::Value> {
    let bytes = std::fs::read(path)?;
    // Windows PowerShell 5.1's earlier UTF8 writer added a BOM.
    serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes))
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn provider_connection_variable(key: &str) -> bool {
    key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && [
            "_API_KEY",
            "_API_BASE",
            "_BASE_URL",
            "_MODEL",
            "_API_VERSION",
        ]
        .iter()
        .any(|suffix| key.ends_with(suffix))
}

/// Best effort: a missing share only means the dev instance starts without
/// it (downloads its own models, has no cloud keys).
fn share(release: &Path, dev: &Path) {
    if !release.exists() || dev.symlink_metadata().is_ok() {
        return;
    }
    if let Some(parent) = dev.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    #[cfg(unix)]
    let _ = std::os::unix::fs::symlink(release, dev);
    #[cfg(windows)]
    let _ = if release.is_dir() {
        std::os::windows::fs::symlink_dir(release, dev)
    } else {
        std::os::windows::fs::symlink_file(release, dev)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_provider_connection_variables_are_shared() {
        for key in ["OPENAI_API_KEY", "ANTHROPIC_BASE_URL", "DEEPSEEK_MODEL"] {
            assert!(provider_connection_variable(key));
        }
        for key in ["PATH", "SUSI_HOME", "RUSTC_WRAPPER", "COMMAND_API_KEY\n"] {
            assert!(!provider_connection_variable(key));
        }
    }

    #[test]
    fn only_the_installed_binary_is_the_release_instance() {
        let home = Path::new("/home/u");
        let bin = if cfg!(windows) { "susi.exe" } else { "susi" };
        assert!(is_installed_binary(&home.join(".susi/bin").join(bin), home));
        assert!(!is_installed_binary(
            Path::new("/home/u/src/susi/target/release/susi"),
            home
        ));
        assert!(!is_installed_binary(
            Path::new("/home/u/.susi/build-cache/release-sync/release/susi"),
            home
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_swapped_out_release_daemon_is_still_the_release_instance() {
        let home = Path::new("/home/u");
        assert!(is_installed_binary(
            Path::new("/home/u/.susi/bin/susi (deleted)"),
            home
        ));
    }
}
