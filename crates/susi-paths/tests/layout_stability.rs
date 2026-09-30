#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Unix-only: Windows resolves home from %USERPROFILE% and the platform dirs
// from %APPDATA%, which this test does not repoint.
#![cfg(unix)]

//! The XDG-vs-legacy layout is decided once per resolved root for the life
//! of a process. Creating `$HOME/.susi` mid-process (workspace-scoped state
//! does this whenever the workspace is `$HOME`) must not move `config_dir()`
//! from the XDG path to `~/.susi`: state written before the flip would be
//! read from a different file after it.
//!
//! One test function in its own binary: it mutates process env, so nothing
//! else may resolve paths concurrently.

use std::path::PathBuf;
use susi_paths::test_env::EnvGuard;
use susi_paths::SusiDirs;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi_layout_{tag}_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn creating_legacy_dir_mid_process_keeps_the_xdg_layout() {
    // Scrubs SUSI_HOME/SUSI_PORT_OFFSET; everything touched is restored on drop.
    let mut env = EnvGuard::isolated();
    for var in ["SUSI_XDG", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
        env.remove(var);
    }

    // XDG install: no `$HOME/.susi` at first resolution. XDG_CONFIG_HOME
    // also forces the local resolver (no service lookup).
    let home = scratch("xdg");
    let xdg_config = home.join("xdg-config");
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", &xdg_config);
    let config_before = SusiDirs::config_dir();
    let data_before = SusiDirs::data_dir();
    assert_ne!(config_before, home.join(".susi"));
    assert_ne!(data_before, home.join(".susi"));

    std::fs::create_dir_all(home.join(".susi")).unwrap();
    assert_eq!(SusiDirs::config_dir(), config_before);
    assert_eq!(SusiDirs::data_dir(), data_before);

    // A repointed HOME is decided afresh: a legacy root that already exists
    // at its first resolution selects the legacy layout.
    let legacy_home = scratch("legacy");
    std::fs::create_dir_all(legacy_home.join(".susi")).unwrap();
    std::env::set_var("HOME", &legacy_home);
    assert_eq!(SusiDirs::config_dir(), legacy_home.join(".susi"));

    // SUSI_XDG stays a live override over an existing legacy root.
    std::env::set_var("SUSI_XDG", "1");
    assert_ne!(SusiDirs::config_dir(), legacy_home.join(".susi"));
    std::env::remove_var("SUSI_XDG");
    assert_eq!(SusiDirs::config_dir(), legacy_home.join(".susi"));

    // SUSI_HOME still pins a fully isolated instance root.
    let instance = scratch("instance");
    std::env::set_var("SUSI_HOME", &instance);
    assert_eq!(SusiDirs::config_dir(), instance);
    std::env::remove_var("SUSI_HOME");

    // Back on the first HOME, the pinned XDG decision still holds.
    std::env::set_var("HOME", &home);
    assert_eq!(SusiDirs::config_dir(), config_before);

    for dir in [home, legacy_home, instance] {
        let _ = std::fs::remove_dir_all(dir);
    }
}
