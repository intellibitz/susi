//! A child launched through `test_env::isolate_home` resolves every susi dir
//! under the home it was given, whatever profile the launching process has.
//!
//! The launching process here stands in for a developer's shell: its `HOME`,
//! `USERPROFILE`, `%APPDATA%`, `%LOCALAPPDATA%`, `XDG_*_HOME` and `SUSI_HOME`
//! all point at a decoy "real profile", and `SUSI_PORT_OFFSET` at the
//! launching instance's ports. A child handed only `HOME` resolved
//! that decoy on Windows (home from `USERPROFILE`, config and data from the
//! app-data dirs) and on Linux (from the inherited `XDG_*_HOME`) — which is how
//! integration tests wrote the operator's real `~/.susi` and
//! `AppData\Roaming\intellibitz\susi`. `paths_probe` prints what `SusiDirs`
//! resolves in the child, exactly as the `susi` binary would.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use susi_paths::test_env::{isolate_home, EnvGuard};

/// These tests swap this process's env for the decoy profile; one at a time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const DIRS: [&str; 5] = [
    "home_dir",
    "config_dir",
    "data_dir",
    "cache_dir",
    "substrate_home",
];

/// A fresh `<tmp>/<tag>` with a `home` for the child and a `decoy` profile.
fn scratch(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("susi-isolate-home-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (home, decoy) = (root.join("home"), root.join("decoy"));
    std::fs::create_dir_all(&home).unwrap();
    // The decoy carries a legacy root too, as a real profile with an install
    // does: a child that reached it would take it.
    std::fs::create_dir_all(decoy.join(".susi")).unwrap();
    (root, home, decoy)
}

/// The port offset the launching instance runs at (a dev build's).
const INHERITED_OFFSET: &str = "100";

/// Point every variable a child could take its home or instance from at
/// `decoy`, for the guard's lifetime.
fn inherit_a_real_profile(decoy: &Path) -> EnvGuard {
    let mut env = EnvGuard::isolated();
    env.set("HOME", decoy)
        .set("USERPROFILE", decoy)
        .set("APPDATA", decoy.join("AppData").join("Roaming"))
        .set("LOCALAPPDATA", decoy.join("AppData").join("Local"))
        .set("XDG_CONFIG_HOME", decoy.join(".config"))
        .set("XDG_DATA_HOME", decoy.join(".local").join("share"))
        .set("XDG_CACHE_HOME", decoy.join(".cache"))
        .set("SUSI_HOME", decoy.join(".susi-dev"))
        .set("SUSI_PORT_OFFSET", INHERITED_OFFSET)
        .remove("SUSI_XDG");
    env
}

fn probe() -> Command {
    Command::new(env!("CARGO_BIN_EXE_paths_probe"))
}

/// What the child resolved: `SusiDirs`' dirs by key, and its port offset.
struct Resolved {
    dirs: HashMap<String, PathBuf>,
    port_offset: String,
}

fn resolved(cmd: &mut Command) -> Resolved {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "paths_probe failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut lines: HashMap<String, String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_string(), value.trim_end().to_string()))
        .collect();
    let port_offset = lines
        .remove("port_offset")
        .expect("paths_probe printed no port_offset");
    for key in DIRS {
        assert!(lines.contains_key(key), "paths_probe printed no {key}");
    }
    Resolved {
        dirs: lines
            .into_iter()
            .map(|(key, dir)| (key, PathBuf::from(dir)))
            .collect(),
        port_offset,
    }
}

/// Every dir under `home`, none in the decoy profile.
fn assert_all_under(dirs: &HashMap<String, PathBuf>, home: &Path, decoy: &Path) {
    for (key, dir) in dirs {
        assert!(
            dir.starts_with(home) && !dir.starts_with(decoy),
            "{key} = {} is outside the isolated home {}",
            dir.display(),
            home.display()
        );
    }
}

/// The platform layout this OS gives a home with no legacy `~/.susi`.
fn platform_config_dir(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("AppData")
            .join("Roaming")
            .join("intellibitz")
            .join("susi")
            .join("config")
    } else if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("intellibitz.susi")
    } else {
        home.join(".config").join("susi")
    }
}

#[test]
fn a_child_resolves_every_dir_under_its_home() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, home, decoy) = scratch("fresh");
    let env = inherit_a_real_profile(&decoy);

    let child = resolved(isolate_home(&mut probe(), &home));

    drop(env);
    let _ = std::fs::remove_dir_all(&root);
    let dirs = &child.dirs;
    assert_all_under(dirs, &home, &decoy);
    assert_eq!(dirs["home_dir"], home);
    assert_eq!(dirs["config_dir"], platform_config_dir(&home));
    // The launching instance's offset stays with it, as `EnvGuard::isolated`
    // drops it in-process: a dev build keeps an inherited offset, so the
    // child would otherwise talk to that instance's ports.
    assert_eq!(
        child.port_offset, "0",
        "the child inherited the port offset"
    );
}

/// A legacy root under the isolated home is still the layout: the helper
/// moves the home, it does not choose a layout of its own.
#[test]
fn a_legacy_root_under_the_home_still_wins() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, home, decoy) = scratch("legacy");
    std::fs::create_dir_all(home.join(".susi")).unwrap();
    let env = inherit_a_real_profile(&decoy);

    let dirs = resolved(isolate_home(&mut probe(), &home)).dirs;

    drop(env);
    let _ = std::fs::remove_dir_all(&root);
    assert_all_under(&dirs, &home, &decoy);
    for key in ["config_dir", "data_dir", "cache_dir"] {
        assert_eq!(dirs[key], home.join(".susi"), "{key}");
    }
}

/// What the caller sets on the command is theirs, before the helper runs or
/// after: an explicit instance root and port offset, and its own
/// `XDG_*_HOME` layout.
#[test]
fn the_callers_own_choices_are_kept() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (root, home, decoy) = scratch("caller");
    let env = inherit_a_real_profile(&decoy);
    let instance = home.join("instance");

    let mut before = probe();
    before
        .env("SUSI_HOME", &instance)
        .env("SUSI_PORT_OFFSET", "7")
        .env("XDG_CONFIG_HOME", home.join("xdg"));
    let before_child = resolved(isolate_home(&mut before, &home));
    let mut after = probe();
    isolate_home(&mut after, &home)
        .env("SUSI_HOME", &instance)
        .env("SUSI_PORT_OFFSET", "7");
    let after_child = resolved(&mut after);

    drop(env);
    let _ = std::fs::remove_dir_all(&root);
    for child in [&before_child, &after_child] {
        assert_all_under(&child.dirs, &home, &decoy);
        assert_eq!(child.dirs["config_dir"], instance);
        assert_eq!(child.port_offset, "7");
    }
    let xdg_config = before
        .get_envs()
        .find(|(key, _)| key.to_str() == Some("XDG_CONFIG_HOME"))
        .and_then(|(_, value)| value);
    assert_eq!(xdg_config, Some(home.join("xdg").as_os_str()));
}
