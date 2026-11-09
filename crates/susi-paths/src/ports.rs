//! Canonical public substrate ports. External clients may hard-code these.
//! A *uniform* `port_offset` shifts all five together (and the gossip and
//! leaf-service ports with them): the contract shape stays fixed while a
//! second instance or a nonstandard host layout gets clean ports (offset
//! 100 gives 9190–9194). The daemon never drifts to an ephemeral or a
//! per-port alternative.
//!
//! **Where the offset comes from.** An operator pins it with
//! `SUSI_PORT_OFFSET` (any value, `0` included) or a non-zero `port_offset`
//! config key; a pinned offset is bound exactly or the start refuses. With
//! nothing pinned the daemon serves the canonical block, and when a foreign
//! process holds one of the six ports it binds (9090–9094 plus gossip 9095)
//! it falls back to the first offset `k * 100` (`k` = 1..=20,
//! [`FALLBACK_STEP`], [`FALLBACK_BLOCKS`]) at which all six are free. That
//! choice is deterministic for the host's state, uniform, logged, pinned in
//! the daemon's own environment for every child, and published in
//! `ports.json` with the daemon's pid. Clients resolve the offset in one
//! order ([`resolve_offset`]): `SUSI_PORT_OFFSET`, the explicit config key
//! (`SusiConfig::port_offset`), the live daemon's `ports.json`
//! ([`live_daemon_ports`]), then 0. The leaf-service ports (18080–18084)
//! move with the same offset but are not probed: a leaf that cannot bind is
//! the supervisor's to report.
//!
//! **9095 stays outside [`ALL`] on purpose.** External MCP/A2A clients
//! hard-code 9090–9094; 9092 already occupies the UDP slot as A2A discovery.
//! Swarm gossip is cluster-member traffic (`SWARM_UDP`), offset with the
//! instance, printed by `susi os`, and never a public client API.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

pub const GMCP: u16 = 9090;
pub const GEMI: u16 = 9091;
pub const UDP_DISCOVERY: u16 = 9092;
pub const GMCP_HTTP: u16 = 9093;
pub const A2A_HTTP: u16 = 9094;
/// Swarm-wide capability gossip. Not in [`ALL`]: 9092 stays A2A discovery.
pub const GOSSIP: u16 = 9095;

/// Leaf-service default ports (before the instance offset). `LEAF_SERVICES`
/// and every service client name these, so no two copies can drift.
pub const PATHS_SERVICE: u16 = 18080;
pub const ERROR_SERVICE: u16 = 18081;
pub const CONFIG_SERVICE: u16 = 18082;
pub const SANDBOX_SERVICE: u16 = 18083;
pub const NATIVE_SERVICE: u16 = 18084;

/// Stable host contract advertised to external clients.
pub const ALL: [(u16, &str); 5] = [
    (GMCP, "GMCP/MCP HTTP"),
    (GEMI, "GEMI HTTP"),
    (UDP_DISCOVERY, "A2A UDP discovery"),
    (GMCP_HTTP, "GMCP HTTP alias"),
    (A2A_HTTP, "A2A HTTP (JSON-RPC + SSE + agent card)"),
];

/// Swarm-internal UDP. Same offset as [`ALL`]; **not** the host contract.
pub const SWARM_UDP: [(u16, &str); 1] = [(GOSSIP, "swarm capability gossip")];

/// Canonical leaf-service loopback HTTP ports (`18080–18084`).
pub const LEAF_SERVICES: [(u16, &str); 5] = [
    (PATHS_SERVICE, "susi-paths"),
    (ERROR_SERVICE, "susi-error"),
    (CONFIG_SERVICE, "susi-config"),
    (SANDBOX_SERVICE, "susi-sandbox"),
    (NATIVE_SERVICE, "susi-native"),
];

/// Distance between two fallback blocks.
pub const FALLBACK_STEP: u16 = 100;
/// How many blocks past the canonical one the daemon tries (`k` = 1..=20).
pub const FALLBACK_BLOCKS: u16 = 20;

/// The environment variable that pins the offset.
pub const OFFSET_ENV: &str = "SUSI_PORT_OFFSET";

/// `SUSI_PORT_OFFSET` when it is set to a value that parses as a `u16`
/// (surrounding whitespace allowed): an operator-pinned offset, `0`
/// included. Anything else reads as unset.
#[must_use]
pub fn explicit_env_offset() -> Option<u16> {
    std::env::var(OFFSET_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
}

/// The instance offset for code without a `SusiConfig` in scope:
/// `SUSI_PORT_OFFSET`, else the offset the live daemon of this instance
/// published ([`live_daemon_ports`]), else 0. `SusiConfig::port_offset()`
/// adds the explicit config key between the two.
#[must_use]
pub fn env_port_offset() -> u16 {
    resolve_offset(explicit_env_offset(), None, live_daemon_offset)
}

/// The one resolution order every client uses: env, explicit config key,
/// live daemon, 0. `live` is consulted only when neither pin is present.
#[must_use]
pub fn resolve_offset(
    env: Option<u16>,
    config: Option<u16>,
    live: impl FnOnce() -> Option<u16>,
) -> u16 {
    env.or(config).or_else(live).unwrap_or(0)
}

/// `base + env_offset`, saturating — never produces a port above u16::MAX.
pub fn effective(base: u16) -> u16 {
    base.saturating_add(env_port_offset())
}

/// Pin `offset` for this process and every child it spawns, so each reader
/// of [`env_port_offset`] / `SusiConfig::port_offset` and each leaf service
/// sees the block the daemon actually binds.
///
/// The daemon calls this once at boot, after it has decided the block and
/// before it binds, starts a worker of its own or spawns a child.
/// Environment writes are only sound while no other thread reads the
/// environment through libc; every Rust reader goes through std's
/// environment lock, and the daemon has started no worker thread at that
/// point.
pub fn pin_process_offset(offset: u16) {
    std::env::set_var(OFFSET_ENV, offset.to_string());
}

/// What the live daemon of an instance published in its `ports.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LivePorts {
    /// The uniform offset the daemon serves the host contract at.
    pub offset: u16,
    /// The offset it was configured for (0 unless pinned); differs from
    /// `offset` exactly when the daemon fell back.
    pub fallback_from: u16,
    /// The daemon's pid, matching its live substrate lock.
    pub pid: u32,
}

impl LivePorts {
    /// Whether the daemon serves a fallback block, not the configured one.
    #[must_use]
    pub fn is_fallback(&self) -> bool {
        self.offset != self.fallback_from
    }
}

/// The live daemon's published ports for this instance. The directory comes
/// from the local resolver, never the paths service: that service's own
/// port depends on this answer.
#[must_use]
pub fn live_daemon_ports() -> Option<LivePorts> {
    live_ports_in(&crate::LocalDirs::config_dir())
}

/// [`live_daemon_ports`]' offset.
#[must_use]
pub fn live_daemon_offset() -> Option<u16> {
    live_daemon_ports().map(|p| p.offset)
}

/// How long a liveness verdict is reused while `ports.json` is unchanged.
const LIVENESS_TTL: Duration = Duration::from_secs(1);

/// One cached reading of `<dir>/ports.json`.
struct Reading {
    stamp: (Option<SystemTime>, u64),
    checked: Instant,
    live: Option<LivePorts>,
}

/// `<dir>/ports.json` when it was written by the daemon that holds the
/// substrate lock in `dir` right now; `None` for a missing, legacy (no pid)
/// or stale file.
///
/// Cached per directory on the file's mtime and length, and re-verified at
/// most once per [`LIVENESS_TTL`]: `susi start` polls this while the daemon
/// writes the file, so the answer is never pinned for the process.
#[must_use]
pub fn live_ports_in(dir: &Path) -> Option<LivePorts> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Reading>>> = OnceLock::new();
    let meta = std::fs::metadata(dir.join(crate::zc_ports_file::FILE_NAME)).ok()?;
    let stamp = (meta.modified().ok(), meta.len());
    let cache = CACHE.get_or_init(Mutex::default);
    if let Some(hit) = cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dir)
        .filter(|r| r.stamp == stamp && r.checked.elapsed() < LIVENESS_TTL)
    {
        return hit.live;
    }
    let live = crate::zc_ports_file::PortsFile::read_from_home(dir)
        .filter(|pf| daemon_lock_live(dir, pf.pid))
        .map(|pf| LivePorts {
            offset: pf.offset,
            fallback_from: pf.fallback_from,
            pid: pf.pid,
        });
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(
        dir.to_path_buf(),
        Reading {
            stamp,
            checked: Instant::now(),
            live,
        },
    );
    live
}

/// Whether `pid` is the daemon holding the substrate lock in `dir` now.
///
/// The daemon records its pid in `substrate.lock` and mirrors it to the
/// unlocked `substrate.pid`; the recorded pid must be `pid`. On Windows the
/// daemon keeps a byte-range lock on `substrate.lock` for its whole life,
/// which the OS enforces against every other handle and drops when the
/// process ends, crash included, so a read failing with
/// `ERROR_LOCK_VIOLATION` is the proof of life. On Linux `/proc/<pid>/stat`
/// must show a state other than zombie or dead. Other unixes (not release
/// targets) accept the recorded pid.
#[must_use]
pub fn daemon_lock_live(dir: &Path, pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let recorded = recorded_pid(&dir.join("substrate.pid"))
        .or_else(|| recorded_pid(&dir.join("substrate.lock")));
    recorded == Some(pid) && lock_holder_alive(dir, pid)
}

fn recorded_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

#[cfg(windows)]
fn lock_holder_alive(dir: &Path, _pid: u32) -> bool {
    const ERROR_LOCK_VIOLATION: i32 = 33;
    match std::fs::read(dir.join("substrate.lock")) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(ERROR_LOCK_VIOLATION),
    }
}

#[cfg(target_os = "linux")]
fn lock_holder_alive(_dir: &Path, pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit(')').next().map(str::to_string))
        .and_then(|tail| tail.trim().chars().next())
        .is_some_and(|state| !matches!(state, 'Z' | 'X'))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn lock_holder_alive(_dir: &Path, _pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn canonical_port_tables_have_zero_collisions_and_non_empty_labels() {
        let mut seen = BTreeSet::new();
        for (port, label) in ALL
            .iter()
            .chain(SWARM_UDP.iter())
            .chain(LEAF_SERVICES.iter())
        {
            assert!(!label.trim().is_empty(), "port {port} must have a label");
            assert!(
                seen.insert(*port),
                "duplicate canonical port {port} ({label}) across ALL / SWARM_UDP / LEAF_SERVICES"
            );
        }
        assert_eq!(
            seen.len(),
            ALL.len() + SWARM_UDP.len() + LEAF_SERVICES.len()
        );
        assert_eq!(LEAF_SERVICES[0], (18080, "susi-paths"));
        assert_eq!(LEAF_SERVICES[4], (18084, "susi-native"));
    }

    #[test]
    fn effective_port_saturates_at_u16_max() {
        // Two reads of the env offset must not straddle another test's write.
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            u16::MAX.saturating_add(env_port_offset()),
            effective(u16::MAX)
        );
        assert!(effective(GMCP) >= GMCP);
    }

    #[test]
    fn env_offset_parses_trimmed_u16_and_ignores_everything_else() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // An unusable env value reads as unset and falls through to the live
        // daemon's published offset, so the case table is only deterministic
        // under a private empty home that has no daemon (a machine running a
        // daemon on offset 100 otherwise answers 100 for "abc").
        let home = std::env::temp_dir().join(format!("susi_ports_env_{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let mut guard = crate::test_env::EnvGuard::isolated();
        guard.set("HOME", &home);
        let cases: [(Option<&str>, u16); 8] = [
            (Some("100"), 100),
            (Some(" 250 "), 250),
            (Some("65535"), 65535),
            (Some("abc"), 0),
            (Some("-5"), 0),
            (Some("65536"), 0),
            (Some(""), 0),
            (None, 0),
        ];
        let mut observed = Vec::new();
        for (value, _) in cases {
            match value {
                Some(v) => std::env::set_var("SUSI_PORT_OFFSET", v),
                None => std::env::remove_var("SUSI_PORT_OFFSET"),
            }
            observed.push((env_port_offset(), effective(GMCP), effective(u16::MAX)));
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&home);
        for ((value, expected), (offset, gmcp, max)) in cases.iter().zip(observed) {
            assert_eq!(offset, *expected, "{value:?}");
            assert_eq!(gmcp, GMCP.saturating_add(*expected), "{value:?}");
            assert_eq!(max, u16::MAX, "{value:?} must saturate");
        }
    }

    #[test]
    fn gossip_is_swarm_internal_and_never_part_of_the_public_contract() {
        assert_eq!(GOSSIP, 9095);
        assert!(SWARM_UDP.iter().any(|(p, _)| *p == GOSSIP));
        assert!(ALL.iter().all(|(p, _)| *p != GOSSIP));
        let public: Vec<u16> = ALL.iter().map(|(p, _)| *p).collect();
        assert_eq!(public, vec![9090, 9091, 9092, 9093, 9094]);
    }

    #[test]
    fn canonical_constants_match_documented_port_numbers_and_ordering() {
        assert_eq!(GMCP, 9090);
        assert_eq!(GEMI, 9091);
        assert_eq!(UDP_DISCOVERY, 9092);
        assert_eq!(GMCP_HTTP, 9093);
        assert_eq!(A2A_HTTP, 9094);
        assert_eq!(GOSSIP, 9095);
        assert_eq!(PATHS_SERVICE, 18080);
        assert_eq!(ERROR_SERVICE, 18081);
        assert_eq!(CONFIG_SERVICE, 18082);
        assert_eq!(SANDBOX_SERVICE, 18083);
        assert_eq!(NATIVE_SERVICE, 18084);
    }

    #[test]
    fn offset_resolution_order_is_env_then_config_then_live_daemon_then_zero() {
        let never = || -> Option<u16> { panic!("a pinned offset must not read the live daemon") };
        assert_eq!(resolve_offset(Some(300), Some(200), never), 300);
        assert_eq!(
            resolve_offset(Some(0), Some(200), never),
            0,
            "env 0 is a pin"
        );
        assert_eq!(resolve_offset(None, Some(200), never), 200);
        assert_eq!(resolve_offset(None, None, || Some(100)), 100);
        assert_eq!(resolve_offset(None, None, || None), 0);
    }

    #[test]
    fn fallback_blocks_stay_inside_the_port_space() {
        let last = FALLBACK_STEP * FALLBACK_BLOCKS;
        assert_eq!(last, 2000);
        assert!(GOSSIP.checked_add(last).is_some());
        assert!(NATIVE_SERVICE.checked_add(last).is_some());
    }

    #[test]
    fn explicit_env_offset_is_a_pin_only_when_it_parses() {
        let _g = crate::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os(OFFSET_ENV);
        let mut observed = Vec::new();
        for value in [Some("0"), Some(" 100 "), Some("abc"), None] {
            match value {
                Some(v) => std::env::set_var(OFFSET_ENV, v),
                None => std::env::remove_var(OFFSET_ENV),
            }
            observed.push(explicit_env_offset());
        }
        pin_process_offset(400);
        observed.push(explicit_env_offset());
        match previous {
            Some(v) => std::env::set_var(OFFSET_ENV, v),
            None => std::env::remove_var(OFFSET_ENV),
        }
        assert_eq!(observed, vec![Some(0), Some(100), None, None, Some(400)]);
    }
}
