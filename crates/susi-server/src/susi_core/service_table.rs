//! Leaf-service registry and the substrate's process table.
//!
//! The microkernel runs its leaf services (`susi-paths`/`susi-error`/
//! `susi-config`/`susi-sandbox`/`susi-native`) as standalone localhost
//! processes. This module is the shared contract every plane can read:
//! which services exist, which pids the daemon currently supervises, and
//! whether a given port is live. The table itself is a single JSON file
//! under `substrate_home/services.json`, written atomically, so a vendored
//! copy in any consumer process observes the same process table the daemon
//! wrote — the same file-backed rendezvous pattern as `plane_bus_ipc`,
//! `broker`, and `capture`.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::susi_error::{EaiError, EaiResult};
use crate::susi_paths::SusiDirs;

/// One leaf service the daemon may supervise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeafService {
    /// Stable service name used on the CLI and in the process table.
    pub name: &'static str,
    /// Cargo binary name to spawn when the port is dead.
    pub binary: &'static str,
    /// Env var that overrides the bind port.
    pub port_env: &'static str,
    /// Default localhost port when `port_env` is unset.
    pub default_port: u16,
}

/// The microkernel's leaf services, in boot order (lower deps first).
pub const LEAF_SERVICES: &[LeafService] = &[
    LeafService {
        name: "susi-paths",
        binary: "susi-paths",
        port_env: "SUSI_PATHS_PORT",
        default_port: 18080,
    },
    LeafService {
        name: "susi-error",
        binary: "susi-error",
        port_env: "SUSI_ERROR_PORT",
        default_port: 18081,
    },
    LeafService {
        name: "susi-config",
        binary: "susi-config",
        port_env: "SUSI_CONFIG_PORT",
        default_port: 18082,
    },
    LeafService {
        name: "susi-sandbox",
        binary: "susi-sandbox",
        port_env: "SUSI_SANDBOX_PORT",
        default_port: 18083,
    },
    LeafService {
        name: "susi-native",
        binary: "susi-native",
        port_env: "SUSI_NATIVE_PORT",
        default_port: 18084,
    },
];

impl LeafService {
    /// Effective port: `port_env` override wins over the default.
    pub fn port(&self) -> u16 {
        std::env::var(self.port_env)
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(self.default_port)
    }
}

/// Look up a leaf service by name.
pub fn leaf_service(name: &str) -> Option<&'static LeafService> {
    LEAF_SERVICES.iter().find(|s| s.name == name)
}

/// One supervised process row in the table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceRecord {
    /// Leaf service name (`susi-native`, ...).
    pub name: String,
    /// OS pid the daemon spawned.
    pub pid: u32,
    /// Port the service listens on.
    pub port: u16,
    /// Unix seconds when the daemon (re)started it.
    pub started_at: u64,
    /// How many times the supervisor has restarted it since daemon boot.
    pub restarts: u32,
    /// Unix seconds until which respawns are suspended — set when the
    /// service exceeds MAX_RESTARTS so a crash-looping service backs off
    /// instead of churning, then gets retried (restart counter reset)
    /// rather than abandoned forever.
    #[serde(default)]
    pub disabled_until: Option<u64>,
}

/// Where the process table lives: shared host state, not per-pid, because
/// every plane must observe the same supervised set.
pub fn table_path() -> PathBuf {
    SusiDirs::substrate_home().join("services.json")
}

/// Load the process table. Missing/corrupt files read as empty — a corrupt
/// table must never wedge a plane (the daemon rewrites it on the next
/// supervision pass anyway).
pub fn load() -> Vec<ServiceRecord> {
    load_from(&table_path())
}

/// Test seam: load from an explicit path instead of the shared location.
pub fn load_from(path: &std::path::Path) -> Vec<ServiceRecord> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Persist the process table atomically (temp file + rename, same contract
/// as `SusiConfig::save` — a torn write must never leave an empty file for
/// concurrent readers).
pub fn save(records: &[ServiceRecord]) -> EaiResult<()> {
    save_to(&table_path(), records)
}

/// Test seam: save to an explicit path instead of the shared location.
pub fn save_to(path: &std::path::Path, records: &[ServiceRecord]) -> EaiResult<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| EaiError::filesystem(format!("create {}: {e}", dir.display())))?;
    }
    let body = serde_json::to_string_pretty(records)
        .map_err(|e| EaiError::internal(format!("serialize service table: {e}")))?;
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    fs::write(&tmp, &body)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|e| EaiError::filesystem(format!("persist {}: {e}", path.display())))
}

/// Upsert a record by service name; returns the stored record.
pub fn record(records: &mut Vec<ServiceRecord>, name: &str, pid: u32, port: u16) -> ServiceRecord {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Some(existing) = records.iter_mut().find(|r| r.name == name) {
        existing.pid = pid;
        existing.port = port;
        existing.started_at = now;
        existing.restarts = existing.restarts.saturating_add(1);
        existing.disabled_until = None;
        return existing.clone();
    }
    let rec = ServiceRecord {
        name: name.to_string(),
        pid,
        port,
        started_at: now,
        restarts: 0,
        disabled_until: None,
    };
    records.push(rec.clone());
    rec
}

/// Remove a record by service name; true when something was removed.
pub fn remove(records: &mut Vec<ServiceRecord>, name: &str) -> bool {
    let before = records.len();
    records.retain(|r| r.name != name);
    records.len() != before
}

/// TCP liveness probe: the leaf services are all `axum` HTTP listeners, so
/// a successful connect to `127.0.0.1:port` is the health contract.
pub fn probe(port: u16) -> bool {
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// Whether `pid` refers to a live process. `/proc` on Linux; `kill -0`
/// elsewhere on unix; conservatively "alive" where neither exists — a false
/// "dead" would make the supervisor respawn a healthy service.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        PathBuf::from(format!("/proc/{pid}")).is_dir()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(true)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        true
    }
}

/// A snapshot of every leaf service with its live health — what the
/// `services` CLI and `os_services` tool render. Supervision metadata comes
/// from the table when present; health is always probed live.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    /// Leaf service name.
    pub name: String,
    /// Effective port.
    pub port: u16,
    /// Supervised pid, when the daemon owns one.
    pub pid: Option<u32>,
    /// Restart count while supervised.
    pub restarts: u32,
    /// Unix seconds the supervised process was spawned at, when known.
    pub started_at: Option<u64>,
    /// Live TCP probe result.
    pub up: bool,
}

impl ServiceStatus {
    /// Human uptime for a supervised process ("3d2h", "4h12m", "90s");
    /// "-" when the service was never supervised or `started_at` is in
    /// the future (clock skew — never panic on timestamps).
    pub fn uptime(&self) -> String {
        let Some(start) = self.started_at else {
            return "-".to_string();
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let secs = now.saturating_sub(start);
        if secs >= 86400 {
            format!("{}d{}h", secs / 86400, (secs % 86400) / 3600)
        } else if secs >= 3600 {
            format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
        } else if secs >= 60 {
            format!("{}m{}s", secs / 60, secs % 60)
        } else {
            format!("{secs}s")
        }
    }
}

/// Snapshot all leaf services: probe each port, join with table records.
pub fn status() -> Vec<ServiceStatus> {
    let table: BTreeMap<String, ServiceRecord> =
        load().into_iter().map(|r| (r.name.clone(), r)).collect();
    LEAF_SERVICES
        .iter()
        .map(|svc| {
            let rec = table.get(svc.name);
            ServiceStatus {
                name: svc.name.to_string(),
                port: svc.port(),
                pid: rec.map(|r| r.pid),
                restarts: rec.map(|r| r.restarts).unwrap_or(0),
                started_at: rec.map(|r| r.started_at),
                up: probe(svc.port()),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_round_trip_and_upsert() {
        let dir = std::env::temp_dir().join(format!("susi_svc_table_{}", std::process::id()));
        let path = dir.join("services.json");
        let _ = fs::remove_dir_all(&dir);

        let mut records = Vec::new();
        let r1 = record(&mut records, "susi-native", 4242, 18084);
        assert_eq!(r1.restarts, 0);
        save_to(&path, &records).unwrap();
        let loaded = load_from(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].pid, 4242);

        // Upsert bumps restarts and replaces the pid.
        let mut loaded = loaded;
        let r2 = record(&mut loaded, "susi-native", 5555, 18084);
        assert_eq!(r2.pid, 5555);
        assert_eq!(r2.restarts, 1);
        assert_eq!(loaded.len(), 1);

        assert!(remove(&mut loaded, "susi-native"));
        assert!(!remove(&mut loaded, "susi-native"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_table_reads_as_empty() {
        let dir = std::env::temp_dir().join(format!("susi_svc_corrupt_{}", std::process::id()));
        let path = dir.join("services.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, "{not json").unwrap();
        assert!(load_from(&path).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pid_alive_detects_self_and_dead() {
        assert!(pid_alive(std::process::id()));
        // pid 1 always exists on unix; an implausible pid must read dead.
        assert!(!pid_alive(4_000_000));
    }

    #[test]
    fn leaf_registry_covers_five_services_in_boot_order() {
        assert_eq!(LEAF_SERVICES.len(), 5);
        assert_eq!(LEAF_SERVICES[0].name, "susi-paths");
        assert_eq!(LEAF_SERVICES[4].name, "susi-native");
        assert!(leaf_service("susi-config").is_some());
        assert!(leaf_service("nope").is_none());
        assert_eq!(leaf_service("susi-native").map(|s| s.port()), Some(18084));
    }

    #[test]
    fn probe_reports_closed_port() {
        // 9 is the discard port; nothing sane binds it in a test env.
        assert!(!probe(9));
    }
}
