//! Vendored `susi-config` surface + IPC client for the standalone
//! `susi-config` service (`127.0.0.1:18082`, override via `SUSI_CONFIG_PORT`).
//!
//! Decoupled crates own the full config contract locally; only the *global*
//! `~/.susi/config.json` read/write path prefers the service so a running
//! substrate stays the canonical writer. When the service is unreachable —
//! or explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) is set, e.g. in
//! tests with a swapped `HOME` — every call resolves against the local files
//! exactly as the source crate did, so config access never hard-fails on
//! service health. `cluster_key` signing/verification is deliberately local
//! only: exposing HMAC over `cluster.key` (0600) as an unauthenticated
//! localhost endpoint would let any process mint signed cluster messages.
//!
//! The config modules themselves are the `susi-config` crate's own files,
//! `#[path]`-mounted below — one implementation, compiled into each crate's
//! own `susi_config` namespace. Only the IPC `service` hook is local here.

// The mounted sources compile under both editions; `collapsible_if` only
// fires under edition 2024, where `if let … && …` let-chains became stable,
// but the nested form it flags is still required by the edition-2021
// consumers. Collapsing would break those crates, so the lint is waived
// file-wide rather than at each of the shared source's sites.
#![allow(clippy::collapsible_if)]

/// IPC client for the standalone `susi-config` service. Only the global
/// config path is routed here; per-directory loads and `cluster_key` stay
/// local by construction.
mod service {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    use std::time::Duration;

    use crate::susi_config::SusiConfig;

    const DEFAULT_PORT: u16 = 18082;
    const TIMEOUT: Duration = Duration::from_millis(200);

    fn addr() -> SocketAddr {
        let port = std::env::var("SUSI_CONFIG_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// Explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) beats the
    /// service — a swapped HOME in tests must not read or write the host
    /// substrate's real `config.json`. Same rule as the vendored
    /// `susi_paths` client.
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

    /// Round-trips one HTTP/1.0 request; `None` on any transport failure.
    fn request(req: &str) -> Option<String> {
        let mut stream = TcpStream::connect_timeout(&addr(), TIMEOUT).ok()?;
        let _ = stream.set_read_timeout(Some(TIMEOUT));
        let _ = stream.set_write_timeout(Some(TIMEOUT));
        let req = crate::susi_paths::with_bearer(req);
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf).ok()?;
        Some(buf)
    }

    /// Response body when the status line is a 2xx, else `None`.
    fn body(response: &str) -> Option<&str> {
        let status_ok = response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2");
        if !status_ok {
            return None;
        }
        response.split("\r\n\r\n").nth(1)
    }

    /// `GET /config` — healed global config from the running substrate.
    pub fn get_global() -> Option<SusiConfig> {
        if local_override() {
            return None;
        }
        let resp = request("GET /config HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")?;
        serde_json::from_str(body(&resp)?).ok()
    }

    /// `POST /config` — route a global-dir save through the substrate when
    /// it is up; `false` tells the caller to fall back to the local atomic
    /// write (identical bytes, same file).
    pub fn save_global(cfg: &SusiConfig) -> bool {
        if local_override() {
            return false;
        }
        let Ok(payload) = serde_json::to_string(cfg) else {
            return false;
        };
        let req = format!(
            "POST /config HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        request(&req)
            .is_some_and(|resp| resp.starts_with("HTTP/1.1 2") || resp.starts_with("HTTP/1.0 2"))
    }
}

#[path = "../../susi-config/src/cluster_key.rs"]
pub mod cluster_key;
#[path = "../../susi-config/src/json_util.rs"]
mod json_util;
#[path = "../../susi-config/src/types.rs"]
mod types;
#[path = "../../susi-config/src/versioned_store.rs"]
pub mod versioned_store;
#[path = "../../susi-config/src/extensions.rs"]
pub mod extensions;
#[path = "../../susi-config/src/config.rs"]
mod config;

pub use config::SusiConfig;
pub use json_util::{
    atomic_write_json_pretty, confined_workspace_join, http_agent, merge_missing_json_defaults,
    merge_missing_registry_defaults, DynamicRegistry, DynamicValue, ModelTier, ProviderType,
    StringRegistry,
};
pub use types::*;
pub use versioned_store::VersionedJsonStore;
