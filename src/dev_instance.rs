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
use std::path::{Path, PathBuf};

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
