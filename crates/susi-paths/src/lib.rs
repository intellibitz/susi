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

/// Canonical public substrate ports. External clients may hard-code these;
/// the daemon must never silently drift to ephemeral ports.
pub mod ports {
    pub const GMCP: u16 = 9090;
    pub const GEMI: u16 = 9091;
    pub const UDP_DISCOVERY: u16 = 9092;
    pub const GMCP_HTTP: u16 = 9093;

    /// Stable host contract advertised to external clients.
    pub const ALL: [(u16, &str); 4] = [
        (GMCP, "GMCP/MCP HTTP"),
        (GEMI, "GEMI HTTP"),
        (UDP_DISCOVERY, "A2A UDP discovery"),
        (GMCP_HTTP, "GMCP HTTP alias"),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_contract_ports_are_stable() {
        assert_eq!(ports::GMCP, 9090);
        assert_eq!(ports::GEMI, 9091);
        assert_eq!(ports::UDP_DISCOVERY, 9092);
        assert_eq!(ports::GMCP_HTTP, 9093);
        assert_eq!(ports::ALL.len(), 4);
        let mut seen = std::collections::BTreeSet::new();
        for (port, _) in ports::ALL {
            assert!(seen.insert(port), "duplicate host-contract port {port}");
        }
    }

    #[test]
    fn substrate_home_is_not_a_project_cwd() {
        let home = SusiDirs::substrate_home();
        assert!(
            home.ends_with(".susi") || home.to_string_lossy().contains("susi"),
            "substrate_home should be the host substrate root, got {}",
            home.display()
        );
        // Distinct from a typical project folder under github.com/...
        assert!(
            !home.to_string_lossy().contains("/github.com/"),
            "{}",
            home.display()
        );
    }
}
