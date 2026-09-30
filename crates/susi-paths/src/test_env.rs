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
    pub fn set(&mut self, var: &'static str, value: impl AsRef<OsStr>) -> &mut Self {
        self.save(var);
        std::env::set_var(var, value);
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
