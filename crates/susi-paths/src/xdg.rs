//! Linux XDG user directories for Susi (`$XDG_*_HOME/susi` with `$HOME` dot-dir fallbacks).

use std::path::{Path, PathBuf};

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    env_path("HOME")
}

fn resolve_linux(home: Option<&Path>, xdg: Option<&Path>, fallback: &str) -> Option<PathBuf> {
    xdg.filter(|p| !p.as_os_str().is_empty() && p.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| {
            home.filter(|p| !p.as_os_str().is_empty())
                .map(|base| base.join(fallback))
        })
        .map(|base| base.join("susi"))
}

pub(crate) fn config_dir() -> Option<PathBuf> {
    resolve_linux(
        home_dir().as_deref(),
        env_path("XDG_CONFIG_HOME").as_deref(),
        ".config",
    )
}

pub(crate) fn data_dir() -> Option<PathBuf> {
    resolve_linux(
        home_dir().as_deref(),
        env_path("XDG_DATA_HOME").as_deref(),
        ".local/share",
    )
}

pub(crate) fn cache_dir() -> Option<PathBuf> {
    resolve_linux(
        home_dir().as_deref(),
        env_path("XDG_CACHE_HOME").as_deref(),
        ".cache",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_layout_matches_xdg_defaults_and_ignores_relative_overrides() {
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let home = Path::new("/home/alice");
        assert_eq!(
            resolve_linux(Some(home), None, ".config"),
            Some(home.join(".config/susi"))
        );
        assert_eq!(
            resolve_linux(Some(home), Some(Path::new("rel/xdg")), ".config"),
            Some(home.join(".config/susi"))
        );
        assert_eq!(
            resolve_linux(Some(home), Some(Path::new("/tmp/xdg-cfg")), ".config"),
            Some(PathBuf::from("/tmp/xdg-cfg/susi"))
        );
        assert_eq!(resolve_linux(None, None, ".config"), None);
    }
}
