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

//! Typed client for the standalone `susi-sandbox` service
//! (`127.0.0.1:18083`, override via `SUSI_SANDBOX_PORT`), plus the sandbox
//! helpers the service and every client share: the signed audit chain,
//! dev-build auto-install, daemon-state integrity, and the sandbox manager.
//!
//! Operations that should run in the substrate process (`ensure_global`,
//! Docker exec, daemon status / integrity hashing) prefer the service so
//! bollard and host daemon state stay isolated there. When the service is
//! unreachable — or explicit local env config (`SUSI_XDG`, `XDG_*_HOME`) is
//! set, or this process *is* the service — calls fall back to the local
//! helpers. `execute_in_docker` has no local fallback (requires the
//! service). Audit HMAC key ops stay local only: exposing them as an
//! unauthenticated localhost oracle would let any process mint signed audit
//! entries.

pub use susi_config;
pub use susi_error;

/// Marks this process as the `susi-sandbox` service: every client call
/// resolves against the local helpers from now on.
pub fn enter_service_mode() {
    susi_paths::enter_service_mode();
}

/// IPC client for the standalone `susi-sandbox` service.
pub(crate) mod service {
    use std::path::Path;
    use std::time::Duration;
    use susi_paths::loopback::{self, Auth};

    const DEFAULT_PORT: u16 = susi_paths::ports::SANDBOX_SERVICE;
    const TIMEOUT: Duration = Duration::from_millis(200);
    /// Docker container create/start/logs can exceed the fast IPC timeout.
    const DOCKER_TIMEOUT: Duration = Duration::from_secs(120);

    /// The service process, or explicit local env config (`SUSI_XDG`,
    /// `XDG_*_HOME`), resolves locally — a swapped HOME in tests must not
    /// read or write the host substrate's real sandbox state.
    fn local_override() -> bool {
        susi_paths::is_service_mode() || loopback::local_env_override()
    }

    fn call(
        method: &str,
        path: &str,
        payload: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Option<loopback::Response> {
        if local_override() {
            return None;
        }
        let body = match payload {
            Some(v) => Some(serde_json::to_string(&v).ok()?),
            None => None,
        };
        let ep = loopback::Endpoint {
            port: loopback::service_port("SUSI_SANDBOX_PORT", DEFAULT_PORT),
            timeout,
            auth: Auth::HostToken,
        };
        // Fast calls have a local fallback, so a closed service port is
        // backed off rather than re-dialled; Docker exec has none and keeps
        // dialling.
        if timeout == DOCKER_TIMEOUT {
            loopback::request(&ep, method, path, body.as_deref())
        } else {
            loopback::request_with_fallback(&ep, method, path, body.as_deref())
        }
    }

    fn json<T: serde::de::DeserializeOwned>(resp: &loopback::Response) -> Option<T> {
        serde_json::from_str(resp.ok_body()?).ok()
    }

    /// `POST /sandbox/ensure_global` — create global sandbox layout via substrate.
    pub fn ensure_global(global_dir: &Path) -> bool {
        call(
            "POST",
            "/sandbox/ensure_global",
            Some(serde_json::json!({ "global_dir": global_dir.to_string_lossy() })),
            TIMEOUT,
        )
        .is_some_and(|r| r.is_success())
    }

    /// `POST /sandbox/docker_exec` — run `cmd` inside the hardened sandbox image.
    pub fn docker_exec(cmd: &str) -> Option<String> {
        let resp = call(
            "POST",
            "/sandbox/docker_exec",
            Some(serde_json::json!({ "cmd": cmd })),
            DOCKER_TIMEOUT,
        )?;
        json(&resp)
    }

    /// `GET /daemon/status?workspace=&global_dir=`
    pub fn check_status(workspace: &Path, global_dir: &Path) -> Option<bool> {
        let ws = susi_paths::percent_encode_path(&workspace.to_string_lossy());
        let gd = susi_paths::percent_encode_path(&global_dir.to_string_lossy());
        let path = format!("/daemon/status?workspace={ws}&global_dir={gd}");
        json(&call("GET", &path, None, TIMEOUT)?)
    }

    /// `POST /daemon/verify_integrity`
    pub fn verify_integrity(bin_path: &Path, global_dir: &Path) -> Option<bool> {
        let payload = serde_json::json!({
            "bin_path": bin_path.to_string_lossy(),
            "global_dir": global_dir.to_string_lossy(),
        });
        json(&call(
            "POST",
            "/daemon/verify_integrity",
            Some(payload),
            TIMEOUT,
        )?)
    }

    /// `POST /daemon/hash_cached`
    pub fn hash_cached(bin_path: &Path, global_dir: &Path) -> Option<String> {
        let payload = serde_json::json!({
            "bin_path": bin_path.to_string_lossy(),
            "global_dir": global_dir.to_string_lossy(),
        });
        json(&call(
            "POST",
            "/daemon/hash_cached",
            Some(payload),
            TIMEOUT,
        )?)
    }
}

pub mod audit_chain;
pub mod audit_fidelity;
pub mod daemon_state;
pub mod manager;
pub mod placement_audit;

pub use manager::SandboxManager;
pub use susi_config::extensions;
pub use susi_config::versioned_store;
pub use susi_config::VersionedJsonStore;

/// Serializes tests that mutate or read process-global environment-derived
/// paths (`HOME`, `XDG_CONFIG_HOME`, `SUSI_*`). Mutators must hold this lock
/// for the whole env-swap window; readers of `SusiDirs`-derived paths must
/// hold it while resolving so a swapped HOME cannot flip path selection
/// mid-test.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod audit_chain_tests {
    use crate::audit_chain::*;
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn append_waits_for_the_cross_process_chain_lock() {
        // The chain's HMAC key path follows SUSI_HOME, which sibling tests swap.
        let _guard = crate::env_test_lock();
        let log = temp_audit();
        // Another "process" holds the chain lock: the append must not
        // proceed on a possibly stale tip.
        let held = crate::susi_config::file_lock::FileLock::acquire(
            log.parent().unwrap(),
            "audit.log.chain",
        )
        .unwrap();
        let key = [0x24u8; 32];
        let entry = |details| Entry {
            level: "Info",
            event_type: "T",
            details,
            pid: 1,
        };
        // A lock held past the budget fails the append closed.
        let budget = std::time::Duration::from_millis(40);
        assert!(append_with_key_within(&log, &key, &entry("blocked"), budget).is_err());
        // A holder that keeps the lock while writers queue behind it is waited
        // out within CHAIN_LOCK_BUDGET, not refused: writers queued behind
        // syncing holders on Windows used to lose their appends that way.
        let started = std::time::Instant::now();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(120));
            drop(held);
        });
        append_with_key(&log, &key, &entry("after")).unwrap();
        assert!(started.elapsed() >= std::time::Duration::from_millis(90));
        release.join().unwrap();
        assert_eq!(verify_with_key(&log, &key), Ok(1));
    }

    #[test]
    fn concurrent_processes_extend_one_unforked_chain() {
        const WORKERS: usize = 4;
        const APPENDS: usize = 60;
        let key = [0x42u8; 32];
        if let Ok(log) = std::env::var("SUSI_CHAIN_WORKER_LOG") {
            let log = PathBuf::from(log);
            // Start together so appends genuinely contend.
            let go = log.with_extension("go");
            while !go.exists() {
                std::thread::yield_now();
            }
            for i in 0..APPENDS {
                let details = format!("{}-{i}", std::process::id());
                let entry = Entry {
                    level: "Info",
                    event_type: "T",
                    details: &details,
                    pid: std::process::id(),
                };
                append_with_key(&log, &key, &entry).unwrap();
            }
            return;
        }
        let log = temp_audit();
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..WORKERS)
            .map(|_| {
                std::process::Command::new(&exe)
                    .args([
                        "concurrent_processes_extend_one_unforked_chain",
                        "--nocapture",
                    ])
                    .env("SUSI_CHAIN_WORKER_LOG", &log)
                    .spawn()
                    .unwrap()
            })
            .collect();
        fs::write(log.with_extension("go"), "").unwrap();
        for mut c in children {
            assert!(c.wait().unwrap().success(), "worker failed");
        }
        assert_eq!(verify_with_key(&log, &key), Ok(WORKERS * APPENDS));
    }

    #[test]
    fn concurrent_processes_never_tear_jsonl_lines() {
        const WORKERS: usize = 4;
        const APPENDS: usize = 60;
        if let Ok(ws) = std::env::var("SUSI_JSONL_WORKER_WS") {
            let ws = PathBuf::from(ws);
            let go = ws.join("go");
            while !go.exists() {
                std::thread::yield_now();
            }
            // Large, structured outcomes: many Display write calls per line
            // if the line is not emitted as one write.
            let outcome = serde_json::json!({ "k": "v".repeat(512), "n": [1, 2, 3] }).to_string();
            for i in 0..APPENDS {
                crate::manager::SusiMemory::save_interaction(
                    &ws,
                    &format!("intent {} {i}", std::process::id()),
                    &outcome,
                    "test",
                );
            }
            return;
        }
        let ws = temp_audit().parent().unwrap().to_path_buf();
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = (0..WORKERS)
            .map(|_| {
                std::process::Command::new(&exe)
                    .args(["concurrent_processes_never_tear_jsonl_lines", "--nocapture"])
                    .env("SUSI_JSONL_WORKER_WS", &ws)
                    .spawn()
                    .unwrap()
            })
            .collect();
        fs::write(ws.join("go"), "").unwrap();
        for mut c in children {
            assert!(c.wait().unwrap().success(), "worker failed");
        }
        let text = fs::read_to_string(ws.join(".susi/memory.jsonl")).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), WORKERS * APPENDS);
        for (n, line) in lines.iter().enumerate() {
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_ok(),
                "line {} torn: {}",
                n + 1,
                &line[..line.len().min(80)]
            );
        }
    }

    fn temp_audit() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("susi_audit_chain_{}_{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.join("audit.log")
    }

    #[test]
    fn signed_entries_verify_and_detect_tamper() {
        // The HMAC key path derives from SusiDirs::substrate_home(); serialize
        // against tests that swap HOME so the key does not change mid-test.
        let _guard = crate::env_test_lock();
        let path = temp_audit();
        append_signed_entry(&path, "Info", "TEST_A", "alpha-payload", 1).unwrap();
        append_signed_entry(&path, "Info", "TEST_B", "beta-payload", 1).unwrap();
        assert_eq!(verify_chain(&path).unwrap(), 2);

        let mut content = fs::read_to_string(&path).unwrap();
        content = content.replace("alpha-payload", "EVIL-payload");
        fs::write(&path, &content).unwrap();
        assert!(verify_chain(&path).is_err());
    }
}

#[cfg(test)]
mod service_ipc_tests {
    use crate::daemon_state::SusiDaemonState;
    use crate::service;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;

    fn read_http_request(stream: &mut TcpStream) -> (String, String) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            let n = stream.read(&mut chunk).expect("read request header");
            assert!(n > 0, "unexpected EOF reading HTTP request header");
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
            let n = stream.read(&mut chunk).expect("read request body");
            assert!(n > 0, "unexpected EOF reading HTTP request body");
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = String::from_utf8_lossy(&buf[body_start..body_start + content_len]).to_string();
        (head, body)
    }

    fn write_http_response(stream: &mut TcpStream, status: &str, body: &str) {
        write!(
            stream,
            "HTTP/1.0 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .expect("write HTTP response");
        stream.flush().expect("flush HTTP response");
    }

    #[test]
    fn sandbox_service_client_routes_all_five_endpoints_and_honors_local_override() {
        let _guard = crate::env_test_lock();
        let temp_home =
            std::env::temp_dir().join(format!("susi_sbx_ipc_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_home);
        std::fs::create_dir_all(&temp_home).expect("create temp SUSI_HOME");
        std::fs::write(temp_home.join("api_token"), "sbx-secret-token\n").expect("seed api_token");
        std::fs::write(
            temp_home.join("daemon.lock"),
            format!("{}\n", std::process::id()),
        )
        .expect("seed daemon.lock");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        let port = listener.local_addr().expect("listener addr").port();

        let prev_home = std::env::var_os("SUSI_HOME");
        let prev_port = std::env::var_os("SUSI_SANDBOX_PORT");
        let prev_xdg = std::env::var_os("SUSI_XDG");
        let prev_cfg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_data = std::env::var_os("XDG_DATA_HOME");
        let prev_cache = std::env::var_os("XDG_CACHE_HOME");

        std::env::set_var("SUSI_HOME", &temp_home);
        std::env::set_var("SUSI_SANDBOX_PORT", port.to_string());
        std::env::remove_var("SUSI_XDG");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("XDG_CACHE_HOME");

        let server = std::thread::spawn(move || {
            // 1. POST /sandbox/ensure_global -> 204 No Content
            let (mut s1, _) = listener.accept().expect("accept ensure_global");
            let (h1, b1) = read_http_request(&mut s1);
            assert!(
                h1.starts_with("POST /sandbox/ensure_global HTTP/1.0"),
                "{h1}"
            );
            assert!(
                h1.contains("Authorization: Bearer sbx-secret-token"),
                "{h1}"
            );
            let j1: serde_json::Value = serde_json::from_str(&b1).unwrap();
            assert!(j1["global_dir"].as_str().unwrap().contains("global-sbx"));
            write_http_response(&mut s1, "204 No Content", "");
            drop(s1);

            // 2. POST /sandbox/docker_exec -> 200 OK "container-ok"
            let (mut s2, _) = listener.accept().expect("accept docker_exec");
            let (h2, b2) = read_http_request(&mut s2);
            assert!(h2.starts_with("POST /sandbox/docker_exec HTTP/1.0"), "{h2}");
            let j2: serde_json::Value = serde_json::from_str(&b2).unwrap();
            assert_eq!(j2["cmd"], "echo sandbox");
            write_http_response(&mut s2, "200 OK", "\"container-ok\"");
            drop(s2);

            // 3. GET /daemon/status -> 200 OK true
            let (mut s3, _) = listener.accept().expect("accept daemon/status");
            let (h3, _) = read_http_request(&mut s3);
            assert!(h3.starts_with("GET /daemon/status?workspace="), "{h3}");
            write_http_response(&mut s3, "200 OK", "true");
            drop(s3);

            // 4. POST /daemon/verify_integrity -> 200 OK true
            let (mut s4, _) = listener.accept().expect("accept verify_integrity");
            let (h4, b4) = read_http_request(&mut s4);
            assert!(
                h4.starts_with("POST /daemon/verify_integrity HTTP/1.0"),
                "{h4}"
            );
            let j4: serde_json::Value = serde_json::from_str(&b4).unwrap();
            assert!(j4["bin_path"].as_str().unwrap().contains("susi-bin"));
            write_http_response(&mut s4, "200 OK", "true");
            drop(s4);

            // 5. POST /daemon/hash_cached -> 200 OK "deadbeef..."
            let (mut s5, _) = listener.accept().expect("accept hash_cached");
            let (h5, _) = read_http_request(&mut s5);
            assert!(h5.starts_with("POST /daemon/hash_cached HTTP/1.0"), "{h5}");
            write_http_response(&mut s5, "200 OK", "\"sha256-cached-digest\"");
            drop(s5);
        });

        let gdir = temp_home.join("global-sbx");
        let ws = temp_home.join("ws");
        let bin = temp_home.join("susi-bin");

        assert!(service::ensure_global(&gdir));
        assert_eq!(
            service::docker_exec("echo sandbox").as_deref(),
            Some("container-ok")
        );
        assert!(SusiDaemonState::check_status(&ws, &gdir));
        assert!(SusiDaemonState::verify_binary_integrity(&bin, &gdir).unwrap());
        assert_eq!(
            SusiDaemonState::calculate_binary_hash_cached(&bin, &gdir).unwrap(),
            "sha256-cached-digest"
        );

        // When SUSI_XDG is set, local_override() short-circuits IPC without dialling
        std::env::set_var("SUSI_XDG", "1");
        assert!(!service::ensure_global(Path::new("/unused")));
        assert!(service::docker_exec("echo bypassed").is_none());

        server.join().expect("server thread completed cleanly");

        let restore = |k: &str, v: Option<std::ffi::OsString>| match v {
            Some(val) => std::env::set_var(k, val),
            None => std::env::remove_var(k),
        };
        restore("SUSI_HOME", prev_home);
        restore("SUSI_SANDBOX_PORT", prev_port);
        restore("SUSI_XDG", prev_xdg);
        restore("XDG_CONFIG_HOME", prev_cfg);
        restore("XDG_DATA_HOME", prev_data);
        restore("XDG_CACHE_HOME", prev_cache);
        let _ = std::fs::remove_dir_all(&temp_home);
    }
}
