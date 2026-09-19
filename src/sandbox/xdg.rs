use std::path::PathBuf;

pub struct SusiDirs;

impl SusiDirs {
    fn legacy_base() -> PathBuf {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".susi")
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

    pub fn config_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.config_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }

    pub fn data_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.data_local_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }

    pub fn cache_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.cache_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }
}
