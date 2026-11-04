#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Every home swap goes through `EnvGuard`, which on Windows also repoints
// %USERPROFILE% (the home there) and %APPDATA%/%LOCALAPPDATA% (the platform
// dirs), so this runs on every OS.

//! The XDG-vs-legacy layout is decided once per resolved root for the life
//! of a process. Creating `$HOME/.susi` mid-process (workspace-scoped state
//! does this whenever the workspace is `$HOME`) must not move `config_dir()`
//! from the XDG path to `~/.susi`: state written before the flip would be
//! read from a different file after it.
//!
//! One test function in its own binary: it mutates process env, so nothing
//! else may resolve paths concurrently.

use std::path::PathBuf;
use std::sync::Mutex;
use susi_paths::test_env::EnvGuard;
use susi_paths::SusiDirs;

static LAYOUT_LOCK: Mutex<()> = Mutex::new(());

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
    let _guard = LAYOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    env.set("HOME", &legacy_home);
    assert_eq!(SusiDirs::config_dir(), legacy_home.join(".susi"));

    // SUSI_XDG stays a live override over an existing legacy root.
    env.set("SUSI_XDG", "1");
    assert_ne!(SusiDirs::config_dir(), legacy_home.join(".susi"));
    env.remove("SUSI_XDG");
    assert_eq!(SusiDirs::config_dir(), legacy_home.join(".susi"));

    // SUSI_HOME still pins a fully isolated instance root.
    let instance = scratch("instance");
    env.set("SUSI_HOME", &instance);
    assert_eq!(SusiDirs::config_dir(), instance);
    env.remove("SUSI_HOME");

    // Back on the first HOME, the pinned XDG decision still holds.
    env.set("HOME", &home);
    assert_eq!(SusiDirs::config_dir(), config_before);

    drop(env);
    for dir in [home, legacy_home, instance] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

#[test]
fn scratch_creates_unique_existing_temp_directory() {
    let d1 = scratch("unit-a");
    let d2 = scratch("unit-b");
    assert!(d1.is_dir());
    assert!(d2.is_dir());
    assert_ne!(d1, d2);
    let _ = std::fs::remove_dir_all(d1);
    let _ = std::fs::remove_dir_all(d2);
}

#[test]
fn susi_home_override_pins_config_and_data_dirs_to_instance_root() {
    let _guard = LAYOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut env = EnvGuard::isolated();
    let instance = scratch("pinned-instance");
    env.set("SUSI_HOME", &instance);
    assert_eq!(SusiDirs::config_dir(), instance);
    assert_eq!(SusiDirs::data_dir(), instance);
    drop(env);
    let _ = std::fs::remove_dir_all(instance);
}

#[test]
fn legacy_home_with_existing_dot_susi_resolves_to_dot_susi() {
    let _guard = LAYOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut env = EnvGuard::isolated();
    env.remove("SUSI_XDG");
    let legacy = scratch("legacy-unit");
    let dot_susi = legacy.join(".susi");
    std::fs::create_dir_all(&dot_susi).unwrap();
    env.set("HOME", &legacy);
    assert_eq!(SusiDirs::config_dir(), dot_susi);
    drop(env);
    let _ = std::fs::remove_dir_all(legacy);
}

#[test]
fn susi_dirs_cache_dir_under_susi_home_matches_instance_root() {
    let _guard = LAYOUT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut env = EnvGuard::isolated();
    let instance = scratch("cache-instance");
    env.set("SUSI_HOME", &instance);
    assert_eq!(SusiDirs::cache_dir(), instance);
    drop(env);
    let _ = std::fs::remove_dir_all(instance);
}
