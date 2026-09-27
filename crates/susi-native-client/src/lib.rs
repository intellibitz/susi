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

//! Typed IPC client for the first-party `susi-native` Wasmer service.
//!
//! Keeping the transport in its own crate gives every caller one compiled
//! implementation without linking Wasmer into feature planes.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;
use susi_error::{EaiError, EaiResult};

const DEFAULT_PORT: u16 = 18084;
const WASM_TIMEOUT: Duration = Duration::from_secs(120);

/// Client facade for reflex execution in the standalone native service.
pub struct WasmHost;

/// Compatibility namespace for callers that address the host as
/// `susi_native::wasm::WasmHost`.
pub mod wasm {
    pub use super::WasmHost;
}

impl WasmHost {
    /// Run untrusted Wasm under the native service's Wasmer/WASI isolation.
    pub fn execute_untrusted_wasm(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        Self::execute_reflex(wasm_path, arg)
    }

    /// Execute a distilled reflex without linking the Wasm runtime locally.
    pub fn execute_reflex(wasm_path: &Path, arg: &str) -> EaiResult<String> {
        match execute(wasm_path, arg) {
            Some(Ok(output)) => Ok(output),
            Some(Err(message)) => Err(EaiError::process(format!(
                "Wasm execution failed: {message}"
            ))),
            None => Err(EaiError::process(
                "susi-native service unreachable on 127.0.0.1:18084 \
                 (SUSI_NATIVE_PORT); start the service to run Wasm reflexes",
            )),
        }
    }
}

fn addr() -> SocketAddr {
    let port = std::env::var("SUSI_NATIVE_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
}

fn with_bearer(request: &str) -> String {
    let token = std::fs::read_to_string(susi_paths::SusiDirs::config_dir().join("api_token"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    match (token, request.split_once("\r\n")) {
        (Some(token), Some((line, rest))) => {
            format!("{line}\r\nAuthorization: Bearer {token}\r\n{rest}")
        }
        _ => request.to_string(),
    }
}

fn error_message(head: &str, message: String) -> String {
    if !message.is_empty() {
        return message;
    }
    let status = head.lines().next().unwrap_or(head).trim();
    if status.contains(" 401") {
        format!("{status} (bearer token rejected; check SUSI_HOST_TOKEN / api_token)")
    } else {
        status.to_string()
    }
}

fn execute(wasm_path: &Path, arg: &str) -> Option<Result<String, String>> {
    let payload = serde_json::to_string(&serde_json::json!({
        "wasm_path": wasm_path.to_string_lossy(),
        "arg": arg,
    }))
    .ok()?;
    let request = format!(
        "POST /wasm/execute HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        payload.len(),
        payload
    );
    let mut stream = TcpStream::connect_timeout(&addr(), WASM_TIMEOUT).ok()?;
    let _ = stream.set_read_timeout(Some(WASM_TIMEOUT));
    let _ = stream.set_write_timeout(Some(WASM_TIMEOUT));
    stream.write_all(with_bearer(&request).as_bytes()).ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    let (head, body) = response.split_once("\r\n\r\n")?;
    if head.starts_with("HTTP/1.1 2") || head.starts_with("HTTP/1.0 2") {
        let value: serde_json::Value = serde_json::from_str(body).ok()?;
        value
            .get("output")
            .and_then(|output| output.as_str())
            .map(|output| Ok(output.to_string()))
    } else {
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| body.trim().to_string());
        Some(Err(error_message(head, message)))
    }
}

#[cfg(test)]
mod tests {
    use super::error_message;

    #[test]
    fn empty_rejection_names_the_http_status() {
        let head = "HTTP/1.1 401 Unauthorized\r\ncontent-length: 0";
        let message = error_message(head, String::new());
        assert!(
            message.starts_with("HTTP/1.1 401 Unauthorized"),
            "{message}"
        );
        assert!(message.contains("bearer"), "{message}");
        assert_eq!(
            error_message("HTTP/1.1 500 Internal Server Error", String::new()),
            "HTTP/1.1 500 Internal Server Error"
        );
        assert_eq!(error_message(head, "boom".into()), "boom");
    }
}
