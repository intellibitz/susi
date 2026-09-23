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

/// Embedded REST service mode: serves the host path/port contract over
/// HTTP. Shared by the standalone `susi-paths` binary and the root `susi`
/// binary's `service-run` dispatch — the daemon spawns the staged `susi`
/// binary in this mode, so leaf services need no sibling binaries on disk.
pub fn serve(port: u16) -> std::io::Result<()> {
    use axum::{routing::get, Json, Router};
    use serde::Serialize;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    #[derive(Serialize)]
    struct PathsResponse {
        home_dir: PathBuf,
        config_dir: PathBuf,
        data_dir: PathBuf,
        cache_dir: PathBuf,
        substrate_home: PathBuf,
    }

    #[derive(Serialize)]
    struct PortsResponse {
        gmcp: u16,
        gemi: u16,
        udp_discovery: u16,
        gmcp_http: u16,
    }

    async fn get_paths() -> Json<PathsResponse> {
        Json(PathsResponse {
            home_dir: SusiDirs::home_dir(),
            config_dir: SusiDirs::config_dir(),
            data_dir: SusiDirs::data_dir(),
            cache_dir: SusiDirs::cache_dir(),
            substrate_home: SusiDirs::substrate_home(),
        })
    }

    async fn get_ports() -> Json<PortsResponse> {
        Json(PortsResponse {
            gmcp: ports::GMCP,
            gemi: ports::GEMI,
            udp_discovery: ports::UDP_DISCOVERY,
            gmcp_http: ports::GMCP_HTTP,
        })
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let app = Router::new()
                .route("/paths", get(get_paths))
                .route("/ports", get(get_ports));
            let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
            let listener = tokio::net::TcpListener::bind(addr).await?;
            eprintln!("susi-paths service listening on {addr}");
            axum::serve(listener, app).await
        })
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
