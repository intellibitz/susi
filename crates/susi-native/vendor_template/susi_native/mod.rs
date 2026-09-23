//! Vendored `susi-native` surface + IPC client for the standalone
//! `susi-native` service (`127.0.0.1:18084`, override via `SUSI_NATIVE_PORT`).
//!
//! Wasmer/Wasix stay in the service binary only, so Wasm execution has no
//! local fallback (same posture as `susi_sandbox` docker exec): callers get a
//! typed `EaiError` when the service is unreachable.
//!
//! Keep this tree identical across the workspace.

/// IPC client for the standalone `susi-native` service.
pub(crate) mod service {
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
    use std::path::Path;
    use std::time::Duration;

    const DEFAULT_PORT: u16 = 18084;
    /// Wasm module compile + WASI run can exceed the fast IPC timeout.
    const WASM_TIMEOUT: Duration = Duration::from_secs(120);

    fn addr() -> SocketAddr {
        let port = std::env::var("SUSI_NATIVE_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(DEFAULT_PORT);
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    /// `POST /wasm/execute` — run a WASI module under Wasmer inside the
    /// substrate process. `None` on transport failure; `Some(Err)` carries
    /// the service-side error message.
    pub fn execute(wasm_path: &Path, arg: &str) -> Option<Result<String, String>> {
        let payload = serde_json::to_string(&serde_json::json!({
            "wasm_path": wasm_path.to_string_lossy(),
            "arg": arg,
        }))
        .ok()?;
        let req = format!(
            "POST /wasm/execute HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            payload.len(),
            payload
        );
        let mut stream = TcpStream::connect_timeout(&addr(), WASM_TIMEOUT).ok()?;
        let _ = stream.set_read_timeout(Some(WASM_TIMEOUT));
        let _ = stream.set_write_timeout(Some(WASM_TIMEOUT));
        stream.write_all(req.as_bytes()).ok()?;
        let mut buf = String::new();
        stream.read_to_string(&mut buf).ok()?;
        let (head, body) = buf.split_once("\r\n\r\n")?;
        if head.starts_with("HTTP/1.1 2") || head.starts_with("HTTP/1.0 2") {
            let v: serde_json::Value = serde_json::from_str(body).ok()?;
            v.get("output")
                .and_then(|o| o.as_str())
                .map(|s| Ok(s.to_string()))
        } else {
            let msg = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|v| {
                    v.get("error")
                        .and_then(|e| e.as_str())
                        .map(str::to_string)
                })
                .unwrap_or_else(|| body.to_string());
            Some(Err(msg))
        }
    }
}

pub mod wasm;

pub use wasm::WasmHost;
