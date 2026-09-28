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

//! Host path and port contract.
//!
//! [`SusiDirs`] asks the standalone `susi-paths` service (`127.0.0.1:18080`,
//! override via `SUSI_PATHS_PORT`) and falls back to the local XDG/legacy
//! resolver when the service is unreachable, so path lookup never hard-fails.
//! The service itself answers from the same local resolver.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// Host directory contract, resolved via the `susi-paths` service with a
/// local XDG/legacy fallback when it is unreachable.
pub struct SusiDirs;

const SERVICE_TIMEOUT: Duration = Duration::from_millis(200);

impl SusiDirs {
    /// Cached while the service is healthy; retries (bounded) while it is down.
    fn fetch_paths() -> HashMap<String, PathBuf> {
        static CACHE: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
        if let Some(cached) = CACHE.get() {
            return cached.clone();
        }
        let map = Self::fetch_paths_uncached();
        if !map.is_empty() {
            let _ = CACHE.set(map.clone());
        }
        map
    }

    fn fetch_paths_uncached() -> HashMap<String, PathBuf> {
        // Explicit SUSI_PATHS_PORT wins, else the default rides the instance
        // offset like every other susi port.
        let port = std::env::var("SUSI_PATHS_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or_else(|| ports::effective(18080));
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        let fetch = || -> Option<HashMap<String, PathBuf>> {
            let mut stream = TcpStream::connect_timeout(&addr, SERVICE_TIMEOUT).ok()?;
            let _ = stream.set_read_timeout(Some(SERVICE_TIMEOUT));
            let _ = stream.set_write_timeout(Some(SERVICE_TIMEOUT));
            stream
                .write_all(b"GET /paths HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
                .ok()?;
            let mut buf = String::new();
            stream.read_to_string(&mut buf).ok()?;
            let (_, body) = buf.split_once("\r\n\r\n")?;
            serde_json::from_str::<HashMap<String, PathBuf>>(body).ok()
        };
        let map = fetch().unwrap_or_default();
        // The service answers for whichever user/HOME started it. Only trust
        // it when it resolves *our* home — otherwise another user's (or
        // another HOME's) daemon would silently hand us its substrate paths.
        match map.get("home_dir") {
            Some(home) if same_path(home, &LocalDirs::home_dir()) => map,
            _ => HashMap::new(),
        }
    }

    /// Explicit local env config (`SUSI_HOME`, `SUSI_XDG`, `XDG_*_HOME`)
    /// beats the service — a second instance's root must not resolve to the
    /// primary host substrate's paths.
    fn local_override() -> bool {
        [
            "SUSI_HOME",
            "SUSI_XDG",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
        ]
        .iter()
        .any(|v| std::env::var_os(v).is_some())
    }

    fn get(key: &str) -> PathBuf {
        if Self::local_override() {
            return LocalDirs::get(key);
        }
        Self::fetch_paths()
            .remove(key)
            .unwrap_or_else(|| LocalDirs::get(key))
    }

    /// User home directory.
    #[must_use]
    pub fn home_dir() -> PathBuf {
        Self::get("home_dir")
    }
    /// Substrate config dir (`~/.susi` legacy or the platform config dir).
    #[must_use]
    pub fn config_dir() -> PathBuf {
        Self::get("config_dir")
    }
    /// Substrate data dir (`~/.susi` legacy or the platform data dir).
    #[must_use]
    pub fn data_dir() -> PathBuf {
        Self::get("data_dir")
    }
    /// Substrate cache dir (`~/.susi` legacy or the platform cache dir).
    #[must_use]
    pub fn cache_dir() -> PathBuf {
        Self::get("cache_dir")
    }
    /// Host substrate root the daemon binds to (the data dir). This is
    /// **not** a project workspace — CLI intents use the caller's cwd.
    #[must_use]
    pub fn substrate_home() -> PathBuf {
        Self::get("substrate_home")
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// Host API bearer token (`<config_dir>/api_token`), when seeded. The
/// susi-config and susi-sandbox leaf services require it on every request.
#[must_use]
pub fn host_token() -> Option<String> {
    std::fs::read_to_string(SusiDirs::config_dir().join("api_token"))
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Insert the host bearer header after the request line of a raw loopback
/// HTTP/1.0 request. Unchanged when no token is seeded.
#[must_use]
pub fn with_bearer(raw: &str) -> String {
    match (host_token(), raw.split_once("\r\n")) {
        (Some(token), Some((line, rest))) => {
            format!("{line}\r\nAuthorization: Bearer {token}\r\n{rest}")
        }
        _ => raw.to_string(),
    }
}

/// Constant-time check of an `Authorization` header value against the host
/// token. Fails closed when no token is seeded.
#[must_use]
pub fn bearer_authorized(header: Option<&str>) -> bool {
    let (Some(expected), Some(presented)) =
        (host_token(), header.and_then(|h| h.strip_prefix("Bearer ")))
    else {
        return false;
    };
    let (a, b) = (presented.trim().as_bytes(), expected.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Local XDG/legacy resolver: the rule the service answers with and the
/// fallback every client uses when the service is unreachable.
struct LocalDirs;

impl LocalDirs {
    fn get(key: &str) -> PathBuf {
        match key {
            "home_dir" => Self::home_dir(),
            "config_dir" => Self::config_dir(),
            "cache_dir" => Self::cache_dir(),
            "substrate_home" => Self::substrate_home(),
            _ => Self::data_dir(),
        }
    }

    /// `SUSI_HOME` selects a fully isolated instance root — the multi-instance
    /// knob: `SUSI_HOME=~/.susi-b SUSI_PORT_OFFSET=100 susi start` runs a
    /// second node beside the primary with its own config, lock, and state.
    fn instance_root() -> Option<PathBuf> {
        std::env::var_os("SUSI_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }

    fn legacy_base() -> PathBuf {
        Self::instance_root().unwrap_or_else(|| Self::home_dir().join(".susi"))
    }

    fn home_dir() -> PathBuf {
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
        if Self::instance_root().is_some() {
            return false;
        }
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
    fn config_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.config_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }

    #[must_use]
    fn data_dir() -> PathBuf {
        if Self::use_xdg() {
            if let Some(p) = Self::project_dirs() {
                return p.data_local_dir().to_path_buf();
            }
        }
        Self::legacy_base()
    }

    #[must_use]
    fn cache_dir() -> PathBuf {
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
    fn substrate_home() -> PathBuf {
        Self::data_dir()
    }
}

pub mod ports;

/// Percent-encode a string for a query component (RFC 3986 unreserved
/// plus the extra bytes in `keep`). Used by live search and sandbox IPC
/// so those callers do not each keep a forked encoder.
#[must_use]
pub fn percent_encode(s: &str, keep: &[u8]) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        let unreserved = matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~')
            || keep.contains(&b);
        if unreserved {
            out.push(b as char);
        } else if b == b' ' {
            out.push_str("%20");
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Query-string component encoding (spaces and commas encoded).
#[must_use]
pub fn percent_encode_query(s: &str) -> String {
    percent_encode(s, &[])
}

/// Path encoding that leaves `/` intact (sandbox IPC workspace paths).
#[must_use]
pub fn percent_encode_path(s: &str) -> String {
    percent_encode(s, &[b'/'])
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
        a2a_http: u16,
    }

    async fn get_paths() -> Json<PathsResponse> {
        Json(PathsResponse {
            home_dir: LocalDirs::home_dir(),
            config_dir: LocalDirs::config_dir(),
            data_dir: LocalDirs::data_dir(),
            cache_dir: LocalDirs::cache_dir(),
            substrate_home: LocalDirs::substrate_home(),
        })
    }

    async fn get_ports() -> Json<PortsResponse> {
        Json(PortsResponse {
            gmcp: ports::effective(ports::GMCP),
            gemi: ports::effective(ports::GEMI),
            udp_discovery: ports::effective(ports::UDP_DISCOVERY),
            gmcp_http: ports::effective(ports::GMCP_HTTP),
            a2a_http: ports::effective(ports::A2A_HTTP),
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
        assert_eq!(ports::A2A_HTTP, 9094);
        assert_eq!(ports::ALL.len(), 5);
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

    #[test]
    fn percent_encode_query_and_path() {
        assert_eq!(percent_encode_query("Chennai, India"), "Chennai%2C%20India");
        assert_eq!(percent_encode_path("/tmp/my ws"), "/tmp/my%20ws");
    }
}
