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
    use susi_paths::loopback::{self, Auth};
    let payload = serde_json::to_string(&serde_json::json!({
        "wasm_path": wasm_path.to_string_lossy(),
        "arg": arg,
    }))
    .ok()?;
    let ep = loopback::Endpoint {
        port: loopback::service_port("SUSI_NATIVE_PORT", DEFAULT_PORT),
        timeout: WASM_TIMEOUT,
        auth: Auth::HostToken,
    };
    let resp = loopback::request(&ep, "POST", "/wasm/execute", Some(&payload))?;
    if resp.is_success() {
        let value: serde_json::Value = serde_json::from_str(&resp.body).ok()?;
        value
            .get("output")
            .and_then(|output| output.as_str())
            .map(|output| Ok(output.to_string()))
    } else {
        let message = serde_json::from_str::<serde_json::Value>(&resp.body)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_else(|| resp.body.trim().to_string());
        Some(Err(error_message(
            &format!("HTTP {}", resp.status),
            message,
        )))
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
