//! Scoped process-env mutation and child home isolation for Linux tests and probes.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Instance-selecting variables an isolated test must not inherit.
const INSTANCE_VARS: [&str; 2] = ["SUSI_HOME", "SUSI_PORT_OFFSET"];

fn xdg_under(home: &Path) -> [(&'static str, PathBuf); 3] {
    [
        ("XDG_CONFIG_HOME", home.join(".config")),
        ("XDG_DATA_HOME", home.join(".local").join("share")),
        ("XDG_CACHE_HOME", home.join(".cache")),
    ]
}

/// Point `cmd`'s child at `home` as its whole home while preserving caller overrides.
pub fn isolate_home<'c>(cmd: &'c mut Command, home: &Path) -> &'c mut Command {
    let set_by_caller =
        |cmd: &Command, var: &str| cmd.get_envs().any(|(k, v)| v.is_some() && k == var);
    cmd.env("SUSI_NO_REGISTRY_ENV", "1");
    cmd.env("HOME", home);
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

/// Restores every variable it touched (to its prior value, or unset) on drop.
#[derive(Debug)]
#[must_use = "the environment is restored when the guard is dropped"]
pub struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    /// Guard with `SUSI_HOME` / `SUSI_PORT_OFFSET` removed for its lifetime.
    pub fn isolated() -> Self {
        let mut guard = Self { saved: Vec::new() };
        guard.set("SUSI_NO_REGISTRY_ENV", "1");
        for var in INSTANCE_VARS {
            guard.remove(var);
        }
        guard
    }

    /// Set `var` until drop.
    pub fn set(&mut self, var: &'static str, value: impl AsRef<OsStr>) -> &mut Self {
        self.save(var);
        std::env::set_var(var, value.as_ref());
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
    use super::{isolate_home, EnvGuard};
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn isolate_home_sets_home_xdg_scrubs_instance_vars_and_preserves_caller_overrides() {
        let home = Path::new("/tmp/susi-isolated-child-home");
        let mut cmd = Command::new("susi");
        cmd.env("SUSI_NO_REGISTRY_ENV", "0");
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
    fn env_guard_swaps_home_and_restores_in_lifo_order() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before_home = std::env::var_os("HOME");
        let tmp_a = std::env::temp_dir().join(format!("susi_env_lifo_a_{}", std::process::id()));
        let tmp_b = std::env::temp_dir().join(format!("susi_env_lifo_b_{}", std::process::id()));
        {
            let mut env = EnvGuard::isolated();
            for var in ["XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"] {
                env.remove(var);
            }
            env.set("HOME", &tmp_a).remove("HOME").set("HOME", &tmp_b);
            assert_eq!(crate::xdg::home_dir().as_deref(), Some(tmp_b.as_path()));
            assert!(crate::xdg::config_dir().unwrap().starts_with(&tmp_b));
        }
        assert_eq!(std::env::var_os("HOME"), before_home);
    }
}
