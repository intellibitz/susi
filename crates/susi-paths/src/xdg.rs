//! Per-OS user directories for the `intellibitz/susi` project, std-only.
//!
//! Reproduces `directories::ProjectDirs::from("", "intellibitz", "susi")`
//! exactly (config / data-local / cache), so existing installs keep their
//! paths, without the crate: `susi-paths` is a foundation crate and carries
//! no third-party code for what is a few environment lookups.
//!
//! | OS      | config                                   | data (local)                           | cache                               |
//! |---------|------------------------------------------|----------------------------------------|-------------------------------------|
//! | Linux   | `$XDG_CONFIG_HOME/susi` (`~/.config`)    | `$XDG_DATA_HOME/susi` (`~/.local/share`) | `$XDG_CACHE_HOME/susi` (`~/.cache`) |
//! | macOS   | `~/Library/Application Support/intellibitz.susi` | same as config                 | `~/Library/Caches/intellibitz.susi` |
//! | Windows | `%APPDATA%\intellibitz\susi\config`      | `%LOCALAPPDATA%\intellibitz\susi\data` | `%LOCALAPPDATA%\intellibitz\susi\cache` |
//!
//! Relative `XDG_*` values are ignored (XDG spec; `directories` does the
//! same). Home is `$HOME` when non-empty (`%USERPROFILE%` on Windows); the
//! crate's `getpwuid` fallback needs `unsafe`, which this crate forbids.

use std::path::PathBuf;

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

// macOS layouts are home-relative only; the XDG/Windows readers use this.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn absolute_env(key: &str) -> Option<PathBuf> {
    env_path(key).filter(|p| p.is_absolute())
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("USERPROFILE").or_else(|| env_path("HOME"))
    } else {
        env_path("HOME").or_else(|| env_path("USERPROFILE"))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn xdg(key: &str, fallback: &str) -> Option<PathBuf> {
    absolute_env(key)
        .or_else(|| home_dir().map(|h| h.join(fallback)))
        .map(|base| base.join("susi"))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn config_dir() -> Option<PathBuf> {
    xdg("XDG_CONFIG_HOME", ".config")
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn data_dir() -> Option<PathBuf> {
    xdg("XDG_DATA_HOME", ".local/share")
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn cache_dir() -> Option<PathBuf> {
    xdg("XDG_CACHE_HOME", ".cache")
}

#[cfg(target_os = "macos")]
pub(crate) fn config_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join("Library/Application Support/intellibitz.susi"))
}

#[cfg(target_os = "macos")]
pub(crate) fn data_dir() -> Option<PathBuf> {
    config_dir()
}

#[cfg(target_os = "macos")]
pub(crate) fn cache_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join("Library/Caches/intellibitz.susi"))
}

#[cfg(windows)]
fn app_data(roaming: bool, leaf: &str) -> Option<PathBuf> {
    let key = if roaming { "APPDATA" } else { "LOCALAPPDATA" };
    absolute_env(key).map(|base| base.join("intellibitz").join("susi").join(leaf))
}

#[cfg(windows)]
pub(crate) fn config_dir() -> Option<PathBuf> {
    app_data(true, "config")
}

#[cfg(windows)]
pub(crate) fn data_dir() -> Option<PathBuf> {
    app_data(false, "data")
}

#[cfg(windows)]
pub(crate) fn cache_dir() -> Option<PathBuf> {
    app_data(false, "cache")
}

#[cfg(test)]
mod tests {
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_layout_matches_the_directories_crate() {
        let home = super::home_dir().unwrap();
        if std::env::var_os("XDG_CONFIG_HOME").is_none() {
            assert_eq!(super::config_dir().unwrap(), home.join(".config/susi"));
        }
        if std::env::var_os("XDG_DATA_HOME").is_none() {
            assert_eq!(super::data_dir().unwrap(), home.join(".local/share/susi"));
        }
        if std::env::var_os("XDG_CACHE_HOME").is_none() {
            assert_eq!(super::cache_dir().unwrap(), home.join(".cache/susi"));
        }
    }
}
