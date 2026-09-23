//! IPC client for the standalone `susi-paths` service (`127.0.0.1:18080`,
//! override via `SUSI_PATHS_PORT`). Falls back to local XDG/legacy resolution
//! when the service is unreachable so path lookup never hard-fails.
//!
//! Vendored per crate so no `susi-*` source dependency is required — keep this
//! file identical across the workspace.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// Host directory contract, resolved via the `susi-paths` service with a
/// local XDG/legacy fallback when it is unreachable.
pub struct SusiDirs;

/// Host-contract ports (kept in sync with `susi-paths`' `ports` module).
pub mod ports {
    pub const GMCP: u16 = 9090;
    pub const GEMI: u16 = 9091;
    pub const UDP_DISCOVERY: u16 = 9092;
    pub const GMCP_HTTP: u16 = 9093;
}

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
        let port = std::env::var("SUSI_PATHS_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(18080);
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
        fetch().unwrap_or_default()
    }

    /// Mirrors `susi-paths`' XDG/legacy rule: `~/.susi` wins when it already
    /// exists unless `SUSI_XDG=1|true`; otherwise the platform dirs apply.
    fn fallback(key: &str) -> PathBuf {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        if key == "home_dir" {
            return home;
        }
        let legacy = home.join(".susi");
        let use_xdg = if legacy.is_dir() {
            std::env::var("SUSI_XDG")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false)
        } else {
            true
        };
        if use_xdg {
            let (var, default) = match key {
                "config_dir" => ("XDG_CONFIG_HOME", ".config"),
                "cache_dir" => ("XDG_CACHE_HOME", ".cache"),
                _ => ("XDG_DATA_HOME", ".local/share"),
            };
            let base = std::env::var_os(var)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(default));
            return base.join("susi");
        }
        legacy
    }

    /// Explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) beats the service —
    /// a swapped HOME in tests must not leak the host substrate's paths.
    fn local_override() -> bool {
        [
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
            return Self::fallback(key);
        }
        Self::fetch_paths()
            .remove(key)
            .unwrap_or_else(|| Self::fallback(key))
    }

    /// User home directory.
    pub fn home_dir() -> PathBuf {
        Self::get("home_dir")
    }
    /// Substrate config dir (`~/.susi` legacy or `$XDG_CONFIG_HOME/susi`).
    pub fn config_dir() -> PathBuf {
        Self::get("config_dir")
    }
    /// Substrate data dir (`~/.susi` legacy or `$XDG_DATA_HOME/susi`).
    pub fn data_dir() -> PathBuf {
        Self::get("data_dir")
    }
    /// Substrate cache dir (`~/.susi` legacy or `$XDG_CACHE_HOME/susi`).
    pub fn cache_dir() -> PathBuf {
        Self::get("cache_dir")
    }
    /// Host substrate root the daemon binds to (the data dir).
    pub fn substrate_home() -> PathBuf {
        Self::get("substrate_home")
    }
}
