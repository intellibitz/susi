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

use std::path::{Path, PathBuf};

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn is_abs_cross_os(p: &Path) -> bool {
    let s = p.to_string_lossy();
    let bytes = s.as_bytes();
    p.is_absolute()
        || s.starts_with('/')
        || s.starts_with("\\\\")
        || (bytes.len() > 2 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn abs_path(p: Option<&Path>) -> Option<PathBuf> {
    p.filter(|p| !p.as_os_str().is_empty() && is_abs_cross_os(p))
        .map(Path::to_path_buf)
}

pub(crate) fn home_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("USERPROFILE").or_else(|| env_path("HOME"))
    } else {
        env_path("HOME").or_else(|| env_path("USERPROFILE"))
    }
}

pub(crate) fn config_dir_for(
    os: &str,
    home: Option<&Path>,
    xdg_config: Option<&Path>,
    appdata: Option<&Path>,
) -> Option<PathBuf> {
    match os {
        "windows" => {
            abs_path(appdata).map(|base| base.join("intellibitz").join("susi").join("config"))
        }
        "macos" => home
            .filter(|h| !h.as_os_str().is_empty())
            .map(|h| h.join("Library/Application Support/intellibitz.susi")),
        _ => abs_path(xdg_config)
            .or_else(|| {
                home.filter(|h| !h.as_os_str().is_empty())
                    .map(|h| h.join(".config"))
            })
            .map(|base| base.join("susi")),
    }
}

pub(crate) fn data_dir_for(
    os: &str,
    home: Option<&Path>,
    xdg_data: Option<&Path>,
    localappdata: Option<&Path>,
) -> Option<PathBuf> {
    match os {
        "windows" => {
            abs_path(localappdata).map(|base| base.join("intellibitz").join("susi").join("data"))
        }
        "macos" => home
            .filter(|h| !h.as_os_str().is_empty())
            .map(|h| h.join("Library/Application Support/intellibitz.susi")),
        _ => abs_path(xdg_data)
            .or_else(|| {
                home.filter(|h| !h.as_os_str().is_empty())
                    .map(|h| h.join(".local/share"))
            })
            .map(|base| base.join("susi")),
    }
}

pub(crate) fn cache_dir_for(
    os: &str,
    home: Option<&Path>,
    xdg_cache: Option<&Path>,
    localappdata: Option<&Path>,
) -> Option<PathBuf> {
    match os {
        "windows" => {
            abs_path(localappdata).map(|base| base.join("intellibitz").join("susi").join("cache"))
        }
        "macos" => home
            .filter(|h| !h.as_os_str().is_empty())
            .map(|h| h.join("Library/Caches/intellibitz.susi")),
        _ => abs_path(xdg_cache)
            .or_else(|| {
                home.filter(|h| !h.as_os_str().is_empty())
                    .map(|h| h.join(".cache"))
            })
            .map(|base| base.join("susi")),
    }
}

pub(crate) fn config_dir() -> Option<PathBuf> {
    let home = home_dir();
    let xdg_config = env_path("XDG_CONFIG_HOME");
    let appdata = env_path("APPDATA");
    config_dir_for(
        std::env::consts::OS,
        home.as_deref(),
        xdg_config.as_deref(),
        appdata.as_deref(),
    )
}

pub(crate) fn data_dir() -> Option<PathBuf> {
    let home = home_dir();
    let xdg_data = env_path("XDG_DATA_HOME");
    let localappdata = env_path("LOCALAPPDATA");
    data_dir_for(
        std::env::consts::OS,
        home.as_deref(),
        xdg_data.as_deref(),
        localappdata.as_deref(),
    )
}

pub(crate) fn cache_dir() -> Option<PathBuf> {
    let home = home_dir();
    let xdg_cache = env_path("XDG_CACHE_HOME");
    let localappdata = env_path("LOCALAPPDATA");
    cache_dir_for(
        std::env::consts::OS,
        home.as_deref(),
        xdg_cache.as_deref(),
        localappdata.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_layout_matches_the_directories_crate() {
        // Resolving a layout reads HOME, SUSI_HOME and the XDG vars, and
        // another test in this binary mutates them under `ENV_LOCK`. Without
        // the lock this test can compare a path resolved before that mutation
        // against one resolved after it — which is how it failed twice in a
        // row under the loaded workspace suite while passing on its own.
        let _env = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    #[test]
    fn cross_os_layouts_match_linux_macos_and_windows_contracts() {
        let home = Path::new("/home/alice");

        // 1. Linux fallback when XDG_*_HOME is None or relative
        assert_eq!(
            config_dir_for("linux", Some(home), None, None),
            Some(home.join(".config").join("susi"))
        );
        assert_eq!(
            config_dir_for("linux", Some(home), Some(Path::new("rel/xdg")), None),
            Some(home.join(".config").join("susi"))
        );
        assert_eq!(
            data_dir_for("linux", Some(home), None, None),
            Some(home.join(".local/share").join("susi"))
        );
        assert_eq!(
            cache_dir_for("linux", Some(home), None, None),
            Some(home.join(".cache").join("susi"))
        );

        // 2. Linux honors absolute XDG_*_HOME
        let xdg_cfg = Path::new("/tmp/xdg-cfg");
        let xdg_data = Path::new("/tmp/xdg-data");
        let xdg_cache = Path::new("/tmp/xdg-cache");
        assert_eq!(
            config_dir_for("linux", Some(home), Some(xdg_cfg), None),
            Some(xdg_cfg.join("susi"))
        );
        assert_eq!(
            data_dir_for("linux", Some(home), Some(xdg_data), None),
            Some(xdg_data.join("susi"))
        );
        assert_eq!(
            cache_dir_for("linux", Some(home), Some(xdg_cache), None),
            Some(xdg_cache.join("susi"))
        );

        // 3. macOS layout
        let mac_home = Path::new("/Users/alice");
        assert_eq!(
            config_dir_for("macos", Some(mac_home), None, None),
            Some(mac_home.join("Library/Application Support/intellibitz.susi"))
        );
        assert_eq!(
            data_dir_for("macos", Some(mac_home), None, None),
            Some(mac_home.join("Library/Application Support/intellibitz.susi"))
        );
        assert_eq!(
            cache_dir_for("macos", Some(mac_home), None, None),
            Some(mac_home.join("Library/Caches/intellibitz.susi"))
        );

        // 4. Windows layout (APPDATA / LOCALAPPDATA)
        let appdata = Path::new(r"C:\Users\alice\AppData\Roaming");
        let localappdata = Path::new(r"C:\Users\alice\AppData\Local");
        assert_eq!(
            config_dir_for("windows", None, None, Some(appdata)),
            Some(appdata.join("intellibitz").join("susi").join("config"))
        );
        assert_eq!(
            data_dir_for("windows", None, None, Some(localappdata)),
            Some(localappdata.join("intellibitz").join("susi").join("data"))
        );
        assert_eq!(
            cache_dir_for("windows", None, None, Some(localappdata)),
            Some(localappdata.join("intellibitz").join("susi").join("cache"))
        );
        // Relative APPDATA is ignored
        assert_eq!(
            config_dir_for("windows", None, None, Some(Path::new("relative"))),
            None
        );
    }

    #[test]
    fn abs_path_and_empty_or_missing_base_dirs_behave_deterministically() {
        for valid in [
            "/abs/unix/path",
            r"\\server\share\folder",
            r"C:\Windows\System32",
            "D:/data/models",
        ] {
            let p = Path::new(valid);
            assert!(is_abs_cross_os(p), "expected {valid} to be cross-OS abs");
            assert_eq!(abs_path(Some(p)), Some(p.to_path_buf()));
        }

        for invalid in ["", "   ", "relative/path", r"relative\win"] {
            let p = Path::new(invalid);
            assert_eq!(abs_path(Some(p)), None, "expected {invalid:?} to be None");
        }
        assert_eq!(abs_path(None), None);

        let empty = Path::new("");
        for os in ["linux", "macos", "windows"] {
            assert_eq!(config_dir_for(os, None, None, None), None);
            assert_eq!(data_dir_for(os, None, None, None), None);
            assert_eq!(cache_dir_for(os, None, None, None), None);
            assert_eq!(
                config_dir_for(os, Some(empty), Some(empty), Some(empty)),
                None
            );
            assert_eq!(
                data_dir_for(os, Some(empty), Some(empty), Some(empty)),
                None
            );
            assert_eq!(
                cache_dir_for(os, Some(empty), Some(empty), Some(empty)),
                None
            );
        }
    }

    #[test]
    fn macos_ignores_xdg_and_appdata_and_windows_ignores_home_and_xdg_fallbacks() {
        let mac_home = Path::new("/Users/alice");
        let xdg_cfg = Path::new("/tmp/xdg-cfg");
        let xdg_data = Path::new("/tmp/xdg-data");
        let xdg_cache = Path::new("/tmp/xdg-cache");
        let appdata = Path::new(r"C:\Users\alice\AppData\Roaming");

        assert_eq!(
            config_dir_for("macos", Some(mac_home), Some(xdg_cfg), Some(appdata)),
            Some(mac_home.join("Library/Application Support/intellibitz.susi"))
        );
        assert_eq!(
            data_dir_for("macos", Some(mac_home), Some(xdg_data), Some(appdata)),
            Some(mac_home.join("Library/Application Support/intellibitz.susi"))
        );
        assert_eq!(
            cache_dir_for("macos", Some(mac_home), Some(xdg_cache), Some(appdata)),
            Some(mac_home.join("Library/Caches/intellibitz.susi"))
        );

        let home = Path::new("/home/alice");
        let rel = Path::new("relative/appdata");
        for win_appdata in [None, Some(rel)] {
            assert_eq!(
                config_dir_for("windows", Some(home), Some(xdg_cfg), win_appdata),
                None
            );
            assert_eq!(
                data_dir_for("windows", Some(home), Some(xdg_data), win_appdata),
                None
            );
            assert_eq!(
                cache_dir_for("windows", Some(home), Some(xdg_cache), win_appdata),
                None
            );
        }
    }

    #[test]
    fn unknown_os_falls_back_to_xdg_or_home_dot_dirs_like_linux() {
        let home = Path::new("/home/bob");
        assert_eq!(
            config_dir_for("freebsd", Some(home), None, None),
            Some(home.join(".config").join("susi"))
        );
        assert_eq!(
            data_dir_for("freebsd", Some(home), None, None),
            Some(home.join(".local/share").join("susi"))
        );
        assert_eq!(
            cache_dir_for("freebsd", Some(home), None, None),
            Some(home.join(".cache").join("susi"))
        );
    }
}
