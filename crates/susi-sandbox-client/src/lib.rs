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

use std::sync::atomic::{AtomicBool, Ordering};

/// Set by the service process so no call routes back to itself over IPC.
static SERVICE_MODE: AtomicBool = AtomicBool::new(false);

/// Marks this process as the `susi-sandbox` service: every client call
/// resolves against the local helpers from now on.
pub fn enter_service_mode() {
    SERVICE_MODE.store(true, Ordering::Relaxed);
}

/// IPC client for the standalone `susi-sandbox` service.
pub(crate) mod service {
    use std::path::Path;
    use std::time::Duration;
    use susi_paths::loopback::{self, Auth};

    const DEFAULT_PORT: u16 = 18083;
    const TIMEOUT: Duration = Duration::from_millis(200);
    /// Docker container create/start/logs can exceed the fast IPC timeout.
    const DOCKER_TIMEOUT: Duration = Duration::from_secs(120);

    /// The service process, or explicit local env config (`SUSI_XDG`,
    /// `XDG_*_HOME`), resolves locally — a swapped HOME in tests must not
    /// read or write the host substrate's real sandbox state.
    fn local_override() -> bool {
        super::SERVICE_MODE.load(super::Ordering::Relaxed) || loopback::local_env_override()
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
        loopback::request(&ep, method, path, body.as_deref())
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
pub mod daemon_state;
pub mod manager;

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
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn append_waits_for_the_cross_process_chain_lock() {
        let log = temp_audit();
        // Another "process" holds the chain lock: the append must not
        // proceed on a possibly stale tip.
        let held = crate::susi_config::file_lock::FileLock::acquire(
            log.parent().unwrap(),
            "audit.log.chain",
        )
        .unwrap();
        assert!(append_signed_entry(&log, "Info", "T", "blocked", 1).is_err());
        drop(held);
        append_signed_entry(&log, "Info", "T", "after", 1).unwrap();
        assert_eq!(verify_chain(&log), Ok(1));
    }

    #[test]
    fn concurrent_processes_extend_one_unforked_chain() {
        const WORKERS: usize = 4;
        const APPENDS: usize = 300;
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
        const APPENDS: usize = 200;
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
