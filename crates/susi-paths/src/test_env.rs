//! Scoped process-env mutation for tests that isolate themselves from the
//! launching instance.
//!
//! `SusiDirs` resolves `SUSI_HOME` ahead of `HOME`/`XDG_*`, and ports honor
//! `SUSI_PORT_OFFSET`, so a test that only swaps `HOME`/`XDG_CONFIG_HOME`
//! still reads and writes the launching instance (e.g. `~/.susi-dev`) when
//! run from a dev binary. [`EnvGuard::isolated`] removes both instance knobs;
//! every change is undone in reverse order on drop.
//!
//! Callers keep holding their own env lock — this only saves and restores.

use std::ffi::{OsStr, OsString};

/// Instance-selecting variables an isolated test must not inherit.
const INSTANCE_VARS: [&str; 2] = ["SUSI_HOME", "SUSI_PORT_OFFSET"];

/// Restores every variable it touched (to its prior value, or unset) on drop.
#[derive(Debug)]
#[must_use = "the environment is restored when the guard is dropped"]
pub struct EnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    /// Guard with the launching instance's `SUSI_HOME` / `SUSI_PORT_OFFSET`
    /// removed for its lifetime.
    pub fn isolated() -> Self {
        let mut guard = Self { saved: Vec::new() };
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
        if cfg!(windows) && var == "HOME" {
            let home = std::path::Path::new(value.as_ref());
            self.save("USERPROFILE");
            std::env::set_var("USERPROFILE", home);
            for (app_data, leaf) in [("APPDATA", "Roaming"), ("LOCALAPPDATA", "Local")] {
                self.save(app_data);
                std::env::set_var(app_data, home.join("AppData").join(leaf));
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

    /// Swapping `HOME` swaps the whole home: on Windows that is
    /// `USERPROFILE`, and both come back on drop.
    #[test]
    fn a_swapped_home_is_the_resolved_home_and_is_restored() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before_home = std::env::var_os("HOME");
        let before_profile = std::env::var_os("USERPROFILE");
        let tmp = std::env::temp_dir().join(format!("susi_env_guard_{}", std::process::id()));
        {
            let mut env = EnvGuard::isolated();
            env.set("HOME", &tmp);
            assert_eq!(crate::xdg::home_dir().as_deref(), Some(tmp.as_path()));
        }
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
}
