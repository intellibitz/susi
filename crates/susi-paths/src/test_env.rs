//! Scoped process-env mutation for tests that isolate themselves from the
//! launching instance.
//!
//! `SusiDirs` resolves `SUSI_HOME` ahead of `HOME`/`XDG_*`, and ports honor
//! `SUSI_PORT_OFFSET`, so a test that only swaps `HOME`/`XDG_CONFIG_HOME`
//! still reads and writes the launching instance (e.g. `~/.susi-dev`) when
//! run from a dev binary. [`EnvGuard::isolated`] removes both instance knobs;
//! registry environment fallback is disabled, and every change is undone in reverse order on drop.
//!
//! Callers keep holding their own env lock — this only saves and restores.
//!
//! A test that launches a child (the `susi` binary, a script that runs it)
//! under a throwaway home uses [`isolate_home`] instead: it hands the child
//! the same whole-home swap, and the same instance scrub, without touching
//! this process's env. The release gate's E2E checks launch the release
//! binary the same way, so this module is not test-only code.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Instance-selecting variables an isolated test must not inherit.
const INSTANCE_VARS: [&str; 2] = ["SUSI_HOME", "SUSI_PORT_OFFSET"];

/// The variables that move with `HOME`, and where each points under `home`.
///
/// On Windows the profile (`USERPROFILE`) is the home directory and `HOME`
/// only its fallback, and the profile's `%APPDATA%` / `%LOCALAPPDATA%` hold
/// the platform layout (`xdg.rs`). Elsewhere nothing reads them. One list
/// for [`EnvGuard::set`] and [`isolate_home`], so a test that swaps its own
/// home and a child it launches resolve the same layout.
fn moved_with_home(home: &Path) -> Vec<(&'static str, PathBuf)> {
    if !cfg!(windows) {
        return Vec::new();
    }
    vec![
        ("USERPROFILE", home.to_path_buf()),
        ("APPDATA", home.join("AppData").join("Roaming")),
        ("LOCALAPPDATA", home.join("AppData").join("Local")),
    ]
}

/// The XDG base dirs [`isolate_home`] puts under `home`: the spec's
/// defaults, so a Linux child resolves exactly what `HOME` alone would give
/// it when nothing inherited overrides them.
fn xdg_under(home: &Path) -> [(&'static str, PathBuf); 3] {
    [
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local").join("share")),
        ("XDG_CACHE_HOME", home.join(".cache")),
    ]
}

/// Point `cmd`'s child at `home` as its whole home, and return `cmd`.
///
/// Setting `HOME` on a child is not enough: on Windows it resolves its home
/// from the inherited `USERPROFILE` and its config/data dirs from the
/// inherited `%APPDATA%` / `%LOCALAPPDATA%`, and on Linux from any inherited
/// `XDG_*_HOME` — so a test that set only `HOME` read and wrote the
/// developer's real `~/.susi` and `AppData\Roaming\intellibitz\susi` unless
/// the whole run happened to be isolated. This sets `HOME`, the variables
/// [`EnvGuard::set`] moves with it, and the three `XDG_*_HOME` dirs under
/// `home`, and drops the inherited instance knobs [`EnvGuard::isolated`]
/// drops: `SUSI_HOME` (which outranks all of the dirs) and
/// `SUSI_PORT_OFFSET` (a dev build keeps an inherited offset, so the child
/// would talk to the launching instance's ports).
///
/// What the caller set on `cmd` itself is theirs and kept, before or after
/// this call: its own `XDG_*_HOME` (a test's `home/xdg` layout), its own
/// `SUSI_HOME` (an explicit instance root) and its own `SUSI_PORT_OFFSET`.
/// Call it after any `env_clear`, which would discard it.
///
/// Every launch of `susi` under a throwaway home goes through here — the
/// integration tests and the release gate's end-to-end checks
/// (`SusiAdmin::run_e2e_checks`) alike — and `tests/child_home_isolation.rs`
/// keeps it that way.
pub fn isolate_home<'c>(cmd: &'c mut Command, home: &Path) -> &'c mut Command {
    // Windows env names are case-insensitive; elsewhere they are exact.
    let set_by_caller = |cmd: &Command, var: &str| {
        cmd.get_envs().any(|(k, v)| {
            v.is_some()
                && if cfg!(windows) {
                    k.eq_ignore_ascii_case(var)
                } else {
                    k == var
                }
        })
    };
    // Clearing a child's process environment does not clear Windows user or
    // machine environment registry values. Private-home runs never read them.
    cmd.env("SUSI_NO_REGISTRY_ENV", "1");
    cmd.env("HOME", home);
    for (var, dir) in moved_with_home(home) {
        cmd.env(var, dir);
    }
    for (var, dir) in xdg_under(home) {
        if !set_by_caller(cmd, var) {
            cmd.env(var, dir);
        }
    }
    for var in INSTANCE_VARS {
        if !set_by_caller(cmd, var) {
            cmd.env_remove(var);
        }
    }
    cmd
}

/// Preserve the caller's registry opt-out after `env_clear`, without changing
/// normal credential discovery for an ordinary, non-isolated child.
pub fn inherit_registry_isolation(cmd: &mut Command) -> &mut Command {
    if let Some(value) = std::env::var_os("SUSI_NO_REGISTRY_ENV") {
        cmd.env("SUSI_NO_REGISTRY_ENV", value);
    }
    cmd
}

/// Restores every variable it touched (to its prior value, or unset) on drop.
#[derive(Debug)]
#[must_use = "the environment is restored when the guard is dropped"]
pub struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    /// Guard with the launching instance's `SUSI_HOME` / `SUSI_PORT_OFFSET`
    /// removed and registry environment fallback disabled for its lifetime.
    pub fn isolated() -> Self {
        let mut guard = Self { saved: Vec::new() };
        guard.set("SUSI_NO_REGISTRY_ENV", "1");
        for var in INSTANCE_VARS {
            guard.remove(var);
        }
        guard
    }

    /// Set `var` until drop.
    ///
    /// `HOME` also moves `USERPROFILE` on Windows, where the profile is the
    /// home directory and `HOME` only its fallback: a test that swapped
    /// `HOME` alone still resolved the operator's real profile, and wrote
    /// the real `~/.susi/cloud.env` and extension state. The profile's
    /// `%APPDATA%` and `%LOCALAPPDATA%` move with it too: they are where the
    /// platform layout lives (`xdg.rs`), so with no legacy `~/.susi` under
    /// the swapped home a test still wrote `cloud.env` into the operator's
    /// real `AppData\Roaming\intellibitz\susi\config`.
    pub fn set(&mut self, var: &'static str, value: impl AsRef<OsStr>) -> &mut Self {
        self.save(var);
        std::env::set_var(var, value.as_ref());
        if var == "HOME" {
            for (moved, dir) in moved_with_home(Path::new(value.as_ref())) {
                self.save(moved);
                std::env::set_var(moved, dir);
            }
        }
        self
    }

    /// Unset `var` until drop.
    pub fn remove(&mut self, var: &'static str) -> &mut Self {
        self.save(var);
        std::env::remove_var(var);
        self
    }

    fn save(&mut self, var: &'static str) {
        self.saved.push((var, std::env::var_os(var)));
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        while let Some((var, prev)) = self.saved.pop() {
            match prev {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::EnvGuard;

    #[test]
    fn private_home_disables_registry_even_after_environment_clear() {
        let mut child = std::process::Command::new("unused");
        child.env_clear().env("SUSI_NO_REGISTRY_ENV", "0");
        super::isolate_home(&mut child, std::path::Path::new("private"));
        assert!(child
            .get_envs()
            .any(|(key, value)| key == "SUSI_NO_REGISTRY_ENV"
                && value == Some(std::ffi::OsStr::new("1"))));
    }

    #[test]
    fn registry_isolation_is_inherited_only_when_the_caller_requests_it() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut env = EnvGuard::isolated();
        env.remove("SUSI_NO_REGISTRY_ENV");
        let mut normal = std::process::Command::new("unused");
        normal.env_clear();
        super::inherit_registry_isolation(&mut normal);
        assert!(!normal
            .get_envs()
            .any(|(key, _)| key == "SUSI_NO_REGISTRY_ENV"));
        for value in ["", "0", "1"] {
            env.set("SUSI_NO_REGISTRY_ENV", value);
            let mut isolated = std::process::Command::new("unused");
            isolated.env_clear();
            super::inherit_registry_isolation(&mut isolated);
            assert!(isolated
                .get_envs()
                .any(|(key, found)| key == "SUSI_NO_REGISTRY_ENV"
                    && found == Some(std::ffi::OsStr::new(value))));
        }
    }

    /// Swapping `HOME` swaps the whole home: on Windows that is
    /// `USERPROFILE`, and both come back on drop.
    #[test]
    fn a_swapped_home_is_the_resolved_home_and_is_restored() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before_registry = std::env::var_os("SUSI_NO_REGISTRY_ENV");
        let before_home = std::env::var_os("HOME");
        let before_profile = std::env::var_os("USERPROFILE");
        let tmp = std::env::temp_dir().join(format!("susi_env_guard_{}", std::process::id()));
        {
            let mut env = EnvGuard::isolated();
            env.set("HOME", &tmp);
            assert_eq!(crate::xdg::home_dir().as_deref(), Some(tmp.as_path()));
        }
        assert_eq!(std::env::var_os("SUSI_NO_REGISTRY_ENV"), before_registry);
        assert_eq!(std::env::var_os("HOME"), before_home);
        assert_eq!(std::env::var_os("USERPROFILE"), before_profile);
    }

    /// With no legacy `~/.susi` the substrate takes the platform layout,
    /// whose dirs must sit under the swapped home on every OS: `~/.config`
    /// and friends on Linux, `~/Library` on macOS, and on Windows the
    /// profile's `AppData`, which used to stay the operator's real one.
    #[test]
    fn a_swapped_home_takes_the_platform_dirs_along() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = ["APPDATA", "LOCALAPPDATA"].map(std::env::var_os);
        let tmp = std::env::temp_dir().join(format!("susi_env_guard_dirs_{}", std::process::id()));
        {
            let mut env = EnvGuard::isolated();
            for var in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
                env.remove(var);
            }
            env.set("HOME", &tmp);
            for dir in [
                crate::xdg::config_dir(),
                crate::xdg::data_dir(),
                crate::xdg::cache_dir(),
            ] {
                let dir = dir.unwrap();
                assert!(
                    dir.starts_with(&tmp),
                    "{} is outside the swapped home",
                    dir.display()
                );
            }
        }
        assert_eq!(["APPDATA", "LOCALAPPDATA"].map(std::env::var_os), before);
    }

    #[test]
    fn isolate_home_sets_home_xdg_scrubs_instance_vars_and_preserves_caller_overrides() {
        use super::isolate_home;
        use std::collections::HashMap;
        use std::ffi::OsStr;
        use std::path::Path;
        use std::process::Command;

        let home = Path::new("/tmp/susi-isolated-child-home");
        let mut cmd = Command::new("susi");
        isolate_home(&mut cmd, home);
        let envs: HashMap<_, _> = cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(OsStr::to_os_string)))
            .collect();

        assert_eq!(
            envs.get("HOME").and_then(Option::as_ref),
            Some(&home.as_os_str().to_os_string())
        );
        assert_eq!(
            envs.get("XDG_CONFIG_HOME").and_then(Option::as_ref),
            Some(&home.join(".config").into_os_string())
        );
        assert_eq!(
            envs.get("XDG_DATA_HOME").and_then(Option::as_ref),
            Some(&home.join(".local").join("share").into_os_string())
        );
        assert_eq!(
            envs.get("XDG_CACHE_HOME").and_then(Option::as_ref),
            Some(&home.join(".cache").into_os_string())
        );
        assert_eq!(
            envs.get("SUSI_NO_REGISTRY_ENV").and_then(Option::as_ref),
            Some(&OsStr::new("1").to_os_string())
        );
        assert_eq!(envs.get("SUSI_HOME"), Some(&None));
        assert_eq!(envs.get("SUSI_PORT_OFFSET"), Some(&None));

        let custom_xdg = Path::new("/tmp/custom-xdg-cfg");
        let custom_susi = Path::new("/tmp/custom-susi-home");
        let mut custom_cmd = Command::new("susi");
        custom_cmd
            .env("XDG_CONFIG_HOME", custom_xdg)
            .env("SUSI_HOME", custom_susi)
            .env("SUSI_PORT_OFFSET", "250");
        isolate_home(&mut custom_cmd, home);
        let custom_envs: HashMap<_, _> = custom_cmd
            .get_envs()
            .map(|(k, v)| (k.to_string_lossy().into_owned(), v.map(OsStr::to_os_string)))
            .collect();

        assert_eq!(
            custom_envs.get("XDG_CONFIG_HOME").and_then(Option::as_ref),
            Some(&custom_xdg.as_os_str().to_os_string())
        );
        assert_eq!(
            custom_envs.get("SUSI_HOME").and_then(Option::as_ref),
            Some(&custom_susi.as_os_str().to_os_string())
        );
        assert_eq!(
            custom_envs.get("SUSI_PORT_OFFSET").and_then(Option::as_ref),
            Some(&OsStr::new("250").to_os_string())
        );
    }

    #[test]
    fn env_guard_reverses_multiple_mutations_on_same_variable_in_lifo_order() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before_registry = std::env::var_os("SUSI_NO_REGISTRY_ENV");
        let before_home = std::env::var_os("HOME");
        let before_profile = std::env::var_os("USERPROFILE");
        let tmp_a = std::env::temp_dir().join(format!("susi_env_lifo_a_{}", std::process::id()));
        let tmp_b = std::env::temp_dir().join(format!("susi_env_lifo_b_{}", std::process::id()));
        {
            let mut env = EnvGuard::isolated();
            env.set("HOME", &tmp_a);
            env.remove("HOME");
            env.set("HOME", &tmp_b);
            assert_eq!(crate::xdg::home_dir().as_deref(), Some(tmp_b.as_path()));
        }
        assert_eq!(std::env::var_os("SUSI_NO_REGISTRY_ENV"), before_registry);
        assert_eq!(std::env::var_os("HOME"), before_home);
        assert_eq!(std::env::var_os("USERPROFILE"), before_profile);
    }

    #[test]
    fn env_guard_set_and_remove_restore_custom_non_home_variables_on_drop() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        const KEY: &str = "SUSI_CUSTOM_TEST_ENV_VAR";
        let prior = std::env::var_os(KEY);
        {
            let mut env = EnvGuard::isolated();
            env.set(KEY, "alpha").remove(KEY).set(KEY, "beta");
            assert_eq!(std::env::var(KEY).as_deref(), Ok("beta"));
            assert!(!format!("{env:?}").is_empty());
        }
        assert_eq!(std::env::var_os(KEY), prior);
    }
}
