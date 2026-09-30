//! Shared fixtures for the integration tests.

// Each integration test compiles this module separately and uses a subset.
#![allow(dead_code)]

use std::path::PathBuf;

/// Swaps `HOME` / `USERPROFILE` / `XDG_CONFIG_HOME` to a fresh temp dir
/// (with `.susi` pre-created, so `SusiDirs` picks the deterministic legacy
/// path) and restores them — and removes the dir — on drop.
pub struct HomeGuard {
    tmp: PathBuf,
    // Scrubs `SUSI_HOME`/`SUSI_PORT_OFFSET` (they outrank HOME/XDG) and
    // restores every variable it touched; dropped before the dir is removed.
    env: Option<susi_paths::test_env::EnvGuard>,
}

impl HomeGuard {
    /// `tag` keeps concurrent test binaries' temp dirs apart.
    pub fn new(tag: &str) -> Self {
        let tmp = std::env::temp_dir().join(format!(
            "susi_{tag}_it_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".susi")).unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", &tmp);
        env.set("USERPROFILE", &tmp);
        env.set("XDG_CONFIG_HOME", tmp.join("xdg"));
        Self {
            tmp,
            env: Some(env),
        }
    }

    /// The legacy `.susi` config dir under the swapped HOME.
    pub fn config(&self) -> PathBuf {
        self.tmp.join(".susi")
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        self.env = None;
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}
