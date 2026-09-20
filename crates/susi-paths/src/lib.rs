use std::path::PathBuf;

pub struct SusiDirs;

impl SusiDirs {
    fn legacy_base() -> PathBuf {
        Self::home_dir().join(".susi")
    }

    pub fn home_dir() -> PathBuf {
        directories::BaseDirs::new()
            .map(|d| d.home_dir().to_path_buf())
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("."))
            })
    }

    fn use_xdg() -> bool {
        let legacy = Self::legacy_base();
        if legacy.is_dir() {
            std::env::var("SUSI_XDG")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false)
        } else {
            true
        }
    }

    fn project_dirs() -> Option<directories::ProjectDirs> {
        directories::ProjectDirs::from("", "intellibitz", "susi")
    }

    #[must_use]
    pub fn config_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.config_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }

    #[must_use]
    pub fn data_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.data_local_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }

    #[must_use]
    pub fn cache_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.cache_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }

    /// Host substrate root the background daemon is always bound to
    /// (`~/.susi` or the XDG data dir). This is **not** a project workspace —
    /// CLI intents use the caller's cwd; the daemon only owns host-global
    /// state (models, ports, lock, rediscovery).
    #[must_use]
    pub fn substrate_home() -> PathBuf {
        Self::data_dir()
    }
}
