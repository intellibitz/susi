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
    /// True when the port is bound by a process the daemon did not spawn
    /// (`cargo run`, a stale dev binary, a foreign listener). External
    /// records are observed and displayed but never signaled — the
    /// supervisor only kills its own children. When the external process
    /// dies, the supervisor drops this record and spawns its own.
    #[serde(default)]
    pub external: bool,
    /// Operator stop (`susi services stop`): the supervisor leaves the
    /// service down until `susi services start` clears this — distinct
    /// from `disabled_until`, which is crash-loop backoff the supervisor
    /// manages itself.
    #[serde(default)]
    pub stopped: bool,
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
/// Duplicate service names collapse to the first row — the table
/// invariant is one row per name, and a hand-edited/corrupted file must
/// not hand readers a state writers can't reason about.
pub fn load_from(path: &std::path::Path) -> Vec<ServiceRecord> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };
    let mut rows: Vec<ServiceRecord> = serde_json::from_str(&text).unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    rows.retain(|r| seen.insert(r.name.clone()));
    rows
}

/// Persist the process table atomically (temp file + rename, same contract
/// as `SusiConfig::save` — a torn write must never leave an empty file for
/// concurrent readers).
pub fn save(records: &[ServiceRecord]) -> EaiResult<()> {
    save_to(&table_path(), records)
}

/// Atomic read-modify-write on the shared table, serialized across
/// processes by the same lockfile primitive the commit ledger uses.
/// Without it, `susi services stop` can load, mutate, and save while the
/// supervisor does the same — whichever writes last wins, and an
/// operator's stop flag is silently clobbered by the monitor's pass.
/// The closure sees the freshly loaded table; its mutations are what
/// gets persisted.
pub fn update_with<R>(f: impl FnOnce(&mut Vec<ServiceRecord>) -> R) -> EaiResult<R> {
    let path = table_path();
    let Some(_lock) = path
        .parent()
        .and_then(|dir| crate::susi_core::commit_log::FileLock::acquire(dir, "services"))
    else {
        return Err(EaiError::filesystem(
            "service table lock unavailable — possible wedged holder or extreme contention",
        ));
    };
    let mut table = load();
    let out = f(&mut table);
    save(&table)?;
    Ok(out)
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
    // One row per name — a table restored from a corrupted file could
    // carry duplicates; absorb them into the first and drop the rest
    // so the invariant is repaired, not just respected.
    let mut seen = false;
    let mut updated: Option<ServiceRecord> = None;
    records.retain_mut(|r| {
        if r.name != name {
            return true;
        }
        if seen {
            return false;
        }
        seen = true;
        r.pid = pid;
        r.port = port;
        r.started_at = now;
        r.restarts = r.restarts.saturating_add(1);
        r.disabled_until = None;
        r.external = false;
        r.stopped = false;
        updated = Some(r.clone());
        true
    });
    if let Some(rec) = updated {
        return rec;
    }
    let rec = ServiceRecord {
        name: name.to_string(),
        pid,
        port,
        started_at: now,
        restarts: 0,
        disabled_until: None,
        external: false,
        stopped: false,
    };
    records.push(rec.clone());
    rec
}

/// Record a service whose port is bound by a process the daemon did not
/// spawn. `pid` is best-effort (`pid_for_port` may not identify the
/// holder — `0` means unknown). External rows keep `restarts` at 0 and
/// are replaced by a supervised row if the supervisor ever takes over.
pub fn record_external(
    records: &mut Vec<ServiceRecord>,
    name: &str,
    pid: u32,
    port: u16,
) -> ServiceRecord {
    let now = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let rec = ServiceRecord {
        name: name.to_string(),
        pid,
        port,
        started_at: now,
        restarts: 0,
        disabled_until: None,
        external: true,
        stopped: false,
    };
    // Same one-row-per-name repair as `record`: overwrite the first
    // same-named row and drop any duplicates a corrupted table carried.
    let mut seen = false;
    records.retain_mut(|r| {
        if r.name != name {
            return true;
        }
        if seen {
            return false;
        }
        seen = true;
        *r = rec.clone();
        true
    });
    if !seen {
        records.push(rec.clone());
    }
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

/// HTTP liveness probe — strictly stronger than `probe`: a listener can
/// accept TCP while its request handler is wedged (deadlocked runtime,
/// starved worker pool). Any HTTP response — even a 404 — proves the
/// routing layer answers requests. Leaf services all serve axum, so
/// `GET /` always elicits a status line.
pub fn http_probe(port: u16) -> bool {
    use std::io::{Read, Write};
    let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(300)));
    if stream
        .write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 16];
    match stream.read(&mut buf) {
        Ok(n) => buf[..n].starts_with(b"HTTP/"),
        Err(_) => false,
    }
}

/// Whether `pid` refers to a live process. `/proc` on Linux; `kill -0`
/// elsewhere on unix; conservatively "alive" where neither exists — a false
/// "dead" would make the supervisor respawn a healthy service.
pub fn pid_alive(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        if !PathBuf::from(format!("/proc/{pid}")).is_dir() {
            return false;
        }
        // A zombie still has a /proc dir but cannot serve or hold
        // anything — read the state field (the char after the last ')'
        // in `pid (comm) state ...`; comm itself may contain parens).
        // Unreadable stat keeps the conservative "alive" answer.
        fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|s| s.rsplit(')').next().map(|tail| tail.to_string()))
            .and_then(|tail| tail.trim().chars().next())
            .map(|state| state != 'Z')
            .unwrap_or(true)
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

/// Identify the pid holding a localhost TCP port, when the OS lets us.
/// Linux: the socket inode comes from `/proc/net/tcp{,6}` (local_address
/// port match, state LISTEN), then `/proc/*/fd` is scanned for the
/// `socket:[inode]` symlink. `None` on other platforms or when the
/// holder is unreadable — callers must tolerate an unknown owner.
#[cfg(target_os = "linux")]
pub fn pid_for_port(port: u16) -> Option<u32> {
    let hex_port = format!("{port:04X}");
    let mut inode = String::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = fs::read_to_string(table) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let mut cols = line.split_whitespace();
            let _idx = cols.next();
            let Some(local) = cols.next() else {
                continue;
            };
            let Some((_ip, p)) = local.rsplit_once(':') else {
                continue;
            };
            if p != hex_port {
                continue;
            }
            // Columns: sl local_address rem_address st ... inode — field 3
            // is state (0A = LISTEN), field 9 the socket inode.
            let _remote = cols.next();
            let state = cols.next().unwrap_or("");
            if state != "0A" {
                continue;
            }
            inode = line.split_whitespace().nth(9).unwrap_or("").to_string();
            break;
        }
        if !inode.is_empty() {
            break;
        }
    }
    if inode.is_empty() {
        return None;
    }
    let needle = format!("socket:[{inode}]");
    let proc = PathBuf::from("/proc");
    for entry in fs::read_dir(&proc).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            continue;
        }
        let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if fs::read_link(fd.path())
                .map(|t| t.to_string_lossy() == needle)
                .unwrap_or(false)
            {
                return name.parse::<u32>().ok();
            }
        }
    }
    None
}

/// No `/proc` on non-Linux platforms — the owner of a port is unknown.
#[cfg(not(target_os = "linux"))]
pub fn pid_for_port(_port: u16) -> Option<u32> {
    None
}

/// Resident set size of `pid` in KiB (`VmRSS` from `/proc/<pid>/status`)
/// — the footprint column for service tables. `None` when the pid is
/// dead, unreadable, or the field is absent.
#[cfg(target_os = "linux")]
pub fn rss_kb(pid: u32) -> Option<u64> {
    let text = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
}

/// No `/proc` on non-Linux platforms — memory footprint is unknown.
#[cfg(not(target_os = "linux"))]
pub fn rss_kb(_pid: u32) -> Option<u64> {
    None
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
    /// True when the port is bound by a process the daemon does not own.
    #[serde(default)]
    pub external: bool,
    /// True when the operator stopped the service — the supervisor holds
    /// it down until `susi services start`.
    #[serde(default)]
    pub stopped: bool,
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

    /// Human resident memory for the process behind `pid` ("84.2 MiB",
    /// "612 KiB"); "-" when no supervised pid exists or the OS won't
    /// tell us — never invent a number for an unreadable process.
    pub fn rss(&self) -> String {
        match self.pid.and_then(rss_kb) {
            Some(kb) if kb >= 1024 => format!("{:.1} MiB", kb as f64 / 1024.0),
            Some(kb) => format!("{kb} KiB"),
            None => "-".to_string(),
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
                // External rows may carry pid 0 when the holder couldn't
                // be identified — render unknown, not a bogus "pid 0".
                pid: rec.and_then(|r| {
                    if r.external && r.pid == 0 {
                        None
                    } else {
                        Some(r.pid)
                    }
                }),
                restarts: rec.map(|r| r.restarts).unwrap_or(0),
                started_at: rec.map(|r| r.started_at),
                up: probe(svc.port()),
                external: rec.map(|r| r.external).unwrap_or(false),
                stopped: rec.map(|r| r.stopped).unwrap_or(false),
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
    fn rss_kb_reads_self_and_none_for_dead() {
        assert!(rss_kb(std::process::id()).is_some_and(|kb| kb > 0));
        assert_eq!(rss_kb(4_000_000), None);
    }

    #[test]
    fn pid_alive_detects_self_and_dead() {
        assert!(pid_alive(std::process::id()));
        // pid 1 always exists on unix; an implausible pid must read dead.
        assert!(!pid_alive(4_000_000));
    }

    #[test]
    fn pid_for_port_finds_a_listener_we_own() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let found = pid_for_port(port);
        #[cfg(target_os = "linux")]
        assert_eq!(found, Some(std::process::id()));
        #[cfg(not(target_os = "linux"))]
        assert!(found.is_none());
        // An unbound high port must not produce a bogus owner.
        assert!(pid_for_port(1).is_none() || probe(1));
    }

    #[test]
    fn record_external_marks_row_and_supervised_record_replaces_it() {
        let mut records = Vec::new();
        let ext = record_external(&mut records, "susi-native", 0, 18084);
        assert!(ext.external);
        assert_eq!(ext.restarts, 0);
        // A supervised spawn upserts over the external row — the daemon's
        // own child replaces the observed one, external flag cleared.
        let owned = record(&mut records, "susi-native", 7777, 18084);
        assert!(!owned.external);
        assert_eq!(owned.pid, 7777);
        assert_eq!(records.len(), 1);
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

    #[test]
    fn http_probe_distinguishes_serving_from_listening() {
        use std::io::{Read, Write};
        // A bare TCP listener that accepts but never speaks HTTP fails
        // the HTTP probe while passing the TCP one — the wedge case the
        // monitor must catch.
        let wedged = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = wedged.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in wedged.incoming().flatten() {
                let mut s = s;
                let mut buf = [0u8; 64];
                let _ = s.read(&mut buf);
                // Never answer — wedged handler.
            }
        });
        assert!(probe(port), "TCP probe sees the listener");
        assert!(!http_probe(port), "HTTP probe must see through the wedge");

        // A minimal HTTP responder passes both.
        let serving = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = serving.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in serving.incoming().flatten() {
                let mut s = s;
                let _ = s.write_all(b"HTTP/1.0 404 Not Found\r\n\r\n");
            }
        });
        assert!(http_probe(port), "any HTTP status line counts as alive");
    }

    mod prop_tests {
        use super::*;
        use proptest::prelude::*;

        fn name_strategy() -> impl Strategy<Value = String> {
            // Real tables only carry leaf-service names, but the state
            // machine must hold for arbitrary keys — including unicode.
            "[a-z\\-]{1,16}".prop_map(String::from)
        }

        fn record_strategy() -> impl Strategy<Value = ServiceRecord> {
            (
                name_strategy(),
                any::<u32>(),
                any::<u16>(),
                any::<u64>(),
                any::<u32>(),
                any::<bool>(),
                any::<bool>(),
                any::<Option<u64>>(),
            )
                .prop_map(
                    |(name, pid, port, started_at, restarts, external, stopped, disabled_until)| {
                        ServiceRecord {
                            name,
                            pid,
                            port,
                            started_at,
                            restarts,
                            disabled_until,
                            external,
                            stopped,
                        }
                    },
                )
        }

        proptest! {
            /// serde round-trip: save_to → load_from is lossless modulo
            /// the one-row-per-name repair — duplicate names collapse to
            /// the first row on load, everything else survives verbatim.
            #[test]
            fn save_load_round_trips_any_table(
                records in proptest::collection::vec(record_strategy(), 0..8)
            ) {
                let dir = std::env::temp_dir().join(format!(
                    "susi_svc_prop_{}_{:?}",
                    std::process::id(),
                    std::thread::current().id()
                ));
                let path = dir.join("services.json");
                save_to(&path, &records).expect("save");
                let loaded = load_from(&path);
                let mut deduped = records.clone();
                let mut seen = std::collections::HashSet::new();
                deduped.retain(|r| seen.insert(r.name.clone()));
                prop_assert_eq!(loaded, deduped);
                let _ = fs::remove_dir_all(&dir);
            }

            /// record() is an upsert: the table holds exactly one row per
            /// name, and restarts counts every re-record of the same name.
            #[test]
            fn record_keeps_one_row_per_name(
                name in name_strategy(),
                pids in proptest::collection::vec(any::<u32>(), 1..6),
                port in any::<u16>(),
            ) {
                let mut table = Vec::new();
                for (i, pid) in pids.iter().enumerate() {
                    let rec = record(&mut table, &name, *pid, port);
                    prop_assert_eq!(rec.pid, *pid);
                    prop_assert_eq!(rec.restarts as usize, i);
                }
                prop_assert_eq!(table.iter().filter(|r| r.name == name).count(), 1);
            }

            /// record_external() always leaves exactly one external row —
            /// even over a supervised record (the supervisor must never
            /// signal a pid it didn't spawn, so the flag is rewritten).
            #[test]
            fn record_external_overwrites_to_external(
                seed in proptest::collection::vec(record_strategy(), 0..6),
                name in name_strategy(),
                pid in any::<u32>(),
                port in any::<u16>(),
            ) {
                let mut table = seed;
                let rec = record_external(&mut table, &name, pid, port);
                prop_assert!(rec.external);
                prop_assert_eq!(rec.restarts, 0);
                prop_assert_eq!(table.iter().filter(|r| r.name == name).count(), 1);
                prop_assert!(table.iter().find(|r| r.name == name).map(|r| r.external) == Some(true));
            }

            /// remove() is idempotent and total: returns true iff the
            /// name was present, and leaves no row with that name.
            #[test]
            fn remove_is_total_and_idempotent(
                seed in proptest::collection::vec(record_strategy(), 0..8),
                name in name_strategy(),
            ) {
                let mut table = seed;
                let present = table.iter().any(|r| r.name == name);
                prop_assert_eq!(remove(&mut table, &name), present);
                prop_assert!(!table.iter().any(|r| r.name == name));
                prop_assert!(!remove(&mut table, &name));
            }
        }
    }
}
