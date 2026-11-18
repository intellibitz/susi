#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Host directory contract for Susi core on Linux.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// Host directory contract resolved from local XDG/legacy rules.
pub struct SusiDirs;

/// Sample the wall clock once, returning zero if it precedes the Unix epoch.
#[inline]
pub fn unix_epoch_elapsed() -> Duration {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
}

impl SusiDirs {
    /// User home directory.
    #[must_use]
    pub fn home_dir() -> PathBuf {
        LocalDirs::home_dir()
    }
    /// Substrate config dir (`~/.susi` legacy or `$XDG_CONFIG_HOME/susi`).
    #[must_use]
    pub fn config_dir() -> PathBuf {
        LocalDirs::config_dir()
    }
    /// Substrate data dir (`~/.susi` legacy or `$XDG_DATA_HOME/susi`).
    #[must_use]
    pub fn data_dir() -> PathBuf {
        LocalDirs::data_dir()
    }
    /// Substrate cache dir (`~/.susi` legacy or `$XDG_CACHE_HOME/susi`).
    #[must_use]
    pub fn cache_dir() -> PathBuf {
        LocalDirs::cache_dir()
    }
    /// Host substrate root the daemon binds to (the data dir).
    #[must_use]
    pub fn substrate_home() -> PathBuf {
        LocalDirs::data_dir()
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

struct LocalDirs;

impl LocalDirs {
    fn instance_root() -> Option<PathBuf> {
        let p = std::env::var_os("SUSI_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())?;
        if std::env::var_os("SUSI_HERMETIC_FORBIDDEN")
            .map(PathBuf::from)
            .is_some_and(|f| same_path(&f, &p))
        {
            return None;
        }
        Some(p)
    }

    fn legacy_base() -> PathBuf {
        Self::instance_root().unwrap_or_else(|| Self::home_dir().join(".susi"))
    }

    fn home_dir() -> PathBuf {
        xdg::home_dir().unwrap_or_else(|| PathBuf::from("."))
    }

    fn use_xdg() -> bool {
        if Self::instance_root().is_some() {
            return false;
        }
        if Self::legacy_present(&Self::legacy_base()) {
            std::env::var("SUSI_XDG")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false)
        } else {
            true
        }
    }

    fn legacy_present(legacy: &Path) -> bool {
        static SEEN: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
        let mut seen = SEEN
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *seen
            .entry(legacy.to_path_buf())
            .or_insert_with(|| legacy.is_dir())
    }

    fn config_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = xdg::config_dir() {
                return p;
            }
        }
        Self::legacy_base()
    }

    fn data_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = xdg::data_dir() {
                return p;
            }
        }
        Self::legacy_base()
    }

    fn cache_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = xdg::cache_dir() {
                return p;
            }
        }
        Self::legacy_base()
    }
}

pub mod console;
pub mod hex;
pub mod platform;
pub mod stdio;
pub mod sync;
pub mod tempfile;
#[doc(hidden)]
pub mod test_env;
mod xdg;

/// Encode a substrate topic or key for use as a single filename component.
#[must_use]
pub fn encode_file_component(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    for b in key.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Serialises tests that read or mutate process-wide env (`HOME`, `SUSI_HOME`).
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substrate_home_and_hermetic_forbidden_susi_home_follow_contract() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let forbidden = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut env = test_env::EnvGuard::isolated();
        env.set("HOME", home.path());
        env.set("SUSI_HOME", forbidden.path());
        env.set("SUSI_HERMETIC_FORBIDDEN", forbidden.path());
        env.remove("XDG_CONFIG_HOME");
        assert_eq!(SusiDirs::home_dir(), home.path());
        assert_ne!(SusiDirs::config_dir(), forbidden.path());
        env.set("SUSI_HOME", other.path());
        assert_eq!(SusiDirs::config_dir(), other.path());
        assert_eq!(SusiDirs::substrate_home(), other.path());
        assert_eq!(encode_file_component("a/b:c"), "a%2Fb%3Ac");
    }
}
