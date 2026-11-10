//! Always-On SUSI Substrate Daemon Process Manager
//!
//! External clients may hard-code these ports — the daemon must never drift them:
//! - **9090** GMCP / MCP HTTP
//! - **9091** GEMI HTTP
//! - **9092** A2A UDP discovery
//! - **9093** GMCP HTTP (streamable / SSE alias)
//! - **9094** A2A HTTP (JSON-RPC / + SSE /stream + public agent card)

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::net::{TcpStream, UdpSocket};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

#[cfg(windows)]
use signal_hook::consts::{SIGINT, SIGTERM};
#[cfg(not(windows))]
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::Signals,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tracing::{info, warn};

use crate::susi_error::EaiError;
use crate::susi_sandbox::manager::SusiConfig;
use susi_gmcp::server::GmcpServer;
use susi_server::GemiServer;

pub struct SusiDaemon;

/// The supervised one-shot that registers the providers the host contract
/// answers with, right after the bind.
const CONTRACT_PROVIDERS: &str = "contract-providers";

/// Preserve the supervision cadence while letting shutdown interrupt its idle
/// wait. The flag is shared with the signal handler on every native platform.
fn wait_for_shutdown(shutdown: &AtomicBool, duration: Duration) {
    let start = Instant::now();
    while !shutdown.load(Ordering::Acquire) {
        let remaining = duration.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

/// What `susi stop` actually accomplished — the CLI must exit non-zero when
/// the host contract did not release, instead of printing success for a
/// daemon that is still listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopOutcome {
    /// Processes signalled (lock holder, Linux orphans, contract-port holders).
    pub signalled: usize,
    /// The host-contract TCP ports were observed released before the deadline.
    pub released: bool,
}

pub struct DaemonContext {
    pub shutdown_signal: Arc<AtomicBool>,
    _lock: DaemonLock,
}

/// The running daemon's shutdown flag, so an in-process request (the MCP
/// `daemon_shutdown` tool) can stop the main loop the way SIGTERM does.
static SHUTDOWN_SIGNAL: std::sync::OnceLock<Arc<AtomicBool>> = std::sync::OnceLock::new();

/// Ask the daemon running in this process to shut down gracefully (its main
/// loop exits and stops its children, as on SIGTERM). Returns `false` when
/// no daemon runs in this process.
pub fn request_shutdown() -> bool {
    match SHUTDOWN_SIGNAL.get() {
        Some(signal) => {
            signal.store(true, Ordering::Release);
            true
        }
        None => false,
    }
}

/// Whether a daemon runs in this process (see [`request_shutdown`]).
pub fn daemon_in_process() -> bool {
    SHUTDOWN_SIGNAL.get().is_some()
}

impl DaemonContext {
    pub fn new(lock: DaemonLock) -> Self {
        let shutdown_signal = Arc::new(AtomicBool::new(false));
        let _ = SHUTDOWN_SIGNAL.set(Arc::clone(&shutdown_signal));
        DaemonContext {
            shutdown_signal,
            _lock: lock,
        }
    }

    pub fn setup_signal_handlers(&self) -> Result<(), EaiError> {
        let shutdown = Arc::clone(&self.shutdown_signal);
        // signal-hook's blocking `Signals` iterator is unix-only; on Windows
        // the console SIGINT/SIGTERM land through the flag backend, which
        // needs no dedicated worker thread.
        #[cfg(windows)]
        {
            let _ = signal_hook::flag::register(SIGINT, Arc::clone(&shutdown));
            let _ = signal_hook::flag::register(SIGTERM, shutdown);
        }
        // Supervised like every other long-lived worker: a signal-handler
        // thread that dies would leave the daemon deaf to SIGTERM.
        #[cfg(not(windows))]
        crate::service_supervision::Supervisor::global().spawn(
            "signal-handler",
            3,
            None,
            move || {
                let shutdown = Arc::clone(&shutdown);
                Some(move || {
                    if let Ok(mut signals) = Signals::new([SIGTERM, SIGINT]) {
                        for sig in signals.forever() {
                            let msgs = crate::susi_sandbox::manager::SusiMessages::load_global();
                            let def_msg = "[SusiDaemon] Received signal: {}".to_string();
                            let msg = msgs.get("daemon", "signal_received").unwrap_or(&def_msg);
                            eprintln!("{}", msg.replace("{}", &sig.to_string()));
                            shutdown.store(true, Ordering::Release);
                        }
                    }
                })
            },
        );
        Ok(())
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_signal.load(Ordering::Acquire)
    }
}

/// A running host daemon. Always bound to [`susi_paths::SusiDirs::substrate_home`];
/// project context is per-request cwd, never the daemon's boot path.
pub struct RunningDaemon {
    pub pid: u32,
    /// Substrate home recorded in the lock (diagnostic / identity only).
    pub substrate_home: PathBuf,
}

pub struct DaemonLock {
    file: fs::File,
    /// The `substrate.lock` path, so the pid record can be mirrored to the
    /// unlocked sidecar readers fall back to.
    path: PathBuf,
}

impl DaemonLock {
    #[allow(unsafe_code)]
    fn acquire(path: &Path) -> Result<Self, String> {
        // No .truncate(true): a contending acquire must not wipe the
        // holder's pid/substrate lines before flock refuses it — the
        // empty file would make check_status report "not running" for a
        // live daemon. write_pid truncates explicitly once the lock is
        // ours.
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| e.to_string())?;

        #[cfg(unix)]
        {
            // A flock lives on the open file description — a child caught
            // between fork and exec still shares our fds (CLOEXEC closes
            // at execve), so a sibling process spawning children in the
            // same process can present a *transient* EWOULDBLOCK that is
            // fork-window noise, not a real contender. A genuine lock
            // holder never releases in a few hundred ms; retry briefly
            // before declaring contention.
            // SAFETY: We just opened the file, so fd is valid. flock
            // doesn't access memory unsafely or take ownership.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
            loop {
                // SAFETY: flock on a valid fd owned by `file`, which outlives the call; no memory is passed.
                let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
                if ret == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                // EWOULDBLOCK == EAGAIN on unix.
                let transient = matches!(
                    err.raw_os_error(),
                    Some(libc::EINTR) | Some(libc::EWOULDBLOCK)
                );
                if transient && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                    continue;
                }
                return Err(format!("Locked by another process ({err})"));
            }
        }

        #[cfg(windows)]
        {
            use winapi::um::fileapi::LockFileEx;
            use winapi::um::minwinbase::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY};

            let handle = file.as_raw_handle();
            // SAFETY: OVERLAPPED is a plain C struct for which all-zero is the documented initial state.
            let mut overlapped = unsafe { std::mem::zeroed() };
            // SAFETY: `handle` is a valid open file handle owned by `file` and `overlapped` lives across the call.
            let ret = unsafe {
                LockFileEx(
                    handle as _,
                    LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                    0,
                    1,
                    0,
                    &mut overlapped,
                )
            };
            if ret == 0 {
                return Err("Locked by another process".into());
            }
        }

        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Writes `pid\nsubstrate_home`. Second line is host identity only —
    /// never a project cwd. Mirrors the same record into the unlocked
    /// sidecar: Windows byte-range locks are enforced against every other
    /// handle (even inside the same process), so a pid written only into
    /// the locked file is unreadable to `check_status`, which is how `stop`
    /// once reported a daemon it could not even identify.
    fn write_pid(&mut self, substrate_home: &Path) -> Result<(), String> {
        let record = format!("{}\n{}", std::process::id(), substrate_home.display());
        self.file.set_len(0).map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        write!(self.file, "{record}").map_err(|e| e.to_string())?;
        self.file.flush().map_err(|e| e.to_string())?;
        fs::write(lock_record_path(&self.path), record).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Unlocked mirror of the daemon lock record (`substrate.lock` → `substrate.pid`).
fn lock_record_path(lock_file_path: &Path) -> PathBuf {
    lock_file_path.with_extension("pid")
}

/// Read the daemon record, preferring the locked file and falling back to
/// its unlocked sidecar (the Windows-mandatory-lock case above).
fn read_lock_record(lock_file_path: &Path) -> Option<String> {
    fs::read_to_string(lock_file_path)
        .ok()
        .or_else(|| fs::read_to_string(lock_record_path(lock_file_path)).ok())
}

/// Remove both the lock and its readable mirror, and the `ports.json` its
/// daemon published unless a live daemon wrote it: a stopped or crashed
/// daemon must not keep claiming a port block.
fn remove_lock_files(lock_file_path: &Path) {
    let _ = fs::remove_file(lock_file_path);
    let _ = fs::remove_file(lock_record_path(lock_file_path));
    if let Some(dir) = lock_file_path.parent() {
        susi_paths::zc_ports_file::PortsFile::remove_unless_live(dir);
    }
}

impl SusiDaemon {
    pub fn get_lock_file(global_dir: &Path) -> PathBuf {
        global_dir.join("substrate.lock")
    }

    pub fn check_status(_workspace: &Path, global_dir: &Path) -> Option<u32> {
        let global_lock = Self::get_lock_file(global_dir);
        Self::check_status_path(&global_lock)
    }

    /// The four TCP ports that make up the host contract (canonical
    /// 9090–9094 shifted by any `port_offset`). UDP discovery (9092) is
    /// separate — it answers, it does not listen.
    pub fn host_contract_tcp_ports() -> [u16; 4] {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        [
            cfg.gmcp_port(),
            cfg.gemi_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port(),
        ]
    }

    /// True when the public host-contract TCP surfaces (canonical 9090-9094
    /// plus any `port_offset`) accept connections.
    pub fn host_contract_tcp_ready() -> bool {
        Self::tcp_ports_listening(&Self::host_contract_tcp_ports())
    }

    /// True when every port in `ports` accepts a loopback connection.
    fn tcp_ports_listening(ports: &[u16]) -> bool {
        ports.iter().all(|port| {
            TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], *port)),
                Duration::from_millis(200),
            )
            .is_ok()
        })
    }

    /// True when UDP discovery answers an authenticated probe (daemon owns
    /// the port). The probe is a cluster-key signed ping; only a responder
    /// holding the same `cluster.key` can echo our nonce in a signed pong,
    /// so a foreign process squatting on the port never reads as ready.
    /// Unsigned pings get no answer (the legacy unauthenticated dialect was
    /// removed).
    ///
    /// Without a usable `cluster.key` (malformed, or not creatable) the
    /// daemon still serves the host contract in unsigned mode, so readiness
    /// falls back to a key-independent bound-port test (`udp_port_bound`):
    /// cluster-membership key material never gates single-host start. The
    /// missing key is reported once per process, and not re-read after.
    pub fn host_contract_udp_ready() -> bool {
        static KEY_UNUSABLE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.udp_discovery_port()));
        let wait = Duration::from_millis(300);
        // Zero bloom and no checksum: the probe registers nothing, and a
        // loopback sender is never admitted as a peer (self-edge guard).
        let signed = if KEY_UNUSABLE.get().is_some() {
            None
        } else {
            crate::susi_config::cluster_key::signed_ping_v3("CORE", 0, "0000000000000000", target)
        };
        let Some((ping, nonce)) = signed else {
            KEY_UNUSABLE.get_or_init(|| {
                eprintln!(
                    "[SUSI Daemon] UDP discovery probe: no usable cluster.key (unsigned mode);                      readiness falls back to a bound-port check"
                );
            });
            return udp_port_bound(target, wait);
        };
        let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
            return false;
        };
        let _ = sock.set_read_timeout(Some(wait));
        if sock.send_to(ping.as_bytes(), target).is_err() {
            return false;
        }
        // 4 KiB: a signed pong carrying a gossip roster runs past 1 KiB.
        let mut buf = [0u8; 4096];
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            match sock.recv_from(&mut buf) {
                Ok((n, _)) => {
                    let msg = String::from_utf8_lossy(&buf[..n]);
                    if crate::susi_config::cluster_key::verify_signed_pong(&msg, &nonce).is_some() {
                        return true;
                    }
                }
                Err(_) => return false,
            }
        }
        false
    }

    /// Full host contract: the four TCP surfaces + UDP discovery, at
    /// canonical base 9090–9094 plus any `port_offset`.
    pub fn host_contract_ready() -> bool {
        Self::host_contract_tcp_ready() && Self::host_contract_udp_ready()
    }

    /// Block until host-contract ports are live, or `timeout` elapses.
    pub fn wait_for_host_contract(timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if Self::host_contract_ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        Self::host_contract_ready()
    }

    /// Human-readable host-contract endpoints for CLI / logs.
    pub fn host_contract_endpoints_report() -> String {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let bind = cfg
            .get::<String>("bind_address")
            .unwrap_or_else(|| "127.0.0.1".to_string());
        // Every socket sniffs per-connection: http and https are both live
        // whenever a cert exists. The report names the external surface only
        // when the bind actually exposes one.
        let remote_note = if crate::tls::is_loopback(&bind) {
            String::new()
        } else {
            let _ = crate::tls::remote_tls_required(&cfg);
            format!(
                "
Remote surface: https on {bind}:* (TLS auto-provisioned; remote plaintext                  refused; Susi machines use the sealed peer channel)"
            )
        };
        format!(
            "Host contract endpoints:\n\
             - GMCP/MCP  http://127.0.0.1:{}/mcp\n\
             - GEMI      http://127.0.0.1:{}/\n\
             - UDP disco 127.0.0.1:{}\n\
             - GMCP alias http://127.0.0.1:{}/mcp\n\
             - A2A       http://127.0.0.1:{}/{}{}",
            cfg.gmcp_port(),
            cfg.gemi_port(),
            cfg.udp_discovery_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port(),
            remote_note,
            Self::offset_note(cfg.port_offset(), susi_paths::ports::live_daemon_ports())
        )
    }

    /// The endpoint report's offset line: nothing for the canonical block,
    /// the offset otherwise, and, when the live daemon fell back, that it
    /// did and how to pin a block instead.
    fn offset_note(offset: u16, live: Option<susi_paths::ports::LivePorts>) -> String {
        match live.filter(|l| l.offset == offset && l.is_fallback()) {
            Some(l) => format!(
                "\nport_offset={offset} (automatic fallback: the configured block at offset {} \
                 was held by another process; pin a block with SUSI_PORT_OFFSET)",
                l.fallback_from
            ),
            None if offset == 0 => String::new(),
            None => format!("\nport_offset={offset} (canonical base 9090–9094)"),
        }
    }

    /// Locate the single host daemon, if running.
    pub fn find_running_daemon(global_dir: &Path) -> Option<RunningDaemon> {
        let global_lock = Self::get_lock_file(global_dir);
        Self::check_status_path(&global_lock).map(|pid| {
            let substrate_home = Self::read_recorded_substrate_home(&global_lock)
                .unwrap_or_else(susi_paths::SusiDirs::substrate_home);
            RunningDaemon {
                pid,
                substrate_home,
            }
        })
    }

    fn read_recorded_substrate_home(lock_file_path: &Path) -> Option<PathBuf> {
        let content = read_lock_record(lock_file_path)?;
        let mut lines = content.lines();
        lines.next()?; // PID line
        lines.next().map(PathBuf::from)
    }

    #[allow(unsafe_code)]
    pub fn check_status_path(lock_file_path: &Path) -> Option<u32> {
        if !lock_file_path.exists() {
            return None;
        }

        if let Some(content) = read_lock_record(lock_file_path)
            && let Ok(pid) = content.lines().next().unwrap_or("").trim().parse::<u32>()
        {
            #[cfg(unix)]
            {
                // SAFETY: kill(pid, 0) sends no signal; it only probes whether the pid exists.
                if unsafe { libc::kill(pid as i32, 0) } == 0 {
                    return Some(pid);
                } else {
                    remove_lock_files(lock_file_path);
                    return None;
                }
            }
            #[cfg(windows)]
            {
                if Self::is_process_alive(lock_file_path) {
                    return Some(pid);
                } else {
                    remove_lock_files(lock_file_path);
                    return None;
                }
            }
        }
        None
    }

    #[allow(unsafe_code)]
    fn is_process_alive(lock_file_path: &Path) -> bool {
        let Some(content) = read_lock_record(lock_file_path) else {
            return false;
        };
        let Ok(pid) = content.lines().next().unwrap_or("").trim().parse::<u32>() else {
            return false;
        };
        Self::process_alive(pid)
    }

    /// Whether a bare pid names a live process.
    ///
    /// Windows asks the process object itself
    /// ([`process_probe::pid_alive`](crate::susi_config::process_probe::pid_alive)).
    /// It used to spawn `tasklist` per call — a process launch on every tick
    /// of a stop's wait, and one whose exit status is 0 whether or not the
    /// pid matched. A pid the probe cannot judge reads as gone, as a
    /// `tasklist` that could not run did. One answer differs: a process that
    /// exited with code 259 (`STILL_ACTIVE`) cannot be told from a running
    /// one and reads as alive (the probe's documented conservative side), so
    /// a stop waiting on it runs to its timeout. No susi process exits 259.
    ///
    /// On Linux a killed-but-unreaped child is a zombie: `kill(pid, 0)`
    /// succeeds for it, so the probe read a terminated process as alive and a
    /// stop could never confirm its own work. The state field of
    /// `/proc/<pid>/stat` is the authority there, and `Z`/`X` mean dead.
    #[allow(unsafe_code)]
    pub fn process_alive(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        #[cfg(unix)]
        {
            #[cfg(target_os = "linux")]
            {
                if let Some(state) = Self::linux_process_state(pid) {
                    return state != 'Z' && state != 'X';
                }
            }
            // SAFETY: kill(pid, 0) sends no signal; it only probes whether the pid exists.
            unsafe { libc::kill(pid as i32, 0) == 0 }
        }
        #[cfg(windows)]
        {
            crate::susi_config::process_probe::pid_alive(pid).unwrap_or(false)
        }
    }

    /// The one-letter state field of `/proc/<pid>/stat`, or `None` when the
    /// pid cannot be read (it has been reaped, or this is not Linux).
    ///
    /// The line is `pid (comm) state …`, and `comm` may itself contain
    /// parentheses, so the state is whatever follows the *last* `)`.
    #[cfg(target_os = "linux")]
    fn linux_process_state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = stat.get(stat.rfind(')')? + 1..)?;
        after.split_whitespace().next()?.chars().next()
    }

    /// Parse `tasklist /FO CSV /NH` for `pid`, returning its image name when
    /// present. A no-match line is `INFO: No tasks are running …`, which has
    /// no numeric pid field, so it cannot masquerade as a live process.
    #[cfg(any(windows, test))]
    fn tasklist_image_name(stdout: &[u8], pid: u32) -> Option<String> {
        String::from_utf8_lossy(stdout).lines().find_map(|line| {
            let mut fields = line.split(',');
            let image = fields.next()?.trim().trim_matches('"').to_string();
            let got = fields
                .next()?
                .trim()
                .trim_matches('"')
                .parse::<u32>()
                .ok()?;
            (got == pid && !image.is_empty()).then_some(image)
        })
    }

    /// Parse `netstat -ano -p tcp` output for the pids LISTENING on `ports`.
    /// The local address column ends `:port`; a matching foreign address or a
    /// non-LISTENING row never counts.
    #[cfg(any(windows, test))]
    fn parse_netstat_listeners(stdout: &[u8], ports: &[u16]) -> Vec<u32> {
        susi_core::service_table::parse_netstat_owners(stdout, ports, false)
    }

    /// Pids listening on `ports`, when the OS can name them.
    fn port_holder_pids(ports: &[u16]) -> Vec<u32> {
        #[cfg(windows)]
        {
            susi_paths::console::background_command("netstat")
                .args(["-ano", "-p", "tcp"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .map(|out| Self::parse_netstat_listeners(&out.stdout, ports))
                .unwrap_or_default()
        }
        #[cfg(not(windows))]
        {
            ports
                .iter()
                .filter_map(|port| susi_core::service_table::pid_for_port(*port))
                .collect()
        }
    }

    /// True when `pid` is a SUSI binary (its image/exe name starts `susi`).
    /// The port-holder sweep only ever force-kills such processes: a foreign
    /// program squatting on a contract port must be reported, never killed.
    fn is_susi_process(pid: u32) -> bool {
        #[cfg(windows)]
        {
            susi_paths::console::background_command("tasklist")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .and_then(|out| Self::tasklist_image_name(&out.stdout, pid))
                .is_some_and(|image| image.to_ascii_lowercase().starts_with("susi"))
        }
        #[cfg(unix)]
        {
            // Linux: the kernel's /proc/<pid>/exe link; macOS and other
            // unixes: the executable sysinfo reports.
            crate::susi_config::process_probe::exe_path(pid)
                .and_then(|exe| {
                    exe.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                })
                .is_some_and(|name| name.starts_with("susi"))
        }
    }

    /// Contract-port listeners that are SUSI processes — the orphan a
    /// previous daemon left behind after its lock was removed. Their tree is
    /// force-killed so `susi stop` reclaims the ports on every platform.
    fn susi_port_holders(ports: &[u16]) -> Vec<u32> {
        Self::port_holder_pids(ports)
            .into_iter()
            .filter(|pid| Self::is_susi_process(*pid))
            .collect()
    }

    pub fn get_hash_file(global_dir: &Path) -> PathBuf {
        global_dir.join("binary.hash")
    }

    /// The `daemon-start` command every spawn path runs: host-scoped, stdio
    /// detached, and started *in* the substrate home. A spawn without a
    /// working directory inherited the cwd of whichever command started the
    /// daemon — an agent's worktree, typically — so the daemon's
    /// cwd-relative work (the intent ladder's repo root) wrote task files
    /// into that worktree, and on Windows the daemon's cwd handle kept the
    /// worktree from being removed.
    pub fn daemon_command(bin: &Path, substrate_home: &Path) -> Command {
        use std::process::Stdio;
        let mut cmd = Command::new(bin);
        cmd.arg("daemon-start")
            .arg("--workspace")
            .arg(substrate_home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // A home that does not exist (yet) must not fail the spawn itself —
        // a missing cwd is an OS spawn error. The daemon creates and enters
        // its home on boot either way (`run_daemon_loop`).
        if substrate_home.is_dir() {
            cmd.current_dir(substrate_home);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        cmd
    }

    /// Spawn a detached background command without letting `CreateProcessW`'s
    /// `bInheritHandles = TRUE` (enabled by `Stdio::null()`) copy the parent
    /// CLI's own stdin/stdout/stderr pipe handles into the long-lived daemon.
    /// Without clearing `HANDLE_FLAG_INHERIT` on the parent's std handles
    /// around `spawn()`, any caller reading `susi`'s stdout/stderr pipe
    /// (`Command::output()`, release smoke missions, `| Out-Null`) blocks
    /// forever waiting for EOF after the CLI exits whenever `susi` started or
    /// restarted the daemon.
    #[allow(unsafe_code)]
    pub fn spawn_without_inheriting_stdio(
        cmd: &mut Command,
    ) -> std::io::Result<std::process::Child> {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            use winapi::shared::minwindef::DWORD;
            use winapi::um::handleapi::{GetHandleInformation, SetHandleInformation};
            use winapi::um::winnt::HANDLE;

            const HANDLE_FLAG_INHERIT: DWORD = 0x0000_0001;
            let raw_handles: [HANDLE; 3] = [
                std::io::stdin().as_raw_handle() as HANDLE,
                std::io::stdout().as_raw_handle() as HANDLE,
                std::io::stderr().as_raw_handle() as HANDLE,
            ];
            let mut restore: Vec<(HANDLE, DWORD)> = Vec::with_capacity(3);
            for handle in raw_handles {
                if handle.is_null() {
                    continue;
                }
                let mut flags: DWORD = 0;
                // SAFETY: `GetHandleInformation` and `SetHandleInformation`
                // inspect and toggle the `HANDLE_FLAG_INHERIT` bit on the
                // current process's standard handles; invalid or pseudo-handles
                // simply return FALSE and are skipped.
                unsafe {
                    if GetHandleInformation(handle, &mut flags) != 0
                        && (flags & HANDLE_FLAG_INHERIT) != 0
                    {
                        let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
                        restore.push((handle, flags & HANDLE_FLAG_INHERIT));
                    }
                }
            }
            let result = cmd.spawn();
            for (handle, orig_inherit) in restore {
                // SAFETY: restores the original `HANDLE_FLAG_INHERIT` bit on
                // the same valid standard handle captured above.
                unsafe {
                    let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, orig_inherit);
                }
            }
            result
        }
        #[cfg(not(windows))]
        {
            cmd.spawn()
        }
    }

    /// Compares the running binary's SHA-256 against the digest last written to
    /// `binary.hash` by `susi admin sync` (src/daemon/admin.rs). Both readers and
    /// writers of this file must agree on one signature format — a `len:mtime`
    /// shortcut here previously diverged from admin.rs's SHA-256 writer, causing
    /// a spurious mismatch (self-healing only after one overwrite cycle). The
    /// `current_sig` computed below is still always a real SHA-256 of the exact
    /// same format; `calculate_binary_hash_cached` only skips *re-computing* it
    /// when the binary provably hasn't changed (see its own doc comment) — it
    /// never substitutes a cheaper, differently-shaped signature.
    /// Same digest as `calculate_binary_hash`, but skips re-reading and
    /// re-hashing the binary (measured ~130ms for this project's real
    /// ~120MB `susi` release binary) when its mtime+size match a
    /// small sidecar cache from the last time this exact path was hashed —
    /// entirely defeating Mandate 4's sub-2ms client reflex on every single
    /// CLI invocation otherwise, since `ensure_daemon_running` calls this on
    /// the hot "daemon already running, nothing to do" path. Stores nanosecond
    /// mtime (not whole-second, to avoid a same-second rebuild+rerun false
    /// cache hit) + size + the real hash in `binary.hash.cache`, next to but
    /// distinct from `binary.hash` itself — the latter's writer/format
    /// contract with `susi admin sync` (see `verify_binary_integrity`'s doc
    /// comment) is untouched; a cache miss or parse failure always falls
    /// back to a real, fresh hash.
    /// Ensure the single host daemon is running, bound to
    /// [`susi_paths::SusiDirs::substrate_home`]. The caller's project cwd is
    /// irrelevant here — work context is attached per intent, not to the daemon.
    #[allow(unsafe_code)]
    pub fn ensure_daemon_running(_caller_cwd: &Path, global_dir: &Path) {
        let substrate_home = susi_paths::SusiDirs::substrate_home();
        let _ = std::fs::create_dir_all(&substrate_home);

        let current_exe = std::env::current_exe().ok();
        let bin_name = if cfg!(target_os = "windows") {
            "bin/susi.exe"
        } else {
            "bin/susi"
        };
        let global_bin = global_dir.join(bin_name);
        // Host contract: prefer the canonical ~/.susi/bin/susi. The
        // current_exe fallback must resolve to a *live* inode — after an
        // in-place replacement it reports a `… (deleted)` path that
        // Command::new fails ENOENT on, leaving the daemon unstartable.
        let bin_to_run = if global_bin.exists() {
            global_bin.clone()
        } else if let Some(exe) = crate::supervisor::reexec_path() {
            exe
        } else if let Some(ref exe) = current_exe {
            exe.clone()
        } else {
            PathBuf::from(if cfg!(target_os = "windows") {
                "susi.exe"
            } else {
                "susi"
            })
        };

        let msgs = crate::susi_sandbox::manager::SusiMessages::load_global();
        if let Some(running) = Self::find_running_daemon(global_dir) {
            if let Ok(false) =
                crate::susi_sandbox::daemon_state::SusiDaemonState::verify_binary_integrity(
                    &bin_to_run,
                    global_dir,
                )
            {
                // Daemon is always home-scoped, so any CLI may safely
                // restart a stale binary — there is no foreign workspace
                // ownership to protect.
                let def_recompiled =
                    "[SusiDaemon] Binary recompiled. Restarting daemon PID {}...".to_string();
                let msg = msgs
                    .get("daemon", "binary_recompiled")
                    .unwrap_or(&def_recompiled);
                info!("{}", msg.replace("{}", &running.pid.to_string()));
                Self::stop_daemon(&substrate_home, global_dir);
                // SIGTERM is async — spawning immediately races the old
                // daemon's port release; a new daemon that binds while the
                // contract ports are still held dies on EADDRINUSE and nothing
                // retries, leaving the host contract permanently down
                // (observed live). Wait for release before spawning.
                let deadline = Instant::now() + Duration::from_secs(10);
                while Self::host_contract_tcp_ready() && Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(100));
                }
            } else {
                return;
            }
        }

        // Binary Integrity Check
        match crate::susi_sandbox::daemon_state::SusiDaemonState::verify_binary_integrity(
            &bin_to_run,
            global_dir,
        ) {
            Ok(true) => {
                let def_verified = "[SusiDaemon] Binary integrity verified.".to_string();
                info!(
                    "{}",
                    msgs.get("daemon", "binary_verified")
                        .unwrap_or(&def_verified)
                );
            }
            Ok(false) => {
                // Hash file is self-healing: Ok(false) means the binary changed
                // (rebuild/hot-reload) and the trust anchor was rewritten — not
                // a refused tamper. Real refusal would leave the old hash.
                let def_updated =
                    "[SusiDaemon] Binary signature changed; trust anchor updated.".to_string();
                info!(
                    "{}",
                    msgs.get("daemon", "binary_updated").unwrap_or(&def_updated)
                );
            }
            Err(e) => warn!("[SusiDaemon] Could not verify binary integrity: {}", e),
        }

        #[cfg(all(unix, target_os = "linux"))]
        {
            // Prefer systemd's transient-service path on Linux: the daemon
            // lands in a real cgroup with MemoryMax/CPUQuota caps applied,
            // instead of an unconstrained detached process. `systemd-run`
            // exits non-zero when the user manager bus is unreachable (WSL,
            // containers, pre-linger SSH), in which case we fall through to
            // the portable double-fork path below.
            //
            // Caps scale with the host: a fixed 2G/50%-of-one-core budget
            // (sized for a minimal daemon) makes local inference impossible —
            // a single 7B Q4 model needs ~5G, and the leaf services + the
            // swarm's worker threads share the same cgroup. Floor at 8G/200%, then a quarter
            // of RAM and half the cores, so runaway stays bounded without
            // starving real workloads.
            let total_ram_bytes = std::fs::read_to_string("/proc/meminfo")
                .ok()
                .and_then(|t| {
                    t.lines().find(|l| l.starts_with("MemTotal")).and_then(|l| {
                        l.split_whitespace()
                            .nth(1)
                            .and_then(|v| v.parse::<u64>().ok())
                    })
                })
                .map(|kb| kb * 1024)
                .unwrap_or(0);
            let mem_cap = (total_ram_bytes / 4).max(8 * 1024 * 1024 * 1024);
            let cores = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1);
            let cpu_quota = (cores * 50).max(200);
            // `/proc/self/exe` only resolves to OUR binary when the child
            // inherits our address space (fork/exec). systemd-run hands the
            // path to the user manager, whose spawn helper resolves
            // /proc/self/exe to *itself* — the unit exits instantly. The
            // systemd path needs a concrete, canonicalized path.
            let unit_bin =
                std::fs::canonicalize(&bin_to_run).unwrap_or_else(|_| bin_to_run.clone());
            let mut run = Command::new("systemd-run");
            run.args(["--user", "--collect", "--quiet"])
                .arg(format!("--unit={}", Self::daemon_unit_name()))
                // Units get the service manager's environment, not ours —
                // forward every SUSI_*/XDG_* var or a second instance
                // (SUSI_HOME/SUSI_PORT_OFFSET/leaf ports) silently lands
                // back on the primary's substrate root and canonical ports.
                .args(std::env::vars_os().filter_map(|(k, v)| {
                    let k = k.to_string_lossy();
                    (k.starts_with("SUSI_") || k.starts_with("XDG_"))
                        .then(|| format!("--setenv={k}={}", v.to_string_lossy()))
                }));
            // Provider keys ride in an owner-only file, never on argv
            // (`provider_key_env_body`); it is removed once the daemon is up.
            let key_env_file = substrate_home.join(DAEMON_KEY_ENV_FILE);
            if write_provider_key_env_file(&key_env_file) {
                run.arg(format!(
                    "--property=EnvironmentFile=-{}",
                    key_env_file.display()
                ));
            }
            let spawned_via_systemd = run
                .arg(format!(
                    "--property=MemoryMax={}G",
                    mem_cap / (1024 * 1024 * 1024)
                ))
                .arg(format!("--property=CPUQuota={cpu_quota}%"))
                // No `--working-directory=`: systemd-run only knows it from
                // systemd 240 on, and an older one (RHEL/Rocky/Alma 8,
                // Ubuntu 18.04) rejects the whole call, silently dropping
                // every start to the uncapped double-fork below. A user
                // unit already starts in the user's home, and the daemon
                // enters its substrate home itself (`run_daemon_loop`).
                .arg("--")
                .arg(&unit_bin)
                .arg("daemon-start")
                .arg("--workspace")
                .arg(substrate_home.to_str().unwrap_or("."))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if spawned_via_systemd {
                // systemd-run returns before binds complete — wait for the
                // public host-contract ports so callers never race.
                if !Self::wait_for_host_contract(Duration::from_secs(10)) {
                    warn!("[SusiDaemon] spawned via systemd but host contract never came up");
                }
                let _ = fs::remove_file(&key_env_file);
                return;
            }
            let _ = fs::remove_file(&key_env_file);
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let mut cmd = Self::daemon_command(&bin_to_run, &substrate_home);
            // SAFETY: `pre_exec` requires the closure to only call
            // async-signal-safe functions, since it runs in the forked child
            // between fork() and execve() with the rest of this (possibly
            // multi-threaded, e.g. the swarm's worker threads) process's threads gone but
            // its locks (malloc, etc.) in whatever state they were left in.
            // `fork()`, `_exit()`, and `setsid()` are all POSIX
            // async-signal-safe, so this closure does not risk deadlocking.
            // The nested fork() here implements the classic Unix double-fork
            // daemonize: this closure runs in Command::spawn()'s child, which
            // is guaranteed single-threaded (fork() only carries the calling
            // thread over), so this second fork() is not exposed to the
            // "another thread held a lock at fork time" hazard either. The
            // grandchild (fork() == 0) detaches into its own session via
            // setsid() and returns Ok(()) to proceed to execve(); the
            // intermediate middle process (fork() > 0) exits immediately via
            // _exit() without ever reaching execve(), so only the detached
            // grandchild becomes the running daemon. spawn() below then waits
            // on the already-exited middle process, not the daemon itself.
            // SAFETY: the pre_exec closure only calls async-signal-safe libc functions (fork, _exit, setsid) between fork and exec.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::fork() > 0 {
                        libc::_exit(0);
                    }
                    libc::setsid();
                    Ok(())
                });
            }
            if let Ok(mut child) = Self::spawn_without_inheriting_stdio(&mut cmd) {
                let _ = child.wait();
            }
            if !Self::wait_for_host_contract(Duration::from_secs(10)) {
                warn!("[SusiDaemon] daemon spawned but host contract never came up");
            }
        }

        #[cfg(not(unix))]
        {
            let mut cmd = Self::daemon_command(&bin_to_run, &substrate_home);
            let _ = Self::spawn_without_inheriting_stdio(&mut cmd);
            if !Self::wait_for_host_contract(Duration::from_secs(10)) {
                warn!("[SusiDaemon] daemon spawned but host contract never came up");
            }
        }
    }

    pub fn run_daemon_loop(_legacy_workspace_arg: PathBuf, global_dir: PathBuf) {
        if cfg!(test) {
            return;
        }
        use crate::boot_trace::phase;
        let boot_started = Instant::now();
        // Daemon is always bound to substrate home — ignore any stale
        // project-cwd passed via --workspace for backwards compatibility.
        let workspace = susi_paths::SusiDirs::substrate_home();
        if let Err(error) = std::fs::create_dir_all(&workspace) {
            eprintln!("[SusiDaemon] Failed to create substrate directory: {error}");
            return;
        }
        // The daemon owns no project, so its working directory is the
        // substrate home — never whatever directory the spawning command
        // ran in. Anything resolved from the cwd (the intent ladder's repo
        // root, relative paths) must not land in an agent's worktree, and on
        // Windows a process's cwd pins that directory against removal.
        if let Err(error) = std::env::set_current_dir(&workspace) {
            warn!(
                "[SusiDaemon] could not enter substrate home {}: {error}",
                workspace.display()
            );
        }
        // Upgrade path: migrate persisted host state (backup + rollback on
        // failure) before anything below reads or rewrites it — token seed,
        // config load, port canonicalization all assume the current schema.
        if let Err(error) = susi_config::migrate_state_dir(&global_dir) {
            eprintln!("[SusiDaemon] Fatal: state migration failed: {error}");
            return;
        }
        // Zero-trust: seed host bearer token before opening world-facing ports.
        if let Err(error) = crate::susi_sandbox::manager::SusiConfig::ensure_api_auth_token_seeded()
        {
            eprintln!("[SusiDaemon] Failed to secure host API token: {error}");
            return;
        }
        // Startup autofixes: correct rather than fail — missing substrate
        // dirs are created and a stale lock (> 1h, dead process) removed.
        let mut issues = Vec::new();
        for sub in ["missions", "logs", "context"] {
            if !workspace.join(sub).is_dir() {
                issues
                    .push(crate::zc_startup_autofix::StartupIssue::MissingDir { path: sub.into() });
            }
        }
        let autofix = crate::zc_startup_autofix::apply_startup_autofix(&workspace, &issues);
        for action in &autofix.actions {
            info!("[SusiDaemon] startup autofix: {action}");
        }

        // Zero-config cloud keys for always-on / systemd spawns (no shell env).
        let lock_file_path = Self::get_lock_file(&global_dir);

        // Ensure lock file is cleaned if stale (> 1 hour old and process is dead)
        let mut stale_lock = Vec::new();
        if let Ok(metadata) = std::fs::metadata(&lock_file_path)
            && let Ok(modified) = metadata.modified()
            && let Ok(age) = modified.elapsed()
            && age.as_secs() > 3600
            && !Self::is_process_alive(&lock_file_path)
        {
            if let Some(rel) = lock_file_path
                .strip_prefix(&global_dir)
                .ok()
                .and_then(|p| p.to_str())
            {
                stale_lock.push(crate::zc_startup_autofix::StartupIssue::StaleLock {
                    path: rel.to_string(),
                });
            } else {
                let _ = std::fs::remove_file(&lock_file_path);
            }
        }
        if !stale_lock.is_empty() {
            let fix = crate::zc_startup_autofix::apply_startup_autofix(&global_dir, &stale_lock);
            for action in &fix.actions {
                info!("[SusiDaemon] startup autofix: {action}");
            }
        }

        // The lock comes before any composition work: a second daemon must
        // not spend its boot wiring a substrate it will never own.
        let mut lock = match DaemonLock::acquire(&lock_file_path) {
            Ok(l) => l,
            Err(e) => {
                eprintln!(
                    "[SusiDaemon] Failed to acquire lock: {}. Daemon likely already running.",
                    e
                );
                return;
            }
        };

        if let Err(e) = lock.write_pid(&workspace) {
            eprintln!("[SusiDaemon] Failed to write PID to lock file: {}", e);
            return;
        }
        // The lock is ours, so any ports.json here is a dead daemon's: it
        // must not feed this boot's offset resolution.
        let _ = fs::remove_file(global_dir.join(susi_paths::zc_ports_file::FILE_NAME));

        // Lifecycle record: warn when the running binary is newer than
        // the one the previous start recorded — a long-running daemon on
        // a stale binary serves old behaviour until restarted.
        let bin_mtime = std::env::current_exe()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let lifecycle_path = global_dir.join("daemon-lifecycle.json");
        let prev: crate::zc_daemon_lifecycle::DaemonLifecycle =
            std::fs::read_to_string(&lifecycle_path)
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or(crate::zc_daemon_lifecycle::DaemonLifecycle {
                    running: false,
                    binary_mtime: 0,
                });
        if prev.needs_restart(bin_mtime) {
            warn!("[SusiDaemon] binary is newer than the recorded start — a restart picks it up");
        }
        let lifecycle = prev.ensure_started(bin_mtime);
        if let Ok(text) = serde_json::to_string(&lifecycle) {
            let _ = std::fs::write(&lifecycle_path, text);
        }

        // Mandate 42: config.json is user-editable and can be hand-corrupted
        // into invalid JSON; with `panic = "abort"` (Cargo.toml release
        // profile) a panic here would abort the whole daemon process, not
        // just this thread, so this follows the same eprintln+return
        // pattern as the lock-acquisition failures just above rather than
        // panicking.
        let mut cfg = match SusiConfig::load(&global_dir) {
            Ok(cfg) => cfg,
            Err(e) => {
                eprintln!("[SusiDaemon] Fatal: malformed configuration: {}", e);
                return;
            }
        };
        let bind_address = cfg
            .get::<String>("bind_address")
            .unwrap_or_else(|| "127.0.0.1".to_string());

        // The host port block (CEO decision 2026-11-08). A pinned offset is
        // bound exactly. With none pinned, a foreign holder of one of the
        // six ports (after the stale-susi reclaim) moves the whole block to
        // the first free offset k*100. The choice is pinned into this
        // process's environment before any worker thread or child exists,
        // so every reader and every leaf service agrees on it, and it is
        // published in ports.json once the contract is bound.
        let pinned_offset = cfg.explicit_port_offset();
        let fallback_from = pinned_offset.unwrap_or(0);
        let offset = match crate::port_fallback::plan(
            pinned_offset,
            crate::port_fallback::bind_probe(&bind_address),
            |port| Self::attempt_port_reclaim(port, &global_dir),
        ) {
            crate::port_fallback::Plan::Pinned(offset) => offset,
            crate::port_fallback::Plan::Serve { offset, held } => {
                if !held.is_empty() {
                    let line = format!(
                        "[SusiDaemon] {}; serving the host contract at offset {offset}: \
                         ports {}. Pin a block with SUSI_PORT_OFFSET to choose it.",
                        Self::describe_held_ports(&held),
                        crate::port_fallback::describe_block(offset)
                    );
                    eprintln!("{line}");
                    warn!("{line}");
                }
                offset
            }
            crate::port_fallback::Plan::Exhausted { held } => {
                // Both channels: a spawned daemon's stderr is detached, so
                // the audit log is where `susi start` sends the operator.
                let line = format!(
                    "[SusiDaemon] Fatal: {} and no block of 100 ports at offsets {}-{} is \
                     free. Free the port, or pin a free block with SUSI_PORT_OFFSET=<offset> \
                     and restart susi.",
                    Self::describe_held_ports(&held),
                    susi_paths::ports::FALLBACK_STEP,
                    susi_paths::ports::FALLBACK_STEP * susi_paths::ports::FALLBACK_BLOCKS
                );
                eprintln!("{line}");
                tracing::error!("{line}");
                return;
            }
        };
        susi_paths::ports::pin_process_offset(offset);

        let ctx = DaemonContext::new(lock);
        if let Err(e) = ctx.setup_signal_handlers() {
            eprintln!("[SusiDaemon] Signal handler setup failed: {}", e);
        }
        // Mandate: config may have been polluted by older "port randomization"
        // self-healing — always restore the public port contract before bind.
        Self::force_canonical_ports(&mut cfg);
        if let Err(error) = cfg.save(&global_dir) {
            warn!("[SusiDaemon] Failed to persist canonical host ports: {error}");
        }

        // Composition root, the part the host contract depends on: plane
        // handlers and EngineHooks before any ToolRegistry / MCP dispatch,
        // the context graph tools write into, and the MAC policy every tool
        // call is authorized against. Provider discovery (catalogs, cloud
        // endpoints, local engines, MCP servers) is not on this path — it
        // runs after the contract is up and fills the registry as it goes.
        phase(
            "daemon: engine hooks",
            crate::composition::wire_daemon_engine_hooks,
        );
        phase("daemon: context graph", || {
            susi_core::context_graph::ContextGraph::init_global_storage(
                workspace.join("context_graph.jsonl"),
            )
        });
        phase("daemon: mac policy", || {
            crate::privacy::wire_mac_policy(&workspace)
        });

        // Boot key health + zero-config scorecard — what still needs
        // attention is reported, not silently skipped.
        phase("daemon: key health + zc score", || {
            Self::report_key_health_and_score(&global_dir)
        });

        // The host contract, as early as the boot allows: `susi start`
        // waits on exactly these sockets, and every worker below is
        // substrate work the contract must never wait on.
        phase("daemon: host contract bind + serve", || {
            Self::serve_host_contract(&cfg, &global_dir, &workspace, &bind_address, fallback_from)
        });
        info!(
            "[SusiDaemon] host contract up {:.1}s after daemon start",
            boot_started.elapsed().as_secs_f64()
        );

        // The providers the contract answers with — configured cloud
        // endpoints, the Candle fallback, the tool registry — registered at
        // once, beside the leaf services coming up: seconds after the bind,
        // never behind discovery (catalog priming, engine probes, the
        // ecosystem scan), which took a minute or more and left `/v1/models`
        // and inference without a single provider meanwhile. Discovery
        // below waits for this one-shot before it starts.
        crate::service_supervision::Supervisor::global().spawn_oneshot(
            CONTRACT_PROVIDERS,
            1,
            || {
                Some(|| {
                    let started = Instant::now();
                    crate::discovery_pipeline::register_contract_providers();
                    crate::boot_trace::finished("daemon: contract providers", started);
                })
            },
        );

        // Process layer: bring the leaf services up and keep them up for
        // the life of the daemon. The supervisor terminates only pids it
        // spawned itself, recorded in the shared process table. Up before
        // discovery, so its config/paths/error calls reach real services.
        phase("daemon: leaf services", || {
            crate::supervisor::start(Arc::clone(&ctx.shutdown_signal))
        });
        if ctx.is_shutdown_requested() {
            Self::shutdown_children(&global_dir);
            return;
        }

        // Initialize Global Stigmergic Blackboard (Swarm OS Points 5, 21, 25, 31)
        let blackboard = crate::blackboard::SwarmBlackboard::new();
        let blackboard_evaporator = blackboard.clone();
        crate::service_supervision::Supervisor::global().spawn(
            "pheromone-evaporator",
            3,
            Some(Duration::from_secs(60)),
            move || {
                let blackboard_evaporator = blackboard_evaporator.clone();
                Some(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        blackboard_evaporator.evaporate_pheromones();
                        crate::service_supervision::heartbeat("pheromone-evaporator");
                    }
                })
            },
        );

        // Substrate Administration & Hardware Optimization (Pillar 1)
        crate::runtime_admin::SusiRuntimeAdmin::start_administration_cycle(
            &workspace,
            blackboard.clone(),
        );

        // Host OS planes (gossip, NAT, checkpoints, cloud inventory): the
        // inventory probe spawns five CLIs, so it runs off the boot thread.
        {
            let workspace = workspace.clone();
            crate::service_supervision::Supervisor::global().spawn_oneshot(
                "os-planes",
                1,
                move || {
                    let workspace = workspace.clone();
                    Some(move || crate::composition::wire_daemon_os_planes(&workspace))
                },
            );
        }

        // Pillar 8: zero-config discovery of local inference engines + MCP
        // tools, then periodic rediscovery — after the contract providers
        // above are in. A full discovery pass probes engines, MCP servers
        // and vendors and can take minutes; it fills the registry in the
        // background while the contract already serves with its providers.
        let rediscovery_secs = cfg.capability_rediscovery_secs();
        crate::service_supervision::Supervisor::global().spawn_oneshot(
            "zero-config-bootstrap",
            1,
            move || {
                Some(move || {
                    let started = Instant::now();
                    // Never ahead of the contract providers: discovery
                    // prunes and re-registers providers.
                    let _ =
                        crate::service_supervision::Supervisor::global().join(CONTRACT_PROVIDERS);
                    // run_daemon_loop is sync (invoked from CLI `daemon-start`);
                    // a short-lived runtime drives the async probes.
                    match tokio::runtime::Runtime::new() {
                        Ok(runtime) => {
                            runtime.block_on(
                                crate::discovery_pipeline::bootstrap_zero_config_substrate(),
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "[SusiDaemon] Zero-config substrate bootstrap skipped: {}",
                                e
                            );
                        }
                    }
                    crate::boot_trace::finished("daemon: zero-config bootstrap", started);
                    crate::discovery_pipeline::spawn_periodic_rediscovery(rediscovery_secs);
                })
            },
        );

        // Spawn the cells already present once; the watcher below spawns
        // only cells added afterwards.
        crate::auto_discovery::spawn_all_cells(&workspace);

        // Hot-pluggable cell watcher: monitors ~/.susi/cells/
        // for new/removed files and triggers auto-discovery without daemon restart.
        let _cell_watcher = crate::cell_watcher::start_cell_watcher(
            crate::cell_watcher::CellWatcherConfig::for_substrate(&workspace),
        );

        // Cluster scout: discovery broadcasts, liveness decay, roster
        // rehydration, and commit-ledger anti-entropy all live in the
        // thread `list_cluster_nodes` lazily spawns. Without this kick it
        // only ever starts when a mission happens to touch the roster —
        // the daemon would answer inbound pings but never scout outbound,
        // and ledger anti-entropy would stay dormant between missions.
        // Supervised one-shot: a panic before the scout starts retries
        // (bounded); a clean return retires it.
        crate::service_supervision::Supervisor::global().spawn_oneshot(
            "swarm-scout-kick",
            1,
            || {
                Some(|| {
                    let _ = susi_gawd::amas::SusiSupervisor::list_cluster_nodes();
                })
            },
        );

        // Continuous Interaction Substrate Worker
        // Each pulse carries the ingesting caller's cwd (`pulse.workspace`).
        // Never substitute the daemon's boot workspace — that made "susi" in
        // folder B silently operate on folder A whenever the daemon had been
        // started from A (same bug class as the cross-workspace binary restart).
        // Supervised: the heartbeat bound is generous — a single pulse can
        // legitimately block in solve_stream for many minutes.
        crate::service_supervision::Supervisor::global().spawn(
            "pulse-worker",
            3,
            Some(Duration::from_secs(900)),
            || {
                Some(move || {
                    let queue = susi_gawd::queue::SubstratePulseQueue::global();
                    let ama = susi_gawd::ama::SusiMasterAgent::new();

                    loop {
                        crate::service_supervision::heartbeat("pulse-worker");
                        if let Some(pulse) = queue.pop() {
                            info!(
                                "[SubstratePulseQueue] Processing Pulse: {} (workspace: {})",
                                pulse.intent,
                                pulse.workspace.display()
                            );
                            let _ = ama.solve_stream(
                                &pulse.intent,
                                susi_gawd::queue::SubstratePulseQueue::execution_workspace(&pulse),
                                &pulse.version,
                                &|_| {},
                            );
                        }
                        thread::sleep(Duration::from_millis(100));
                    }
                })
            },
        );

        while !ctx.is_shutdown_requested() {
            // In-process supervision: restart dead workers, flag hung
            // ones, persist the status surface every round.
            let supervision = crate::service_supervision::Supervisor::global();
            supervision.supervise();
            supervision.persist_report();
            wait_for_shutdown(
                ctx.shutdown_signal.as_ref(),
                crate::service_supervision::SUPERVISE_INTERVAL,
            );
        }

        Self::shutdown_children(&global_dir);
    }

    fn shutdown_children(global_dir: &Path) {
        eprintln!("[SusiDaemon] Graceful shutdown initiated");
        // A stopped daemon publishes no port block.
        susi_paths::zc_ports_file::PortsFile::remove_if_written_by(global_dir, std::process::id());
        // Swarm Cells are the daemon's children; never orphan them.
        let stopped = crate::auto_discovery::stop_all_cells();
        if stopped > 0 {
            eprintln!("[SusiDaemon] Stopped {stopped} Swarm Cell(s)");
        }
        crate::supervisor::shutdown_all();
        // The monitor is shutdown-aware (aborts mid-pass, never spawns
        // or saves once the flag lands) — join it so no stale write or
        // in-flight spawn outlives the teardown.
        let _ = crate::service_supervision::Supervisor::global().join("leaf-service-monitor");
    }

    /// `canonical port 9090 (GMCP/MCP HTTP, tcp) held by pid 4242
    /// (prometheus.exe)` for each held port, joined. The holder is named
    /// when the OS can say who it is.
    fn describe_held_ports(held: &[(u16, crate::port_fallback::Proto)]) -> String {
        held.iter()
            .map(|(port, proto)| {
                let holders: Vec<String> = Self::port_owner_candidates(*port)
                    .into_iter()
                    .map(|pid| {
                        let image =
                            crate::susi_config::process_probe::exe_path(pid).and_then(|exe| {
                                exe.file_name().map(|n| n.to_string_lossy().into_owned())
                            });
                        match image {
                            Some(image) => format!("pid {pid} ({image})"),
                            None => format!("pid {pid}"),
                        }
                    })
                    .collect();
                format!(
                    "canonical port {port} ({}, {}) held by {}",
                    crate::port_fallback::label(*port),
                    proto.label(),
                    if holders.is_empty() {
                        "another process".to_string()
                    } else {
                        holders.join(", ")
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Boot key health + zero-config scorecard — what still needs
    /// attention is reported, not silently skipped.
    fn report_key_health_and_score(global_dir: &Path) {
        let env_text = std::fs::read_to_string(crate::susi_config::cloud_env::cloud_env_path())
            .unwrap_or_default();
        let ages_path =
            crate::susi_config::key_rotation::KeyAges::path_in(&susi_paths::SusiDirs::config_dir());
        let ages = crate::susi_config::key_rotation::KeyAges::load(&ages_path).unwrap_or_default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let key_statuses: Vec<crate::zc_keys_health_boot::KeyStatus> =
            crate::susi_config::key_rotation::key_statuses(
                &env_text,
                &ages,
                now,
                crate::susi_config::key_rotation::DEFAULT_MAX_AGE_DAYS,
            )
            .into_iter()
            .map(|s| crate::zc_keys_health_boot::KeyStatus {
                vendor: s.key,
                health: if s.needs_rotation {
                    crate::zc_keys_health_boot::KeyHealth::Expired
                } else {
                    crate::zc_keys_health_boot::KeyHealth::Ok
                },
            })
            .collect();
        for line in crate::zc_keys_health_boot::boot_key_report(&key_statuses) {
            warn!("[SusiDaemon] key health: {line}");
        }

        // Zero-config score: the debt the release still asks the user for.
        let steps: Vec<crate::zc_scorecard::ManualStep> =
            crate::susi_config::zc_debt_report::scan_debt(&[])
                .into_iter()
                .map(|d| crate::zc_scorecard::ManualStep {
                    description: d.description,
                    closing_task: d.removes_via,
                })
                .collect();
        let score_path = global_dir.join("zc-score.json");
        let prior: f64 = std::fs::read_to_string(&score_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(100.0);
        let card = crate::zc_scorecard::scorecard(steps, prior);
        info!(
            "[SusiDaemon] zero-config score {:.0} (debt {}, delta {:+.1})",
            card.score, card.debt, card.delta_since_release
        );
        if let Ok(text) = serde_json::to_string(&card.score) {
            let _ = std::fs::write(&score_path, text);
        }
    }

    /// Bind the host contract — 9090–9094 plus the uniform offset decided at
    /// boot — publish it in `ports.json`, and start its supervised server
    /// workers. Never an ephemeral or per-port alternative.
    /// `fallback_from` is the offset the daemon was configured for.
    fn serve_host_contract(
        cfg: &SusiConfig,
        global_dir: &Path,
        workspace: &Path,
        bind_address: &str,
        fallback_from: u16,
    ) {
        // TLS for the public endpoints: configured cert/key, or an
        // auto-generated self-signed cert whenever the bind is reachable
        // off-host. Each listener sniffs the first byte per connection, so
        // plain HTTP (internal loopback callers) keeps working on the same
        // port. Remote plaintext is always dropped.
        let tls_acceptor = crate::tls::endpoint_acceptor(bind_address, global_dir);
        let require_tls_remote = crate::tls::https_only()
            || (!crate::tls::is_loopback(bind_address) && tls_acceptor.is_none());
        let (gemi_port, gmcp_port, gmcp_http_port, a2a_port, udp_port) = (
            cfg.gemi_port(),
            cfg.gmcp_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port(),
            cfg.udp_discovery_port(),
        );
        let gemi_server =
            Self::bind_tcp_canonical(gemi_port, "GEMI HTTP", global_dir, bind_address);
        let gmcp_primary =
            Self::bind_tcp_canonical(gmcp_port, "GMCP/MCP HTTP", global_dir, bind_address);
        let gmcp_alias =
            Self::bind_tcp_canonical(gmcp_http_port, "GMCP HTTP alias", global_dir, bind_address);
        let a2a_http = Self::bind_tcp_canonical(a2a_port, "A2A HTTP", global_dir, bind_address);
        let udp_socket =
            Self::bind_udp_canonical(udp_port, "A2A UDP discovery", global_dir, bind_address);

        // Bound: tell every client where. `susi start`, the CLI and the
        // desktop resolve the offset from this file while this daemon holds
        // its lock; it goes away on a clean stop.
        let published = susi_paths::zc_ports_file::PortsFile::for_daemon(
            global_dir.to_path_buf(),
            cfg.port_offset(),
            fallback_from,
            std::process::id(),
            tls_acceptor.is_some(),
        );
        if let Err(error) = published.write_to_home() {
            warn!(
                "[SusiDaemon] could not publish {}: {error}",
                published.path.display()
            );
        }

        let (scheme, transport) = if tls_acceptor.is_some() {
            // Both are live on every socket — first-byte sniff picks per
            // connection; https is the advertised scheme.
            ("https", "http+https")
        } else {
            ("http", "http")
        };
        eprintln!(
            "[SusiDaemon] Public endpoints ready ({transport} on every socket):\n\
             - GMCP/MCP  {}://{}:{}/mcp\n\
             - GEMI      {}://{}:{}/\n\
             - UDP disco {}:{}\n\
             - GMCP alias {}://{}:{}/mcp\n\
             - A2A       {}://{}:{}/",
            scheme,
            bind_address,
            gmcp_port,
            scheme,
            bind_address,
            gemi_port,
            bind_address,
            udp_port,
            scheme,
            bind_address,
            gmcp_http_port,
            scheme,
            bind_address,
            a2a_port
        );

        // Each server thread is supervised: the registry holds the
        // listeners and `try_clone`s them per (re)spawn, so a panicked or
        // exited server is restarted in place with a bounded budget and
        // reported on the status surface instead of silently staying dead.
        let tls_gemi = tls_acceptor.clone();
        let workspace_gemi = workspace.to_path_buf();
        let require_tls_gemi = require_tls_remote;
        crate::service_supervision::Supervisor::global().spawn("gemi-http", 5, None, move || {
            let workspace = workspace_gemi.clone();
            let listeners: Vec<std::net::TcpListener> = gemi_server
                .iter()
                .filter_map(|l| l.clone_uninheritable().ok())
                .collect();
            let tls = tls_gemi.clone();
            Some(move || {
                if listeners.is_empty() {
                    eprintln!("[GEMI] no listener left to serve on");
                    return;
                }
                GemiServer::start_http_server(workspace, listeners, tls, require_tls_gemi);
            })
        });

        let tls_gmcp = tls_acceptor.clone();
        let workspace_gmcp = workspace.to_path_buf();
        let require_tls_gmcp = require_tls_remote;
        crate::service_supervision::Supervisor::global().spawn("gmcp-http", 5, None, move || {
            let workspace = workspace_gmcp.clone();
            let listeners: Vec<std::net::TcpListener> = gmcp_primary
                .iter()
                .filter_map(|l| l.clone_uninheritable().ok())
                .collect();
            let tls = tls_gmcp.clone();
            Some(move || {
                if listeners.is_empty() {
                    eprintln!("[GMCP] no listener left to serve on");
                    return;
                }
                GmcpServer::start_http_server(workspace, listeners, tls, require_tls_gmcp);
            })
        });

        let tls_gmcp_alias = tls_acceptor.clone();
        let workspace_gmcp_alias = workspace.to_path_buf();
        let require_tls_gmcp_alias = require_tls_remote;
        crate::service_supervision::Supervisor::global().spawn(
            "gmcp-http-alias",
            5,
            None,
            move || {
                let workspace = workspace_gmcp_alias.clone();
                let listeners: Vec<std::net::TcpListener> = gmcp_alias
                    .iter()
                    .filter_map(|l| l.clone_uninheritable().ok())
                    .collect();
                let tls = tls_gmcp_alias.clone();
                Some(move || {
                    if listeners.is_empty() {
                        eprintln!("[GMCP alias] no listener left to serve on");
                        return;
                    }
                    GmcpServer::start_http_server(
                        workspace,
                        listeners,
                        tls,
                        require_tls_gmcp_alias,
                    );
                })
            },
        );

        crate::service_supervision::Supervisor::global().spawn(
            "a2a-udp-discovery",
            5,
            None,
            move || {
                // Only signed pings are answered; the pong carries the
                // responder's attested identity and roster, no port.
                let socket = udp_socket.clone_uninheritable().ok();
                Some(move || {
                    let Some(socket) = socket else {
                        eprintln!("[UDP] socket lost; discovery worker exiting");
                        return;
                    };
                    Self::start_udp_discovery_server(socket);
                })
            },
        );

        // Same zero-trust policy as GMCP/GEMI: a bound member's Ed25519
        // signature over the exact body authorizes on its own, the bearer
        // covers unbound/standalone callers, and everything else fails
        // closed. The verifier closure receives the buffered body so v2
        // (body-bound) signatures verify, not just v1.
        let a2a_verifier: susi_gawd_a2a::server::Verifier =
            std::sync::Arc::new(|ctx: &susi_gawd_a2a::server::VerifierContext| {
                let header = |name: &str| ctx.headers.get(name).and_then(|v| v.to_str().ok());
                let signed = susi_gawd::net_guard::SignedRequest {
                    node: header("x-susi-node"),
                    ts_secs: header("x-susi-req-ts").and_then(|s| s.parse().ok()),
                    nonce: header("x-susi-req-nonce"),
                    sig: header("x-susi-req-sig"),
                };
                susi_gawd::net_guard::NetGuard::is_authorized(
                    header("authorization"),
                    ctx.peer,
                    &signed,
                    ctx.method,
                    ctx.path,
                    Some(ctx.body),
                )
            });
        // The sealed peer channel: the one surface meant for other machines
        // (its own port, `peer_channel_listen`), the uplinks to the hubs in
        // `peer_channel_hubs.json`, and the hand-off of sessions that
        // terminate here to the GMCP surface.
        crate::peer_channel_host::start(cfg);

        let tls_a2a = tls_acceptor;
        let require_tls_a2a = require_tls_remote;
        crate::service_supervision::Supervisor::global().spawn("a2a-http", 5, None, move || {
            let listeners: Vec<std::net::TcpListener> = a2a_http
                .iter()
                .filter_map(|l| l.clone_uninheritable().ok())
                .collect();
            let verifier = Arc::clone(&a2a_verifier);
            let tls = tls_a2a.clone();
            Some(move || {
                if listeners.is_empty() {
                    eprintln!("[A2A] no listener left to serve on");
                    return;
                }
                if let Err(e) =
                    susi_gawd_a2a::server::serve(listeners, verifier, tls, require_tls_a2a)
                {
                    eprintln!("[A2A] Server exited: {e}");
                }
            })
        });
    }

    fn force_canonical_ports(cfg: &mut SusiConfig) {
        // The persisted per-port keys are documentation only (accessors
        // derive effective ports from canonical base + port_offset) — write
        // the effective values so config.json reflects what is bound.
        let ports = [
            ("gmcp_port", cfg.gmcp_port()),
            ("gemi_port", cfg.gemi_port()),
            ("udp_discovery_port", cfg.udp_discovery_port()),
            ("gmcp_http_port", cfg.gmcp_http_port()),
            ("a2a_http_port", cfg.a2a_http_port()),
        ];
        for (key, port) in ports {
            cfg.settings
                .insert(key.to_string(), serde_json::json!(port));
        }
    }

    /// Bind a TCP port of the block decided at boot. Reclaims stale susi
    /// holders; never randomizes — exit if a foreign process owns the port
    /// (a pinned offset, or a port taken between the boot probe and here).
    ///
    /// Returns every socket to serve: a specific non-loopback `bind_address`
    /// would strand internal callers dialing 127.0.0.1, so loopback is always
    /// bound alongside it. Wildcard binds already cover loopback.
    fn bind_tcp_canonical(
        port: u16,
        name: &str,
        global_dir: &Path,
        bind_address: &str,
    ) -> Vec<std::net::TcpListener> {
        crate::port_fallback::tcp_bind_addrs(bind_address, port)
            .iter()
            .map(|addr| Self::bind_tcp_with_retry(port, name, global_dir, addr))
            .collect()
    }

    fn bind_tcp_with_retry(
        port: u16,
        name: &str,
        global_dir: &Path,
        addr: &str,
    ) -> std::net::TcpListener {
        for attempt in 1..=5 {
            match std::net::TcpListener::bind(addr) {
                Ok(listener) => return listener,
                Err(e) => {
                    eprintln!(
                        "[SusiDaemon] {} cannot bind {} (attempt {}/5): {}",
                        name, addr, attempt, e
                    );
                    if Self::attempt_port_reclaim(port, global_dir) {
                        thread::sleep(Duration::from_millis(200 * attempt as u64));
                        continue;
                    }
                    thread::sleep(Duration::from_millis(150 * attempt as u64));
                }
            }
        }
        eprintln!(
            "[SusiDaemon] Fatal: canonical port {} ({}) is unavailable.\n\
             External clients trust {} — free the port (or stop the foreign process) and restart susi,\n\
             or pin a free block with SUSI_PORT_OFFSET. Port randomization is disabled by contract.",
            port, name, addr
        );
        std::process::exit(1);
    }

    fn bind_udp_canonical(
        port: u16,
        name: &str,
        global_dir: &Path,
        bind_address: &str,
    ) -> std::net::UdpSocket {
        let addr = format!("{}:{}", bind_address, port);
        for attempt in 1..=5 {
            match std::net::UdpSocket::bind(&addr) {
                Ok(socket) => return socket,
                Err(e) => {
                    eprintln!(
                        "[SusiDaemon] {} cannot bind {} (attempt {}/5): {}",
                        name, addr, attempt, e
                    );
                    if Self::attempt_port_reclaim(port, global_dir) {
                        thread::sleep(Duration::from_millis(200 * attempt as u64));
                        continue;
                    }
                    thread::sleep(Duration::from_millis(150 * attempt as u64));
                }
            }
        }
        eprintln!(
            "[SusiDaemon] Fatal: canonical UDP port {} ({}) is unavailable.\n\
             External clients trust {}:{} — free the port and restart susi, or pin a free \
             block with SUSI_PORT_OFFSET.",
            port, name, bind_address, port
        );
        std::process::exit(1);
    }

    /// Aggressive Port Reclaim: Interrogates the process holding a port and evicts it if it's a susi instance.
    ///
    /// Owners come from [`port_owner_candidates`](Self::port_owner_candidates)
    /// on every OS. Each one must pass
    /// [`is_trusted_susi_process`](Self::is_trusted_susi_process) — exe
    /// identity, this instance's `SUSI_HOME`, and the binary-hash trust
    /// anchor — before it is killed; one that fails is reported, never killed.
    #[allow(unsafe_code)]
    fn attempt_port_reclaim(port: u16, global_dir: &Path) -> bool {
        let own_pid = std::process::id();
        let mut reclaimed = false;
        for pid in Self::port_owner_candidates(port) {
            if pid <= 1 || pid == own_pid {
                continue;
            }
            let Ok(signed_pid) = i32::try_from(pid) else {
                continue;
            };
            if Self::is_trusted_susi_process(signed_pid, global_dir) {
                eprintln!(
                    "[Self-Healing] Evicting stale susi process (PID: {}) holding port {}...",
                    pid, port
                );
                #[cfg(unix)]
                // SAFETY: kill on a pid verified above to be a stale susi process holding our port; no memory is shared.
                unsafe {
                    libc::kill(signed_pid, libc::SIGKILL);
                }
                #[cfg(windows)]
                {
                    let mut kill = susi_paths::console::background_command("taskkill");
                    kill.args(["/PID", &pid.to_string(), "/F"]);
                    let _ =
                        susi_core::bounded_cmd::output_within(&mut kill, Duration::from_secs(10));
                }
                reclaimed = true;
            } else {
                eprintln!(
                    "[SusiDaemon] Port {} held by non-susi PID {} — will not evict",
                    port, pid
                );
            }
        }
        if reclaimed {
            thread::sleep(Duration::from_millis(250));
        }
        reclaimed
    }

    /// Pids bound to `port` (TCP listeners and UDP binds) as this OS can
    /// name them: `fuser`/`ss` on Linux; `lsof` TCP listeners on macOS and
    /// other unixes (UDP owners are not looked up there, since `lsof -iUDP`
    /// also matches clients sending to the port); `netstat -ano` TCP
    /// `LISTENING` and UDP rows on Windows. Candidates only — the caller
    /// applies the trust check.
    fn port_owner_candidates(port: u16) -> Vec<u32> {
        let mut pids: Vec<u32> = Vec::new();
        #[cfg(unix)]
        {
            let scripts = if cfg!(target_os = "linux") {
                vec![
                    format!("fuser {}/tcp 2>/dev/null", port),
                    format!("fuser {}/udp 2>/dev/null", port),
                    format!(
                        "ss -lptn 'sport = :{}' 2>/dev/null | sed -n 's/.*pid=\\([0-9]\\+\\).*/\\1/p'",
                        port
                    ),
                ]
            } else {
                vec![format!(
                    "lsof -nP -t -iTCP:{} -sTCP:LISTEN 2>/dev/null",
                    port
                )]
            };
            for script in scripts {
                let output = Command::new("sh").arg("-c").arg(&script).output();
                let Ok(out) = output else {
                    continue;
                };
                let pid_str = String::from_utf8_lossy(&out.stdout);
                for token in pid_str.split(|c: char| !c.is_ascii_digit()) {
                    if let Ok(pid) = token.parse::<u32>()
                        && !pids.contains(&pid)
                    {
                        pids.push(pid);
                    }
                }
            }
        }
        #[cfg(windows)]
        {
            let mut netstat = susi_paths::console::background_command("netstat");
            netstat.arg("-ano");
            if let Ok(out) =
                susi_core::bounded_cmd::output_within(&mut netstat, Duration::from_secs(10))
            {
                pids = susi_core::service_table::parse_netstat_owners(&out.stdout, &[port], true);
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = port;
        }
        pids
    }

    /// Whether `name` is the file name of the `susi` binary itself.
    fn is_susi_binary_name(name: &str) -> bool {
        if cfg!(windows) {
            name.eq_ignore_ascii_case("susi.exe")
        } else {
            name == "susi"
        }
    }

    /// Whether `exe` looks like a SUSI binary (`susi`, `susi.exe`, a
    /// `susi-*` leaf), ignoring Linux's ` (deleted)` marker so a stale
    /// orphan from a replaced build is still recognised as ours.
    fn is_susi_image(exe: Option<&Path>) -> bool {
        exe.and_then(Path::file_name)
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .is_some_and(|name| name.trim_end_matches(" (deleted)").starts_with("susi"))
    }

    /// Confirms `pid` is genuinely running a trusted susi binary before the
    /// Sovereign Eviction protocol (identity.json Mandate 22) is allowed to
    /// SIGKILL it. `/proc/{pid}/comm` is deliberately NOT used as identity
    /// evidence: it is the process's self-reported name (settable via
    /// `prctl`/`argv[0]`), so any unprivileged process could claim to be
    /// "susi" and either get needlessly evicted or, worse, masquerade
    /// as trusted. `/proc/{pid}/exe` is the kernel's own record of which file
    /// was actually exec'd and cannot be altered by the running process, so
    /// its SHA-256 is compared against this host's trusted `binary.hash`
    /// (the same file `verify_binary_integrity` maintains) — this fails
    /// closed (no eviction) if that trust anchor hasn't been established yet.
    ///
    /// Windows and macOS take the exe path the OS records for the process
    /// (through `sysinfo`); a running image cannot be replaced in place on
    /// Windows, and every other check is the same. A replaced Linux binary's
    /// ` (deleted)` link never matches the name check, so it fails closed.
    fn is_trusted_susi_process(pid: i32, global_dir: &Path) -> bool {
        let Ok(pid_u32) = u32::try_from(pid) else {
            return false;
        };
        let Some(exe_path) = crate::susi_config::process_probe::exe_path(pid_u32) else {
            return false;
        };

        let name_ok = exe_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(Self::is_susi_binary_name);
        if !name_ok {
            return false;
        }

        // Trust is per-instance: a sibling SUSI_HOME daemon runs the same
        // trusted binary and would pass the hash check below, but evicting
        // it to claim its port would kill a live independent node. Only
        // processes whose environ places them in THIS instance are ours to
        // reclaim.
        if !Self::daemon_in_this_instance(pid) {
            return false;
        }

        let hash_file = Self::get_hash_file(global_dir);
        let trusted_hash = match fs::read_to_string(&hash_file) {
            Ok(h) => h.trim().to_string(),
            Err(_) => return false,
        };
        crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&exe_path)
            .map(|h| h.trim() == trusted_hash)
            .unwrap_or(false)
    }

    fn start_udp_discovery_server(socket: std::net::UdpSocket) {
        // Mandate 42: `local_addr()` failing here would only degrade a log
        // message, not the actual listener below - a graceful fallback is
        // the right scope of fix, not restructuring this function to
        // propagate a Result for a purely decorative failure.
        let addr_display = socket
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "<unknown>".to_string());
        eprintln!(
            "[A2A Cluster UDP] Discovery listener active on {}",
            addr_display
        );
        // 4 KiB: signed pings are small today, but wire-format growth
        // (larger bloom fields, future capability metadata) must not
        // silently truncate and fail signature verification.
        let mut buf = [0u8; 4096];
        while let Ok((amt, src)) = socket.recv_from(&mut buf) {
            // host_only: discovery is off toward every other host, whatever
            // the bind, read per datagram so a mode switch applies at once.
            // A loopback ping is this machine's own readiness probe
            // (`host_contract_udp_ready`); its pong never leaves the host.
            if !src.ip().to_canonical().is_loopback() && susi_core::mac_policy::host_only_in_force()
            {
                continue;
            }
            // Discovery is zero-config for the local network only: a
            // datagram from a public address is dropped unread, whatever the
            // bind. Members on other networks join over the sealed peer
            // channel (`docs/peer-channel.md`), never over UDP.
            if !crate::peer_channel_host::is_lan_source(src.ip()) {
                continue;
            }
            // Only the bound dialect (`SUSI_PING_SIG3`) is answered: its MAC
            // covers the sender's source address, a timestamp and a nonce
            // (`verify_bound_ping`). Anything that is not a signed ping is
            // dropped before the key loads.
            if !buf[..amt].starts_with(b"SUSI_PING_SIG") {
                continue;
            }
            let msg = String::from_utf8_lossy(&buf[..amt]);
            // Cluster-key handshake (VC-200-001): answer a verified ping with
            // a signed pong echoing its nonce. A copy captured on the wire and
            // replayed from another address, after the window, or twice gets
            // silence; so does the retired unbound `SUSI_PING_SIG` /
            // `SUSI_PING_SIG2`, which binds nothing about its sender.
            let ping = match crate::susi_config::cluster_key::verify_bound_ping(&msg, src) {
                Ok(ping) => ping,
                Err(crate::susi_config::cluster_key::PingRejection::Unbound) => {
                    static NOTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
                    NOTED.get_or_init(|| {
                        eprintln!(
                            "[A2A Cluster UDP] dropped a retired unbound discovery ping from \
                             {src}: only SUSI_PING_SIG3 is answered; upgrade that node"
                        );
                    });
                    continue;
                }
                Err(_) => continue,
            };
            {
                // Mutual admission: a verified bound ping proves the daemon
                // at its source address holds cluster.key, the same proof a
                // signed pong gives the pinger about us. Self-edge guard:
                // persisting ourselves as Explicit lets mission dispatch
                // recurse into our own endpoint, so loopback, any local
                // interface address (the bind probe can only succeed
                // locally) and a ping attesting OUR node_id are never
                // admitted. The signed pong below still goes out: a CLI
                // probing its own address needs the refusal signal.
                let is_self = src.ip().is_loopback()
                    || std::net::TcpListener::bind((src.ip(), 0)).is_ok()
                    || ping.node_id == crate::susi_config::cluster_key::wire_node_id();
                if !is_self {
                    // The advertised service address is MAC-bound and its IP
                    // is the datagram's source: the peer's real HTTP alias
                    // port, offset-shifted or canonical.
                    let pinger_addr = ping.service.to_string();
                    // The advertised node_id is self-asserted: persisting it
                    // would let any member re-home another member's identity
                    // with one ping. A synthetic id is recorded with no key,
                    // so the row earns no credential (the peer client sends
                    // credentials only to key-attested members); the
                    // scout's reverse handshake and committed member_add
                    // records bind real ids and keys.
                    // A banned member gets no handshake at all: no admission
                    // on our side, and no signed pong to verify us with.
                    if susi_gawd::swarm::peer_registry::is_banned(&ping.node_id, &pinger_addr) {
                        continue;
                    }
                    let node = susi_gawd::swarm::amas::ClusterPeerNode {
                        node_id: format!("susi-peer-{}", src.ip()),
                        address: pinger_addr,
                        node_type: "PEER".into(),
                        is_active: true,
                        capabilities: ping
                            .caps_csv
                            .split(',')
                            .filter(|c| !c.is_empty())
                            .map(str::to_string)
                            .collect(),
                        registry_checksum: ping.checksum,
                        latency_ms: 0,
                        uptime_secs: 0,
                        trust_score: 0.8,
                        capability_bloom: susi_gawd::swarm::amas::CapabilityBloom::from_hex(
                            &ping.bloom_hex,
                        ),
                        admission: susi_gawd::swarm::amas::PeerAdmission::Explicit,
                        last_seen_secs: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0),
                        pubkey: String::new(),
                        key_bound_at: 0,
                        bind_sig: String::new(),
                    };
                    susi_gawd::swarm::peer_registry::persist_verified_peer(&node);
                }
                let bloom = susi_gawd::swarm::amas::CapabilityBloom::local_snapshot().to_hex();
                // Gossip our verified roster so one handshake teaches the
                // joiner the whole cluster — transitive membership.
                let roster: Vec<(String, String)> =
                    susi_gawd::swarm::peer_registry::load_persisted_peers()
                        .into_iter()
                        .map(|p| (p.node_id, p.address))
                        .collect();
                // Bound pings are identity-era: answer with v3 (subject-signed
                // binding) plus the v2 fallback.
                let nonce = ping.nonce;
                let pongs: Vec<Option<String>> = vec![
                    crate::susi_config::cluster_key::signed_pong_v3(
                        &crate::susi_config::cluster_key::wire_node_id(),
                        0,
                        &bloom,
                        &nonce,
                        &roster,
                    ),
                    crate::susi_config::cluster_key::signed_pong_v2(
                        &crate::susi_config::cluster_key::wire_node_id(),
                        0,
                        &bloom,
                        &nonce,
                        &roster,
                    ),
                ];
                for pong in pongs.into_iter().flatten() {
                    let _ = socket.send_to(pong.as_bytes(), src);
                }
                continue;
            }
            // A ping that fails verification gets no reply, like the
            // retired unauthenticated `SUSI_LAN_PING` / bare `SUSI_PING`
            // dialects: answering unsigned datagrams disclosed our node id
            // and GMCP port to any LAN host and made 9092 a reflector.
        }
    }

    /// systemd unit name for this instance — `susi-daemon` for the primary,
    /// suffixed with a stable hash of `SUSI_HOME` for an isolated sibling so
    /// two instances never share a unit (and stop only kills the right one).
    /// The same absolute-path rule as `SusiDirs::instance_root` applies: a
    /// relative `SUSI_HOME` selects no instance root anywhere, so it must not
    /// mint a distinct unit either.
    #[cfg(target_os = "linux")]
    fn daemon_unit_name() -> String {
        Self::unit_name_for(Self::instance_home().as_deref())
    }

    #[cfg(any(target_os = "linux", test))]
    fn unit_name_for(home: Option<&Path>) -> String {
        match home {
            Some(home) => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                home.hash(&mut hasher);
                format!("susi-daemon-{:x}", hasher.finish())
            }
            None => "susi-daemon".to_string(),
        }
    }

    /// The isolated instance root `SUSI_HOME` names, when it names one —
    /// matching `SusiDirs::instance_root`'s absolute-path requirement so unit
    /// naming, env propagation, and path resolution never disagree.
    fn instance_home() -> Option<PathBuf> {
        std::env::var_os("SUSI_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }

    /// Grace period `susi stop` gives a signalled daemon to exit before it
    /// escalates to a forced process-tree kill.
    const STOP_ESCALATE_AFTER: Duration = Duration::from_secs(10);
    /// Overall bound on `susi stop`. Past it, whatever is still alive or
    /// listening is a real failure (non-zero exit), never a "stopped" claim.
    pub const STOP_DEADLINE: Duration = Duration::from_secs(30);
    /// Poll interval for the stop wait. `process_alive` costs one OS probe.
    const STOP_POLL: Duration = Duration::from_millis(250);

    #[allow(unsafe_code)]
    pub fn stop_daemon(_workspace: &Path, global_dir: &Path) -> bool {
        let global_lock = Self::get_lock_file(global_dir);
        let Some(content) = read_lock_record(&global_lock) else {
            return false;
        };
        // The block the daemon serves, read while it still publishes it.
        let contract_ports = Self::host_contract_tcp_ports();
        let pid = content
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .parse::<u32>()
            .ok();
        let pids: Vec<u32> = pid
            .filter(|pid| Self::process_alive(*pid))
            .into_iter()
            .collect();
        let _ = Self::stop_and_wait(&pids, contract_ports);
        // Keep the lock while its process survives: it still names the daemon
        // holding the ports, and deleting it would hide a live instance.
        if pid.is_none_or(|pid| !Self::process_alive(pid)) {
            remove_lock_files(&global_lock);
        }
        true
    }

    /// `susi stop` — stops only THIS instance's daemon: its systemd unit
    /// (named after `SUSI_HOME`), its lock-file pid, orphan `daemon-start`
    /// processes whose environment matches this instance, and any SUSI
    /// process still listening on a host-contract port. A sibling
    /// `SUSI_HOME` instance keeps running.
    ///
    /// Returns whether the host contract actually released, so the CLI can
    /// exit non-zero on a stop that did not land.
    #[allow(unsafe_code)]
    pub fn stop_instance_daemons(global_dir: &Path) -> StopOutcome {
        Self::stop_daemons(global_dir, true)
    }

    /// Stops every susi daemon for this user — every `susi-daemon*` unit and
    /// every orphan `daemon-start` process, including instances rooted at
    /// other `SUSI_HOME`s. Used by `susi uninstall`, which must leave zero
    /// running daemons behind so a later reinstall starts clean.
    /// Returns how many daemon processes were signalled.
    #[allow(unsafe_code)]
    pub fn stop_all_daemons(global_dir: &Path) -> usize {
        Self::stop_daemons(global_dir, false).signalled
    }

    #[allow(unsafe_code)]
    fn stop_daemons(global_dir: &Path, scoped_to_instance: bool) -> StopOutcome {
        // 1. The systemd transient unit spawned by ensure_daemon_running's
        // systemd-run path (Linux only; no-op everywhere else). Scoped stop
        // targets only this instance's unit; uninstall sweeps every
        // `susi-daemon*` unit regardless of the SUSI_HOME they were named for.
        #[cfg(target_os = "linux")]
        {
            let units = if scoped_to_instance {
                vec![format!("{}.service", Self::daemon_unit_name())]
            } else {
                Self::daemon_units()
            };
            for unit in units {
                // Best effort: hosts without a user bus print "Failed to connect
                // to bus" — noise for a stop that has other paths to succeed.
                let _ = Command::new("systemctl")
                    .args(["--user", "stop"])
                    .arg(&unit)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
            }
        }

        let own_pid = std::process::id();
        let mut pids: Vec<u32> = Vec::new();
        // The block this instance serves, read once while its daemon still
        // publishes it: once the daemon is gone the resolver falls back to
        // the canonical block, which a foreign process may legitimately hold.
        let contract_ports = Self::host_contract_tcp_ports();

        // 2. Lock-file path — the common case.
        let global_lock = Self::get_lock_file(global_dir);
        let lock_pid = read_lock_record(&global_lock)
            .and_then(|content| content.lines().next()?.trim().parse::<u32>().ok());
        if let Some(pid) = lock_pid.filter(|pid| *pid != own_pid && Self::process_alive(*pid)) {
            pids.push(pid);
        }

        // 3. Orphan sweep: surviving `daemon-start` processes for this user,
        // lock file or not, on every OS (process exe and argv through
        // sysinfo). A candidate must run a SUSI image and carry
        // `daemon-start` in its argv; scoped stop keeps only the candidates
        // whose environment places them in this instance — a sibling's
        // daemon has a different SUSI_HOME, the primary's has none — and an
        // unreadable environment never matches.
        for process in crate::susi_config::process_probe::processes(true) {
            let pid = process.pid;
            if pid == own_pid || pids.contains(&pid) {
                continue;
            }
            let is_daemon = process.cmd.iter().any(|arg| arg == "daemon-start");
            if is_daemon
                && Self::is_susi_image(process.exe.as_deref())
                && (!scoped_to_instance || Self::daemon_in_this_instance(pid as i32))
            {
                pids.push(pid);
            }
        }

        // 4. Whoever still listens on a contract port and is a SUSI process:
        // the lock is gone (or names a dead pid) while a leaf/daemon child
        // keeps 919x bound. A foreign process on the port is never killed.
        for pid in Self::susi_port_holders(&contract_ports) {
            if pid != own_pid && !pids.contains(&pid) {
                pids.push(pid);
            }
        }

        let signalled = pids.len();
        let released = Self::stop_and_wait(&pids, contract_ports);

        // The lock names a daemon only while that daemon lives; remove it
        // once (and only once) the recorded pid is gone.
        if lock_pid.is_none_or(|pid| !Self::process_alive(pid)) {
            remove_lock_files(&global_lock);
        }

        StopOutcome {
            signalled,
            released,
        }
    }

    /// Bounded stop loop for the current instance's processes and the
    /// contract ports they served (`ports`, fixed before the first signal).
    fn stop_and_wait(pids: &[u32], ports: [u16; 4]) -> bool {
        Self::await_stop(
            pids,
            Self::STOP_DEADLINE,
            Self::STOP_ESCALATE_AFTER,
            Self::process_alive,
            Self::signal_tree,
            || !Self::tcp_ports_listening(&ports),
            || Self::susi_port_holders(&ports),
        )
    }

    /// Terminate `pids`, then wait until every one has exited **and** the
    /// host-contract ports released. Survivors are force-killed once
    /// `escalate_after` has elapsed (together with any SUSI process that
    /// still holds a contract port), and the wait gives up at `deadline`,
    /// returning the measured truth rather than assuming success.
    ///
    /// The probes are injected so tests prove the wait/escalate/deadline
    /// behaviour with fake processes instead of a real daemon.
    #[allow(clippy::too_many_arguments)] // one injected probe per OS surface, so a test fakes each independently
    fn await_stop(
        pids: &[u32],
        deadline: Duration,
        escalate_after: Duration,
        alive: impl Fn(u32) -> bool,
        signal_tree: impl Fn(u32, bool),
        released: impl Fn() -> bool,
        extra_targets: impl Fn() -> Vec<u32>,
    ) -> bool {
        let started = Instant::now();
        for pid in pids {
            signal_tree(*pid, false);
        }
        let mut escalated = false;
        loop {
            let survivors: Vec<u32> = pids.iter().copied().filter(|pid| alive(*pid)).collect();
            if survivors.is_empty() && released() {
                return true;
            }
            let elapsed = started.elapsed();
            if !escalated && elapsed >= escalate_after {
                for pid in &survivors {
                    signal_tree(*pid, true);
                }
                for pid in extra_targets() {
                    if !pids.contains(&pid) {
                        signal_tree(pid, true);
                    }
                }
                escalated = true;
            }
            if elapsed >= deadline {
                for pid in &survivors {
                    signal_tree(*pid, true);
                }
                for pid in extra_targets() {
                    if !pids.contains(&pid) {
                        signal_tree(pid, true);
                    }
                }
                thread::sleep(Self::STOP_POLL);
                return pids.iter().copied().all(|pid| !alive(pid)) && released();
            }
            thread::sleep(Self::STOP_POLL);
        }
    }

    /// Signal a process tree. `force` picks SIGKILL over SIGTERM on unix;
    /// Windows has only `taskkill /T /F`, re-issued on escalation.
    #[allow(unsafe_code)]
    fn signal_tree(pid: u32, force: bool) {
        if pid == 0 || pid == std::process::id() {
            return;
        }
        #[cfg(unix)]
        {
            let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
            // SAFETY: signal a pid this crate verified is a SUSI daemon or
            // leaf child (or a test's own fake child); no memory is shared.
            // The group form reaches leaf services the daemon spawned
            // without their own session; fall back to the single pid when
            // it is not a group leader (ESRCH).
            let group = unsafe { libc::kill(-(pid as i32), sig) };
            if group != 0 {
                unsafe {
                    libc::kill(pid as i32, sig);
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = force;
            let _ = susi_paths::console::background_command("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    /// Every loaded `susi-daemon*` user unit — instance siblings included.
    /// Best-effort: an absent systemctl or an unreadable unit list yields
    /// just this instance's unit so the common path still stops something.
    #[cfg(target_os = "linux")]
    fn daemon_units() -> Vec<String> {
        let own = format!("{}.service", Self::daemon_unit_name());
        let Ok(out) = Command::new("systemctl")
            .args([
                "--user",
                "list-units",
                "--all",
                "--no-legend",
                "--no-pager",
                "susi-daemon.service",
                "susi-daemon-*.service",
            ])
            .output()
        else {
            return vec![own];
        };
        let mut units: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .filter(|u| u.starts_with("susi-daemon") && u.ends_with(".service"))
            .map(str::to_string)
            .collect();
        if !units.iter().any(|u| u == &own) {
            units.push(own);
        }
        units
    }

    /// Whether a `daemon-start` pid belongs to this instance, by comparing
    /// the `SUSI_HOME` in its environment (`/proc/<pid>/environ` on Linux,
    /// read through sysinfo elsewhere) against ours.
    /// A daemon with no `SUSI_HOME` is the primary instance; one with a
    /// different root is a sibling and must survive `susi stop`.
    /// Unreadable environ (permission, pid reused) answers false — the
    /// scoped sweep errs toward leaving a foreign process alive.
    fn daemon_in_this_instance(pid: i32) -> bool {
        let Ok(pid) = u32::try_from(pid) else {
            return false;
        };
        let Some(theirs) = crate::susi_config::process_probe::environment_var(pid, "SUSI_HOME")
        else {
            return false;
        };
        let theirs = theirs.map(PathBuf::from);
        match (Self::instance_home(), theirs) {
            (Some(mine), Some(t)) => {
                // Compare canonicalized when both roots exist on disk (a
                // symlinked SUSI_HOME is still the same instance), falling
                // back to literal equality for not-yet-created roots.
                let canon = |p: &PathBuf| fs::canonicalize(p).unwrap_or_else(|_| p.clone());
                canon(&t) == canon(&mine)
            }
            (None, None) => true,
            (Some(_), None) | (None, Some(_)) => false,
        }
    }
}

/// Key-independent UDP readiness: `target` is bound when a datagram from a
/// connected loopback socket draws no ICMP port-unreachable within `wait`.
/// A connected socket surfaces port-unreachable as `ConnectionReset`
/// (Windows) or `ConnectionRefused` (Unix); silence (the daemon drops
/// unsigned datagrams) or any reply means something owns the port. This
/// cannot tell the daemon from a squatter, which only the signed probe can.
fn udp_port_bound(target: std::net::SocketAddr, wait: Duration) -> bool {
    use std::io::ErrorKind;
    let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
        return false;
    };
    if sock.connect(target).is_err() || sock.set_read_timeout(Some(wait)).is_err() {
        return false;
    }
    if sock.send(b"SUSI_READY_PROBE").is_err() {
        return false;
    }
    let mut buf = [0u8; 64];
    match sock.recv(&mut buf) {
        Ok(_) => true,
        Err(e) => matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
    }
}

/// File in the substrate home that carries provider keys into a
/// `systemd-run` unit; it lives only until the daemon is up.
#[cfg(target_os = "linux")]
const DAEMON_KEY_ENV_FILE: &str = ".daemon-start.env";

/// The `EnvironmentFile=` body that hands the caller's provider keys
/// (`*_API_KEY`) to a `systemd-run` unit, or `None` when there are none.
///
/// A transient unit gets the user manager's environment, not the caller's,
/// so a key exported in the shell that ran `susi start` never reached GEMI
/// on Linux (the double-fork path inherits it). Keys go through a file, never
/// `--setenv` on argv, where any local user could read them from
/// `/proc/<pid>/cmdline`. A value systemd would parse differently (a quote,
/// backslash or control character) is skipped by name, never written altered.
#[cfg(any(target_os = "linux", test))]
fn provider_key_env_body(vars: impl IntoIterator<Item = (String, String)>) -> Option<String> {
    let mut keys: Vec<(String, String)> = vars
        .into_iter()
        .filter(|(name, value)| {
            let named = name.len() > "_API_KEY".len()
                && name.ends_with("_API_KEY")
                && name
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_');
            let plain = !value.is_empty()
                && !value
                    .chars()
                    .any(|c| c.is_control() || matches!(c, '\'' | '"' | '\\'));
            if named && !plain && !value.is_empty() {
                warn!("[SusiDaemon] {name} not passed to the daemon: its value has a quote, backslash or control character");
            }
            named && plain
        })
        .collect();
    if keys.is_empty() {
        return None;
    }
    keys.sort();
    Some(
        keys.iter()
            .map(|(name, value)| format!("{name}='{value}'\n"))
            .collect(),
    )
}

/// Write [`provider_key_env_body`] for this process to `path`, readable by
/// the owner only. Returns whether the unit should read it.
#[cfg(target_os = "linux")]
fn write_provider_key_env_file(path: &Path) -> bool {
    use std::os::unix::fs::OpenOptionsExt;
    let vars = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)));
    let Some(body) = provider_key_env_body(vars) else {
        return false;
    };
    let _ = fs::remove_file(path);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut file| file.write_all(body.as_bytes()));
    if let Err(error) = written {
        warn!("[SusiDaemon] provider keys not passed to the daemon: {error}");
        let _ = fs::remove_file(path);
        return false;
    }
    true
}

/// A clone of a daemon socket that no child process inherits.
///
/// On Windows `try_clone` duplicates a socket through `WSADuplicateSocketW`,
/// and the duplicate comes back inheritable although the original is not
/// (measured on Windows 11 build 26300 with Rust 1.99), while every
/// `Command::spawn` creates its child with `bInheritHandles = TRUE`. The
/// supervised servers serve clones of the host-contract sockets, so every
/// leaf service, cell, probe and tool the daemon started held all five
/// ports, and a descendant that outlived the daemon (two hung
/// `ollama --version` processes on the CEO laptop, 2026-11-07) kept them
/// bound under the dead daemon's pid: every later daemon fell back to the
/// next port block. Clearing the inherit flag on each clone keeps the ports
/// with the daemon. Elsewhere `try_clone` already duplicates close-on-exec.
pub(crate) trait UninheritableClone: Sized {
    /// `try_clone`, with the inherit flag cleared on the new handle.
    fn clone_uninheritable(&self) -> std::io::Result<Self>;
}

impl UninheritableClone for std::net::TcpListener {
    fn clone_uninheritable(&self) -> std::io::Result<Self> {
        let clone = self.try_clone()?;
        #[cfg(windows)]
        forbid_inheritance(&clone)?;
        Ok(clone)
    }
}

impl UninheritableClone for std::net::UdpSocket {
    fn clone_uninheritable(&self) -> std::io::Result<Self> {
        let clone = self.try_clone()?;
        #[cfg(windows)]
        forbid_inheritance(&clone)?;
        Ok(clone)
    }
}

/// Clear `HANDLE_FLAG_INHERIT` on a socket handle.
#[cfg(windows)]
#[allow(unsafe_code)]
fn forbid_inheritance(socket: &impl std::os::windows::io::AsRawSocket) -> std::io::Result<()> {
    use winapi::um::handleapi::SetHandleInformation;
    use winapi::um::winnt::HANDLE;
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    // SAFETY: the raw socket is a live handle owned by `socket` for the whole
    // call; `SetHandleInformation` only clears its inherit bit.
    if unsafe { SetHandleInformation(socket.as_raw_socket() as HANDLE, HANDLE_FLAG_INHERIT, 0) }
        == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_keys_reach_a_systemd_unit_through_a_quoted_env_file() {
        let vars = [
            ("MOONSHOT_API_KEY", "sk-moon_2.x"),
            ("DEEPSEEK_API_KEY", "sk-deep"),
            ("PATH", "/usr/bin"),
            ("GITHUB_TOKEN", "ghp-not-a-provider-key"),
            ("_API_KEY", "nameless"),
            ("lower_api_key", "lowercase"),
            ("EMPTY_API_KEY", ""),
            ("QUOTED_API_KEY", "it's"),
            ("NEWLINE_API_KEY", "a\nB_API_KEY=injected"),
        ]
        .map(|(k, v)| (k.to_string(), v.to_string()));
        assert_eq!(
            provider_key_env_body(vars).as_deref(),
            Some("DEEPSEEK_API_KEY='sk-deep'\nMOONSHOT_API_KEY='sk-moon_2.x'\n")
        );
        assert_eq!(
            provider_key_env_body([("PATH".to_string(), "/bin".to_string())]),
            None
        );
    }

    /// The daemon half of [`a_dead_daemons_ports_are_free_while_its_child_lives`]:
    /// binds a TCP listener and a UDP socket, serves clones of them the way
    /// the supervised servers do, starts a long-lived child the way a leaf
    /// service, cell or probe starts, reports both ports and the child's pid,
    /// and exits without closing anything itself.
    #[test]
    #[ignore = "helper: plays a daemon that dies while its child lives"]
    #[allow(clippy::zombie_processes)] // the child must outlive this process
    fn daemon_that_dies_with_a_child_alive() {
        let Some(report) = std::env::var_os("SUSI_SOCKFIX_REPORT") else {
            return;
        };
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let served = (
            tcp.clone_uninheritable().unwrap(),
            udp.clone_uninheritable().unwrap(),
        );
        #[cfg(windows)]
        let mut child = Command::new("ping");
        #[cfg(windows)]
        child.args(["-n", "60", "127.0.0.1"]);
        #[cfg(not(windows))]
        let mut child = Command::new("sleep");
        #[cfg(not(windows))]
        child.arg("60");
        let child = child
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let line = format!(
            "{} {} {}",
            tcp.local_addr().unwrap().port(),
            udp.local_addr().unwrap().port(),
            child.id()
        );
        std::fs::write(report, line).unwrap();
        let _alive_until_exit = (tcp, udp, served);
        std::process::exit(0);
    }

    /// Defect 2026-11-08: a daemon's ports stayed bound after it died
    /// because a child it started inherited its socket clones. Once the
    /// daemon is gone its ports are free, while that child still runs.
    #[test]
    fn a_dead_daemons_ports_are_free_while_its_child_lives() {
        let report =
            std::env::temp_dir().join(format!("susi-dead-daemon-ports-{}", std::process::id()));
        let _ = std::fs::remove_file(&report);
        let mut daemon = Command::new(std::env::current_exe().unwrap());
        daemon
            .args([
                "--ignored",
                "--exact",
                "server::tests::daemon_that_dies_with_a_child_alive",
            ])
            .env("SUSI_SOCKFIX_REPORT", &report)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut daemon = daemon.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = daemon.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = daemon.kill();
                let _ = daemon.wait();
                panic!("the daemon stand-in did not exit within 60 s");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(status.success(), "daemon stand-in failed: {status}");
        let line = std::fs::read_to_string(&report).unwrap();
        let _ = std::fs::remove_file(&report);
        let fields: Vec<u32> = line
            .split_whitespace()
            .map(|f| f.parse().unwrap())
            .collect();
        let (tcp_port, udp_port, child) = (fields[0] as u16, fields[1] as u16, fields[2]);
        struct Reap(u32);
        impl Drop for Reap {
            fn drop(&mut self) {
                #[cfg(windows)]
                let _ = susi_paths::console::background_command("taskkill")
                    .args(["/F", "/PID", &self.0.to_string()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                #[cfg(not(windows))]
                let _ = Command::new("kill")
                    .args(["-9", &self.0.to_string()])
                    .status();
            }
        }
        let _reap = Reap(child);
        assert!(
            susi_core::service_table::pid_alive(child),
            "the child {child} must still run for this check to mean anything"
        );
        std::net::TcpListener::bind(("127.0.0.1", tcp_port)).unwrap_or_else(|e| {
            panic!("TCP {tcp_port} is still held after the daemon died (child {child}): {e}")
        });
        std::net::UdpSocket::bind(("127.0.0.1", udp_port)).unwrap_or_else(|e| {
            panic!("UDP {udp_port} is still held after the daemon died (child {child}): {e}")
        });
    }

    /// No extra port-holder targets — the fake-process tests need none.
    fn no_targets() -> Vec<u32> {
        Vec::new()
    }
    use susi_paths::ports;

    /// The spawned daemon runs in the substrate home, never in the cwd of
    /// the command that started it (an agent's worktree, where its
    /// cwd-relative work used to author task files).
    #[test]
    fn daemon_command_runs_in_the_substrate_home() {
        let home =
            std::env::temp_dir().join(format!("susi-daemon-cmd-home-{}", std::process::id()));
        // A home that does not exist yet is not entered (the spawn would
        // fail outright); the daemon creates and enters it on boot.
        let _ = std::fs::remove_dir_all(&home);
        let cmd = SusiDaemon::daemon_command(Path::new("susi"), &home);
        assert_eq!(cmd.get_current_dir(), None);
        std::fs::create_dir_all(&home).unwrap();
        let cmd = SusiDaemon::daemon_command(Path::new("susi"), &home);
        assert_eq!(cmd.get_current_dir(), Some(home.as_path()));
        let _ = std::fs::remove_dir_all(&home);
        let args: Vec<_> = cmd.get_args().collect();
        assert_eq!(
            args,
            vec![
                std::ffi::OsStr::new("daemon-start"),
                std::ffi::OsStr::new("--workspace"),
                home.as_os_str(),
            ]
        );
    }

    /// The daemon registers the providers its contract answers with — the
    /// configured cloud endpoints, the Candle fallback, the tool registry —
    /// right after the bind and ahead of discovery's catalog priming, not
    /// at discovery's step 1b a minute or more later (during which `/v1/
    /// models`, inference and pulses had no provider at all).
    #[test]
    fn contract_providers_register_before_discovery_primes_catalogs() {
        let server = include_str!("server.rs");
        let pipeline = include_str!("discovery_pipeline.rs");
        let after = |src: &str, from: usize, needle: &str| {
            src[from..]
                .find(needle)
                .map(|i| from + i)
                .unwrap_or_else(|| panic!("`{needle}` not found"))
        };
        // In the daemon loop: bind, then the providers one-shot (before the
        // leaf services even start), then discovery — which first waits
        // that one-shot out.
        let daemon = after(server, 0, "pub fn run_daemon_loop(");
        let bind = after(server, daemon, "Self::serve_host_contract(");
        let providers = after(
            server,
            daemon,
            "discovery_pipeline::register_contract_providers()",
        );
        let leaf = after(server, daemon, "crate::supervisor::start(");
        let discovery_worker = after(server, daemon, "\"zero-config-bootstrap\"");
        let waits = after(server, discovery_worker, ".join(CONTRACT_PROVIDERS)");
        let discovery = after(
            server,
            daemon,
            "discovery_pipeline::bootstrap_zero_config_substrate()",
        );
        assert!(
            bind < providers,
            "providers register once the contract is bound"
        );
        assert!(
            providers < leaf,
            "providers do not wait on the leaf services"
        );
        assert!(
            waits < discovery,
            "discovery waits for the providers before it runs"
        );
        // What "providers" means, and that it does no slow discovery itself.
        let start = after(pipeline, 0, "pub fn register_contract_providers()");
        let end = after(pipeline, start, "\n}\n");
        let body = &pipeline[start..end];
        for needed in [
            "register_configured_cloud_endpoints(registry)",
            "ensure_candle_fallback(registry)",
            "ToolRegistry::global()",
        ] {
            assert!(
                body.contains(needed),
                "contract providers must run `{needed}`"
            );
        }
        for slow in [
            "prime_catalogs",
            "auto_discover_local_engines",
            "local_ecosystem_report",
        ] {
            assert!(
                !body.contains(slow),
                "contract providers must not run `{slow}`"
            );
        }
        // Discovery primes catalogs; the providers are already in place.
        let bootstrap = after(
            pipeline,
            0,
            "pub async fn bootstrap_zero_config_substrate()",
        );
        assert!(after(pipeline, bootstrap, "prime_catalogs(&substrate)") > end);
    }

    #[test]
    fn endpoint_report_names_a_fallback_and_how_to_pin() {
        assert_eq!(SusiDaemon::offset_note(0, None), "");
        assert!(SusiDaemon::offset_note(300, None).contains("port_offset=300 (canonical base"));
        let fallback = susi_paths::ports::LivePorts {
            offset: 100,
            fallback_from: 0,
            pid: 7,
        };
        let note = SusiDaemon::offset_note(100, Some(fallback));
        assert!(
            note.contains("port_offset=100 (automatic fallback"),
            "{note}"
        );
        assert!(note.contains("SUSI_PORT_OFFSET"), "{note}");
        // A pinned env offset that differs from the published block is not
        // reported as the daemon's fallback.
        assert!(!SusiDaemon::offset_note(500, Some(fallback)).contains("fallback"));
    }

    #[test]
    fn host_contract_endpoint_report_lists_effective_ports() {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let report = SusiDaemon::host_contract_endpoints_report();
        assert!(report.contains(&format!(":{}/mcp", cfg.gmcp_port())));
        assert!(report.contains(&format!(":{}/", cfg.gemi_port())));
        assert!(report.contains(&format!(":{}", cfg.udp_discovery_port())));
        assert!(report.contains(&format!(":{}/mcp", cfg.gmcp_http_port())));
        assert!(report.contains(&format!(":{}/", cfg.a2a_http_port())));
    }

    #[test]
    fn wait_for_host_contract_times_out_when_nothing_listens() {
        // Bind the contract ports ourselves only if they are free — otherwise
        // skip: a live local daemon would make "timeout when down" untestable.
        if SusiDaemon::host_contract_tcp_ready() {
            return;
        }
        let ready = SusiDaemon::wait_for_host_contract(Duration::from_millis(250));
        assert!(
            !ready,
            "wait_for_host_contract must return false when nothing owns the host-contract ports"
        );
    }

    /// B7: the legacy unauthenticated discovery dialects get silence. Only
    /// unsigned datagrams are sent, so the prefix gate drops each one before
    /// `cluster.key` is ever read or created.
    #[test]
    fn udp_discovery_never_answers_unsigned_pings() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = server.local_addr().unwrap();
        std::thread::spawn(move || SusiDaemon::start_udp_discovery_server(server));
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut buf = [0u8; 512];
        for probe in [
            "SUSI_LAN_PING",
            "SUSI_PING",
            "SUSI_PING:CORE:0:0000000000000000",
            "hello",
        ] {
            client.send_to(probe.as_bytes(), addr).unwrap();
            let reply = client.recv_from(&mut buf);
            assert!(
                reply.is_err(),
                "{probe:?} must get no reply, got {:?}",
                reply.map(|(n, _)| String::from_utf8_lossy(&buf[..n]).into_owned())
            );
        }
    }

    /// Unsigned-mode readiness: a bound, silent port is ready; a port with
    /// nothing bound (ICMP port-unreachable) is not.
    #[test]
    fn udp_port_bound_tells_bound_from_unbound() {
        let wait = Duration::from_millis(300);
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(super::udp_port_bound(silent.local_addr().unwrap(), wait));
        let gone = UdpSocket::bind("127.0.0.1:0").unwrap();
        let gone_addr = gone.local_addr().unwrap();
        drop(gone);
        assert!(!super::udp_port_bound(gone_addr, wait));
        drop(silent);
    }

    #[test]
    fn unit_name_is_instance_scoped() {
        assert_eq!(SusiDaemon::unit_name_for(None), "susi-daemon");
        let a = SusiDaemon::unit_name_for(Some(Path::new("/home/x/.susi-a")));
        let b = SusiDaemon::unit_name_for(Some(Path::new("/home/x/.susi-b")));
        assert!(a.starts_with("susi-daemon-"));
        assert!(b.starts_with("susi-daemon-"));
        assert_ne!(a, b, "distinct SUSI_HOME roots must get distinct units");
    }

    #[test]
    fn test_lock_file_path() {
        let tmp_dir = std::env::temp_dir();
        let path = SusiDaemon::get_lock_file(&tmp_dir);
        assert_eq!(path, tmp_dir.join("substrate.lock"));
    }

    #[test]
    fn shutdown_interrupts_supervision_wait_without_shortening_its_cadence() {
        let shutdown = Arc::new(AtomicBool::new(false));
        let start = Instant::now();
        wait_for_shutdown(&shutdown, Duration::from_millis(30));
        assert!(start.elapsed() >= Duration::from_millis(30));
        let cancel = Arc::clone(&shutdown);
        let signal = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            cancel.store(true, Ordering::Release);
        });
        let start = Instant::now();
        wait_for_shutdown(&shutdown, crate::service_supervision::SUPERVISE_INTERVAL);
        signal.join().unwrap();
        assert!(start.elapsed() < Duration::from_secs(1));
        let start = Instant::now();
        wait_for_shutdown(&shutdown, crate::service_supervision::SUPERVISE_INTERVAL);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn canonical_ports_match_public_contract() {
        assert_eq!(ports::GMCP, 9090);
        assert_eq!(ports::GEMI, 9091);
        assert_eq!(ports::UDP_DISCOVERY, 9092);
        assert_eq!(ports::GMCP_HTTP, 9093);
        assert_eq!(ports::A2A_HTTP, 9094);
        let cfg = SusiConfig::default();
        let offset = ports::env_port_offset();
        assert_eq!(cfg.gmcp_port(), ports::GMCP + offset);
        assert_eq!(cfg.gemi_port(), ports::GEMI + offset);
        assert_eq!(cfg.udp_discovery_port(), ports::UDP_DISCOVERY + offset);
        assert_eq!(cfg.gmcp_http_port(), ports::GMCP_HTTP + offset);
        assert_eq!(cfg.a2a_http_port(), ports::A2A_HTTP + offset);
    }

    #[test]
    fn test_global_lock_is_mutually_exclusive() {
        let global_dir =
            std::env::temp_dir().join(format!("susi_global_lock_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&global_dir);
        let global_lock_path = SusiDaemon::get_lock_file(&global_dir);
        let _ = std::fs::remove_file(&global_lock_path);

        let mut first = DaemonLock::acquire(&global_lock_path);
        assert!(
            first.is_ok(),
            "the first daemon must be able to acquire the global lock"
        );
        let _ = first
            .as_mut()
            .map(|l| l.write_pid(&global_dir))
            .expect("lock");

        let second = DaemonLock::acquire(&global_lock_path);
        assert!(
            second.is_err(),
            "a second daemon must be refused the global lock while the first is still running"
        );
        // A contended acquire must not truncate the holder's pid lines —
        // a wiped lock file makes check_status report "not running" for a
        // live daemon.
        let contents = read_lock_record(&global_lock_path).unwrap_or_default();
        assert!(
            !contents.trim().is_empty(),
            "contended acquire wiped the lock file"
        );

        drop(first);
        let third = DaemonLock::acquire(&global_lock_path);
        assert!(
            third.is_ok(),
            "once the first daemon releases the lock, a new one must be able to acquire it — got {:?}",
            third.as_ref().err()
        );

        let _ = std::fs::remove_dir_all(&global_dir);
    }

    #[test]
    fn test_find_running_daemon_reports_substrate_home() {
        let global_dir =
            std::env::temp_dir().join(format!("susi_find_running_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&global_dir);
        std::fs::create_dir_all(&global_dir).unwrap();

        let substrate = global_dir.join("substrate_home");
        std::fs::create_dir_all(&substrate).unwrap();

        let global_lock_path = SusiDaemon::get_lock_file(&global_dir);
        let mut global_lock = DaemonLock::acquire(&global_lock_path).unwrap();
        global_lock.write_pid(&substrate).unwrap();

        let found = SusiDaemon::find_running_daemon(&global_dir)
            .expect("host daemon must be visible via the global lock");
        assert_eq!(found.substrate_home, substrate);
        assert_eq!(found.pid, std::process::id());

        let _ = std::fs::remove_dir_all(&global_dir);
    }

    #[test]
    fn test_daemon_status_and_lifecycle_when_not_running() {
        let tmp_dir = std::env::temp_dir();
        let lock_file = SusiDaemon::get_lock_file(&tmp_dir);
        let _ = std::fs::remove_file(&lock_file);

        let status = SusiDaemon::check_status(&tmp_dir, &tmp_dir);
        assert!(status.is_none());

        let stopped = SusiDaemon::stop_daemon(&tmp_dir, &tmp_dir);
        assert!(!stopped);
    }

    #[test]
    fn tasklist_no_match_is_not_a_live_pid() {
        // `tasklist` exits 0 for "no tasks match" too; only a parsed pid
        // field may answer "alive" (the old exit-status check reported every
        // stale lock pid as a running daemon, so stop could never observe an
        // exit on Windows).
        let no_match = b"INFO: No tasks are running which match the specified criteria.\r\n";
        assert_eq!(SusiDaemon::tasklist_image_name(no_match, 1234), None);

        let hit = b"\"susi.exe\",\"1234\",\"Console\",\"1\",\"12,345 K\"\r\n";
        assert_eq!(
            SusiDaemon::tasklist_image_name(hit, 1234).as_deref(),
            Some("susi.exe")
        );
        // A different pid whose decimal representation contains 1234 must not
        // be confused with 1234 (the old substring-free filter was safe here,
        // but the parser must stay exact).
        assert_eq!(SusiDaemon::tasklist_image_name(hit, 123), None);
    }

    #[test]
    fn netstat_parsing_finds_only_listeners_on_the_contract_ports() {
        let sample = b"Active Connections\r\n\
            \x20 TCP    127.0.0.1:9190         0.0.0.0:0              LISTENING       4321\r\n\
            \x20 TCP    127.0.0.1:9191         0.0.0.0:0              LISTENING       4321\r\n\
            \x20 TCP    127.0.0.1:9100         0.0.0.0:0              LISTENING       7777\r\n\
            \x20 TCP    127.0.0.1:9193         127.0.0.1:52000        ESTABLISHED     9999\r\n\
            \x20 UDP    127.0.0.1:9192         *:*                                    4321\r\n";
        assert_eq!(
            SusiDaemon::parse_netstat_listeners(sample, &[9190, 9191, 9193]),
            vec![4321],
            "only TCP LISTENING rows on a requested port identify a holder"
        );
    }

    #[test]
    fn process_alive_is_false_for_an_impossible_pid() {
        assert!(!SusiDaemon::process_alive(0));
        assert!(
            !SusiDaemon::process_alive(4_000_000_000),
            "an unused pid must not read as alive — stop's wait depends on it"
        );
    }

    /// Liveness is asked of the OS in-process, never by launching a process
    /// per probe: a stop polls it, and on Windows every probe used to spawn
    /// `tasklist` — tens to hundreds of milliseconds each. A hundred probes
    /// stay far inside what even a handful of launches take.
    #[test]
    fn process_alive_launches_no_process_per_probe() {
        let own = std::process::id();
        let start = Instant::now();
        for _ in 0..50 {
            assert!(SusiDaemon::process_alive(own), "this process is alive");
            assert!(!SusiDaemon::process_alive(4_000_000_000));
        }
        let took = start.elapsed();
        assert!(
            took < Duration::from_secs(2),
            "100 liveness probes took {took:?}: each one launches a process"
        );
    }

    /// Windows answers exactly what the process probe answers: a running
    /// process is alive, and one that exited is gone even while its handle
    /// is still held (the `Child` here, until `wait`).
    #[cfg(windows)]
    #[test]
    fn process_alive_is_the_process_probe_on_windows() {
        use crate::susi_config::process_probe::pid_alive;
        let own = std::process::id();
        assert_eq!(pid_alive(own), Some(true));
        assert!(SusiDaemon::process_alive(own));

        let mut child = Command::new("cmd")
            .args(["/C", "exit 0"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(15);
        while pid_alive(pid) == Some(true) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(pid_alive(pid), Some(false), "the child exited");
        assert!(
            !SusiDaemon::process_alive(pid),
            "an exited child must read as gone while its handle is held"
        );
        let _ = child.wait();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_zombie_child_is_not_alive() {
        // A SIGKILLed, unreaped child stays in /proc as state `Z`, where
        // `kill(pid, 0)` still succeeds. Reading that as alive is what made a
        // stop unable to observe its own kill.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(
            SusiDaemon::process_alive(pid),
            "a live child must read alive"
        );
        SusiDaemon::signal_tree(pid, true);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while SusiDaemon::process_alive(pid) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            SusiDaemon::linux_process_state(pid),
            Some('Z'),
            "the killed child is an unreaped zombie"
        );
        assert!(
            !SusiDaemon::process_alive(pid),
            "a zombie must not read as a live process"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn await_stop_waits_for_exit_and_escalates_once() {
        use std::cell::Cell;
        let alive = Cell::new(true);
        let released = Cell::new(false);
        let signals = Cell::new(0u32);
        let forced = Cell::new(0u32);
        let outcome = SusiDaemon::await_stop(
            &[4242],
            Duration::from_millis(1500),
            Duration::from_millis(100),
            |_| alive.get(),
            |_, force| {
                signals.set(signals.get() + 1);
                if force {
                    forced.set(forced.get() + 1);
                    alive.set(false);
                    released.set(true);
                }
            },
            || released.get(),
            no_targets,
        );
        assert!(outcome, "a fake daemon that dies on escalation is a stop");
        assert_eq!(
            signals.get(),
            2,
            "exactly one initial termination and one escalation — the wait must \
             not busy-signal a survivor"
        );
        assert_eq!(forced.get(), 1);
    }

    #[test]
    fn await_stop_reports_failure_when_ports_never_release() {
        let outcome = SusiDaemon::await_stop(
            &[7],
            Duration::from_millis(400),
            Duration::from_millis(100),
            |_| false,
            |_, _| {},
            || false,
            no_targets,
        );
        assert!(
            !outcome,
            "a process that exited but left the contract ports listening is not a successful stop"
        );
    }

    #[test]
    fn await_stop_with_no_targets_is_immediate_when_ports_are_free() {
        let started = Instant::now();
        let outcome = SusiDaemon::await_stop(
            &[],
            Duration::from_secs(5),
            Duration::from_secs(1),
            |_| false,
            |_, _| {},
            || true,
            no_targets,
        );
        assert!(outcome);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "the nothing-to-stop path must not wait out the deadline"
        );
    }

    #[test]
    fn a_real_fake_child_is_detected_then_tree_killed() {
        // A short-lived command stands in for the daemon: no real daemon is
        // required to prove liveness detection and bounded termination.
        let mut cmd = if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "ping -n 60 127.0.0.1"]);
            cmd
        } else {
            let mut cmd = Command::new("sleep");
            cmd.arg("60");
            cmd
        };
        let mut child = cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(
            SusiDaemon::process_alive(pid),
            "a live fake child must be detected as alive"
        );

        SusiDaemon::signal_tree(pid, true);
        let deadline = Instant::now() + Duration::from_secs(15);
        while SusiDaemon::process_alive(pid) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(100));
        }
        let alive_after = SusiDaemon::process_alive(pid);
        let _ = child.kill();
        let _ = child.wait();
        assert!(
            !alive_after,
            "forced tree termination must end the fake child within the deadline"
        );
    }

    #[test]
    fn test_binary_hash_cache_hits_on_unchanged_file_and_misses_after_real_change() {
        let global_dir =
            std::env::temp_dir().join(format!("susi_hash_cache_test_{}", std::process::id()));
        std::fs::create_dir_all(&global_dir).unwrap();
        let bin_path = global_dir.join("fake_binary");
        std::fs::write(&bin_path, b"version one content").unwrap();

        let direct_hash =
            crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&bin_path)
                .unwrap();
        let cached_hash_1 =
            crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash_cached(
                &bin_path,
                &global_dir,
            )
            .unwrap();
        assert_eq!(
            direct_hash, cached_hash_1,
            "the cached path must still produce the exact same digest as a direct hash"
        );

        // Corrupt the sidecar's stored hash directly, bypassing any real
        // rehash, to prove a second call with the SAME mtime+size serves the
        // cached (now-wrong) value rather than silently re-hashing — this is
        // what actually saves the I/O, not just "returns a correct value".
        let cache_path = global_dir.join("binary.hash.cache");
        let cache_content = std::fs::read_to_string(&cache_path).unwrap();
        let mut parts = cache_content.trim().splitn(3, ':');
        let mtime = parts.next().unwrap();
        let size = parts.next().unwrap();
        std::fs::write(&cache_path, format!("{}:{}:deadbeef", mtime, size)).unwrap();

        let cached_hash_2 =
            crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash_cached(
                &bin_path,
                &global_dir,
            )
            .unwrap();
        assert_eq!(
            cached_hash_2, "deadbeef",
            "an unchanged mtime+size must be served from the cache, not re-hashed"
        );

        // Now genuinely change the file's content. Even if the filesystem
        // happens to report the same mtime (fast successive writes on some
        // filesystems), a real re-hash after a content change must never
        // keep returning the stale "deadbeef" sentinel forever.
        std::thread::sleep(std::time::Duration::from_millis(5));
        std::fs::write(&bin_path, b"version two, genuinely different content").unwrap();
        let cached_hash_3 =
            crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash_cached(
                &bin_path,
                &global_dir,
            )
            .unwrap();
        assert_ne!(
            cached_hash_3, "deadbeef",
            "a real content+mtime change must invalidate the cache and re-hash"
        );
        assert_eq!(
            cached_hash_3,
            crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&bin_path)
                .unwrap()
        );

        let _ = std::fs::remove_dir_all(&global_dir);
    }

    #[test]
    fn test_is_trusted_susi_process_fails_closed_without_hash_file() {
        let global_dir =
            std::env::temp_dir().join(format!("susi_trust_test_nohash_{}", std::process::id()));
        std::fs::create_dir_all(&global_dir).unwrap();

        // Own PID's exe is the test binary, not literally named "susi"/"susi",
        // so this exercises the name-mismatch branch regardless of hash state.
        let own_pid = std::process::id() as i32;
        assert!(!SusiDaemon::is_trusted_susi_process(own_pid, &global_dir));

        let _ = std::fs::remove_dir_all(&global_dir);
    }

    /// Serializes the tests whose children outlive a pipe another test is
    /// timing. Windows children inherit every inheritable handle in the
    /// process, so a long-lived child started while a pipe test runs holds
    /// that test's pipe open and reads as the leak the pipe tests guard
    /// against.
    static CHILD_PIPE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn child_pipe_guard() -> std::sync::MutexGuard<'static, ()> {
        CHILD_PIPE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn test_is_trusted_susi_process_matches_only_the_trusted_hash() {
        let _serial = child_pipe_guard();
        let base = std::env::temp_dir().join(format!("susi_trust_test_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let global_dir = base.join("global");
        std::fs::create_dir_all(&global_dir).unwrap();

        // A real file literally named "susi" standing in for the trusted
        // binary, so /proc/{pid}/exe's basename check has something to match.
        // Copy OUR OWN test binary, not /bin/sleep: on some runner images
        // sleep is a dispatch-style binary that exits as soon as argv[0] is
        // not a recognised command, leaving /proc/{pid}/exe gone.
        let fake_bin = base.join(if cfg!(windows) { "susi.exe" } else { "susi" });
        std::fs::copy(std::env::current_exe().unwrap(), &fake_bin).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_bin, perms).unwrap();
        }

        // Run the helper test, which blocks on stdin: the child stays alive
        // until we close the pipe, so the assertions always see a live process.
        // A just-copied binary can be briefly busy on unix (ETXTBSY);
        // Windows has no such state.
        #[cfg(unix)]
        let text_busy = Some(libc::ETXTBSY);
        #[cfg(not(unix))]
        let text_busy: Option<i32> = None;
        let mut child = {
            let mut attempt = 0;
            loop {
                match Command::new(&fake_bin)
                    .args([
                        "--ignored",
                        "--exact",
                        "server::tests::block_until_stdin_closes",
                    ])
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                {
                    Ok(c) => break c,
                    Err(e)
                        if text_busy.is_some() && e.raw_os_error() == text_busy && attempt < 20 =>
                    {
                        attempt += 1;
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(e) => panic!("spawn fake susi: {e}"),
                }
            }
        };
        let pid = child.id() as i32;
        // Give the kernel a moment to populate /proc/{pid}/exe.
        thread::sleep(Duration::from_millis(50));

        // No trust anchor written yet: fails closed even though the name matches.
        assert!(!SusiDaemon::is_trusted_susi_process(pid, &global_dir));

        // Trust anchor matches the real binary's hash: now trusted.
        let real_hash =
            crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&fake_bin)
                .unwrap();
        std::fs::write(SusiDaemon::get_hash_file(&global_dir), &real_hash).unwrap();
        // Diagnose which check fails when the assertion trips: the /proc exe
        // target (name gate), its hash (hash gate), and the instance env.
        let exe = crate::susi_config::process_probe::exe_path(pid as u32);
        let exe_hash = exe.as_ref().and_then(|p| {
            crate::susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(p).ok()
        });
        assert!(
            SusiDaemon::is_trusted_susi_process(pid, &global_dir),
            "trusted hash must match: exe={exe:?} exe_hash={exe_hash:?} trusted={real_hash} susi_home={:?}",
            std::env::var_os("SUSI_HOME")
        );

        // Trust anchor stale/mismatched: no longer trusted.
        std::fs::write(SusiDaemon::get_hash_file(&global_dir), "not-the-real-hash").unwrap();
        assert!(!SusiDaemon::is_trusted_susi_process(pid, &global_dir));

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Helper for [`test_is_trusted_susi_process_matches_only_the_trusted_hash`]:
    /// a fake "susi" child runs this test so it blocks on stdin and stays alive
    /// until the parent closes the pipe. Ignored so it never runs in the suite.
    #[test]
    #[ignore = "helper: keeps the trust test's fake child alive until its stdin closes"]
    fn block_until_stdin_closes() {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    #[test]
    #[ignore = "helper: spawns a long-lived background child and exits so parent pipe EOF is tested"]
    #[allow(clippy::zombie_processes)] // helper intentionally exits while its short-lived grandchild is still running
    fn spawn_background_grandchild_and_exit() {
        let exe = std::env::current_exe().unwrap();
        let mut cmd = Command::new(exe);
        cmd.args([
            "--ignored",
            "--exact",
            "server::tests::sleep_briefly_for_pipe_test",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        let child = SusiDaemon::spawn_without_inheriting_stdio(&mut cmd).unwrap();
        println!("grandchild_pid={}", child.id());
        eprintln!("mission stderr remains visible");
        if std::env::var("SUSI_PIPE_TEST_EXIT").as_deref() == Ok("7") {
            std::process::exit(7);
        }
    }

    #[test]
    #[ignore = "helper: shell forwards mission output without leaking pipe aliases"]
    fn shell_forwards_mission_and_exit_status() {
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--ignored",
            "--exact",
            "server::tests::spawn_background_grandchild_and_exit",
            "--nocapture",
        ])
        .stdin(std::process::Stdio::null());
        let status = SusiDaemon::spawn_without_inheriting_stdio(&mut cmd)
            .unwrap()
            .wait()
            .unwrap();
        std::process::exit(status.code().unwrap_or(1));
    }

    #[test]
    fn spawn_without_inheriting_stdio_preserves_shell_output_and_status() {
        let _serial = child_pipe_guard();
        for exit in ["0", "7"] {
            let started = Instant::now();
            let out = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "server::tests::shell_forwards_mission_and_exit_status",
                    "--nocapture",
                ])
                .env("SUSI_PIPE_TEST_EXIT", exit)
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(exit.parse().unwrap()));
            assert!(String::from_utf8_lossy(&out.stdout).contains("grandchild_pid="));
            assert!(
                String::from_utf8_lossy(&out.stderr).contains("mission stderr remains visible")
            );
            assert!(
                started.elapsed() < Duration::from_secs(3),
                "shell pipes remained open until detached child exited"
            );
        }
    }

    #[test]
    #[ignore = "helper: stays alive long enough to expose leaked parent stdio pipes"]
    fn sleep_briefly_for_pipe_test() {
        thread::sleep(Duration::from_secs(5));
    }

    /// Regression: when a short-lived CLI invocation spawns `daemon-start` in
    /// the background (`Stdio::null()`), the long-lived grandchild must not
    /// inherit the CLI's own stdout/stderr pipe handles and block callers
    /// (`Command::output()`, release smoke checks, shell pipes) waiting for EOF.
    #[test]
    fn spawn_without_inheriting_stdio_closes_parent_pipes_while_child_lives() {
        let _serial = child_pipe_guard();
        let exe = std::env::current_exe().unwrap();
        let started = Instant::now();
        let out = Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "server::tests::spawn_background_grandchild_and_exit",
                "--nocapture",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .unwrap();
        let elapsed = started.elapsed();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "helper failed: {stdout}");
        assert!(
            elapsed < Duration::from_secs(3),
            "parent pipe blocked for {elapsed:?} waiting for 5s background grandchild to exit"
        );
    }
}
