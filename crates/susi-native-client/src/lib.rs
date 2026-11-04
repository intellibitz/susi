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

const DEFAULT_PORT: u16 = susi_paths::ports::NATIVE_SERVICE;
const WASM_TIMEOUT: Duration = Duration::from_secs(120);

/// Client facade for reflex execution in the standalone native service.
pub struct WasmHost;

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
            None => Err(EaiError::process(format!(
                "susi-native service unreachable on 127.0.0.1:{} \
                 (SUSI_NATIVE_PORT); start the service to run Wasm reflexes",
                susi_paths::loopback::service_port("SUSI_NATIVE_PORT", DEFAULT_PORT)
            ))),
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
    use super::{error_message, WasmHost};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;

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

    fn read_http_request(stream: &mut TcpStream) -> (String, String) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            let n = stream.read(&mut chunk).expect("read request header bytes");
            assert!(n > 0, "unexpected EOF reading HTTP request headers");
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_len = head
            .lines()
            .find_map(|line| {
                let (k, v) = line.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        let body_start = header_end + 4;
        while buf.len() < body_start + content_len {
            let n = stream.read(&mut chunk).expect("read request body bytes");
            assert!(n > 0, "unexpected EOF reading HTTP request body");
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buf[body_start..body_start + content_len]).to_string();
        (head, body)
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn wasm_host_executes_reflex_over_loopback_and_surfaces_trap_errors() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_home =
            std::env::temp_dir().join(format!("susi_native_client_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).expect("create temp SUSI_HOME");
        std::fs::write(temp_home.join("api_token"), "native-secret-token\n")
            .expect("seed api_token");
        std::fs::write(
            temp_home.join("daemon.lock"),
            format!("{}\n", std::process::id()),
        )
        .expect("seed daemon.lock");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("listener addr").port();

        let prev_susi_home = std::env::var_os("SUSI_HOME");
        let prev_native_port = std::env::var_os("SUSI_NATIVE_PORT");
        std::env::set_var("SUSI_HOME", &temp_home);
        std::env::set_var("SUSI_NATIVE_PORT", port.to_string());

        let server = std::thread::spawn(move || {
            // 1st request: 200 OK with {"output": "reflex-out"}
            let (mut s1, _) = listener.accept().expect("accept 1st request");
            let (head1, body1) = read_http_request(&mut s1);
            assert!(head1.starts_with("POST /wasm/execute HTTP/1.0"), "{head1}");
            assert!(
                head1.contains("Authorization: Bearer native-secret-token"),
                "{head1}"
            );
            let parsed1: serde_json::Value = serde_json::from_str(&body1).expect("parse json 1");
            assert_eq!(parsed1["arg"], "input-payload");
            assert!(parsed1["wasm_path"]
                .as_str()
                .unwrap_or("")
                .ends_with("reflex_fast.wasm"));
            let resp1_body = r#"{"output":"reflex-out"}"#;
            write!(
                s1,
                "HTTP/1.0 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                resp1_body.len(),
                resp1_body
            )
            .expect("write response 1");
            s1.flush().expect("flush response 1");
            drop(s1);

            // 2nd request: 500 Internal Server Error with {"error": "trap: unreachable"}
            let (mut s2, _) = listener.accept().expect("accept 2nd request");
            let (_head2, _body2) = read_http_request(&mut s2);
            let resp2_body = r#"{"error":"trap: unreachable"}"#;
            write!(
                s2,
                "HTTP/1.0 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                resp2_body.len(),
                resp2_body
            )
            .expect("write response 2");
            s2.flush().expect("flush response 2");
            drop(s2);
        });

        let out = WasmHost::execute_untrusted_wasm(Path::new("reflex_fast.wasm"), "input-payload")
            .expect("200 OK reflex execution");
        assert_eq!(out, "reflex-out");

        let err = WasmHost::execute_reflex(Path::new("reflex_trap.wasm"), "bad-input")
            .expect_err("500 trap error should surface as Err");
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("Wasm execution failed: trap: unreachable"),
            "unexpected error message: {err_msg}"
        );

        server.join().expect("server thread completed cleanly");
        match prev_susi_home {
            Some(v) => std::env::set_var("SUSI_HOME", v),
            None => std::env::remove_var("SUSI_HOME"),
        }
        match prev_native_port {
            Some(v) => std::env::set_var("SUSI_NATIVE_PORT", v),
            None => std::env::remove_var("SUSI_NATIVE_PORT"),
        }
        let _ = std::fs::remove_dir_all(&temp_home);
    }

    #[test]
    fn wasm_host_reports_unreachable_closed_port_and_401_or_502_http_statuses() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let temp_home = std::env::temp_dir().join(format!(
            "susi_native_client_err_test_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).expect("create temp SUSI_HOME");
        std::fs::write(temp_home.join("api_token"), "native-secret-token\n")
            .expect("seed api_token");
        std::fs::write(
            temp_home.join("daemon.lock"),
            format!("{}\n", std::process::id()),
        )
        .expect("seed daemon.lock");

        let prev_susi_home = std::env::var_os("SUSI_HOME");
        let prev_native_port = std::env::var_os("SUSI_NATIVE_PORT");
        std::env::set_var("SUSI_HOME", &temp_home);

        // 1. Bind and immediately drop a listener to obtain a closed loopback port.
        let closed_port = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            l.local_addr().expect("ephemeral addr").port()
        };
        std::env::set_var("SUSI_NATIVE_PORT", closed_port.to_string());

        let unreachable_err = WasmHost::execute_reflex(Path::new("missing.wasm"), "arg")
            .expect_err("closed port must fail");
        let unreachable_msg = unreachable_err.to_string();
        assert!(
            unreachable_msg.contains("susi-native service unreachable"),
            "unexpected unreachable error: {unreachable_msg}"
        );
        assert!(
            unreachable_msg.contains(&closed_port.to_string()),
            "expected port {closed_port} in error: {unreachable_msg}"
        );

        // 2. Spin up a listener returning HTTP 401 with empty body, then HTTP 502 with plain-text body.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("listener addr").port();
        std::env::set_var("SUSI_NATIVE_PORT", port.to_string());

        let server = std::thread::spawn(move || {
            let (mut s1, _) = listener.accept().expect("accept 401 request");
            let _ = read_http_request(&mut s1);
            write!(s1, "HTTP/1.0 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .expect("write 401 response");
            s1.flush().expect("flush 401");
            drop(s1);

            let (mut s2, _) = listener.accept().expect("accept 502 request");
            let _ = read_http_request(&mut s2);
            let body2 = "bad gateway upstream worker crashed";
            write!(
                s2,
                "HTTP/1.0 502 Bad Gateway\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                body2.len(),
                body2
            )
            .expect("write 502 response");
            s2.flush().expect("flush 502");
            drop(s2);
        });

        let err_401 = WasmHost::execute_reflex(Path::new("reflex.wasm"), "arg")
            .expect_err("401 should return Err");
        let msg_401 = err_401.to_string();
        assert!(
            msg_401.contains("bearer token rejected"),
            "expected bearer token hint on 401, got: {msg_401}"
        );

        let err_502 = WasmHost::execute_reflex(Path::new("reflex.wasm"), "arg")
            .expect_err("502 should return Err");
        let msg_502 = err_502.to_string();
        assert!(
            msg_502.contains("bad gateway upstream worker crashed"),
            "expected plain-text body on 502, got: {msg_502}"
        );

        server.join().expect("join server thread");
        match prev_susi_home {
            Some(v) => std::env::set_var("SUSI_HOME", v),
            None => std::env::remove_var("SUSI_HOME"),
        }
        match prev_native_port {
            Some(v) => std::env::set_var("SUSI_NATIVE_PORT", v),
            None => std::env::remove_var("SUSI_NATIVE_PORT"),
        }
        let _ = std::fs::remove_dir_all(&temp_home);
    }
}
