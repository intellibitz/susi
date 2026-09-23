//! Vendored `susi-sandbox` surface + IPC client for the standalone
//! `susi-sandbox` service (`127.0.0.1:18083`, override via `SUSI_SANDBOX_PORT`).
//!
//! Decoupled crates own the sandbox contract locally; operations that should
//! run in the substrate process (`ensure_global`, Docker exec, daemon status /
//! integrity hashing) prefer the service so bollard and host daemon state stay
//! isolated. When the service is unreachable — or explicit local env config
//! (`SUSI_XDG`, `XDG_*_HOME`) is set — most calls fall back to local filesystem
//! helpers. `execute_in_docker` has no local fallback (requires the service).
//! Audit HMAC key ops stay local only: exposing them as an unauthenticated
//! localhost oracle would let any process mint signed audit entries.
//!
//! Keep this tree identical across the workspace.

// This tree is vendored byte-identical into crates on different editions;
// `collapsible_if` only fires under edition 2024, where `if let … && …`
// let-chains became stable, but the pre-2021-compatible nested form it
// flags is still required by the edition-2021 consumers. Collapsing here
// would break those crates, so the lint is waived module-wide rather than
// at each of the shared source's sites.
#![allow(clippy::collapsible_if)]

/// IPC client for the standalone `susi-sandbox` service.
pub(crate) mod service {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    use std::path::Path;
    use std::time::Duration;

    const DEFAULT_PORT: u16 = 18083;
    const TIMEOUT: Duration = Duration::from_millis(200);
    /// Docker container create/start/logs can exceed the fast IPC timeout.
    const DOCKER_TIMEOUT: Duration = Duration::from_secs(120);

    fn addr() -> SocketAddr {
        let port = std::env::var("SUSI_SANDBOX_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// Explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) beats the
    /// service — a swapped HOME in tests must not read or write the host
    /// substrate's real sandbox state. Same rule as the vendored
    /// `susi_paths` / `susi_config` clients.
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
    fn request_with_timeout(req: &str, timeout: Duration) -> Option<String> {
        let mut stream = TcpStream::connect_timeout(&addr(), timeout).ok()?;
        let _ = stream.set_read_timeout(Some(timeout));
        let _ = stream.set_write_timeout(Some(timeout));
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf).ok()?;
        Some(buf)
    }

    fn request(req: &str) -> Option<String> {
        request_with_timeout(req, TIMEOUT)
    }

    /// Response body when the status line is a 2xx, else `None`.
    fn body(response: &str) -> Option<&str> {
        let status_ok = response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2");
        if !status_ok {
            return None;
        }
        response.split("\r\n\r\n").nth(1)
    }

    fn json_post(path: &str, payload: &str) -> Option<String> {
        let req = format!(
            "POST {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        request(&req)
    }

    /// `POST /sandbox/ensure_global` — create global sandbox layout via substrate.
    pub fn ensure_global(global_dir: &Path) -> bool {
        if local_override() {
            return false;
        }
        let Ok(payload) = serde_json::to_string(&serde_json::json!({
            "global_dir": global_dir.to_string_lossy(),
        })) else {
            return false;
        };
        json_post("/sandbox/ensure_global", &payload)
            .is_some_and(|resp| resp.starts_with("HTTP/1.1 2") || resp.starts_with("HTTP/1.0 2"))
    }

    /// `POST /sandbox/docker_exec` — run `cmd` inside the hardened sandbox image.
    pub fn docker_exec(cmd: &str) -> Option<String> {
        if local_override() {
            return None;
        }
        let Ok(payload) = serde_json::to_string(&serde_json::json!({ "cmd": cmd })) else {
            return None;
        };
        let req = format!(
            "POST /sandbox/docker_exec HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let resp = request_with_timeout(&req, DOCKER_TIMEOUT)?;
        serde_json::from_str(body(&resp)?).ok()
    }

    /// `GET /daemon/status?workspace=&global_dir=`
    pub fn check_status(workspace: &Path, global_dir: &Path) -> Option<bool> {
        if local_override() {
            return None;
        }
        let ws = urlencoding_encode(&workspace.to_string_lossy());
        let gd = urlencoding_encode(&global_dir.to_string_lossy());
        let path = format!("/daemon/status?workspace={ws}&global_dir={gd}");
        let resp = request(&format!("GET {path} HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n"))?;
        serde_json::from_str(body(&resp)?).ok()
    }

    /// `POST /daemon/verify_integrity`
    pub fn verify_integrity(bin_path: &Path, global_dir: &Path) -> Option<bool> {
        if local_override() {
            return None;
        }
        let Ok(payload) = serde_json::to_string(&serde_json::json!({
            "bin_path": bin_path.to_string_lossy(),
            "global_dir": global_dir.to_string_lossy(),
        })) else {
            return None;
        };
        let resp = json_post("/daemon/verify_integrity", &payload)?;
        serde_json::from_str(body(&resp)?).ok()
    }

    /// `POST /daemon/hash_cached`
    pub fn hash_cached(bin_path: &Path, global_dir: &Path) -> Option<String> {
        if local_override() {
            return None;
        }
        let Ok(payload) = serde_json::to_string(&serde_json::json!({
            "bin_path": bin_path.to_string_lossy(),
            "global_dir": global_dir.to_string_lossy(),
        })) else {
            return None;
        };
        let resp = json_post("/daemon/hash_cached", &payload)?;
        serde_json::from_str(body(&resp)?).ok()
    }

    /// Minimal query-string encode (paths may contain spaces / non-ASCII).
    fn urlencoding_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    out.push(b as char);
                }
                _ => out.push_str(&format!("%{b:02X}")),
            }
        }
        out
    }
}

pub mod audit_chain;
pub mod auto_install;
pub mod daemon_state;
pub mod manager;

pub use crate::susi_config::extensions;
pub use crate::susi_config::versioned_store;
pub use crate::susi_config::VersionedJsonStore;
pub use manager::SandboxManager;
