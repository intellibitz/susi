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

pub struct DaemonContext {
    pub shutdown_signal: Arc<AtomicBool>,
    _lock: DaemonLock,
}

impl DaemonContext {
    pub fn new(lock: DaemonLock) -> Self {
        DaemonContext {
            shutdown_signal: Arc::new(AtomicBool::new(false)),
            _lock: lock,
        }
    }

    pub fn setup_signal_handlers(&self) -> Result<(), EaiError> {
        let shutdown = Arc::clone(&self.shutdown_signal);
        thread::spawn(move || {
            if let Ok(mut signals) = Signals::new([SIGTERM, SIGINT]) {
                for sig in signals.forever() {
                    let msgs = crate::susi_sandbox::manager::SusiMessages::load_global();
                    let def_msg = "[SusiDaemon] Received signal: {}".to_string();
                    let msg = msgs.get("daemon", "signal_received").unwrap_or(&def_msg);
                    eprintln!("{}", msg.replace("{}", &sig.to_string()));
                    shutdown.store(true, Ordering::Release);
                }
            }
        });
        Ok(())
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_signal.load(Ordering::Acquire)
    }
}

/// A running host daemon. Always bound to [`crate::susi_paths::SusiDirs::substrate_home`];
/// project context is per-request cwd, never the daemon's boot path.
pub struct RunningDaemon {
    pub pid: u32,
    /// Substrate home recorded in the lock (diagnostic / identity only).
    pub substrate_home: PathBuf,
}

pub struct DaemonLock {
    file: fs::File,
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

        Ok(Self { file })
    }

    /// Writes `pid\nsubstrate_home`. Second line is host identity only —
    /// never a project cwd.
    fn write_pid(&mut self, substrate_home: &Path) -> Result<(), String> {
        self.file.set_len(0).map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        write!(
            self.file,
            "{}\n{}",
            std::process::id(),
            substrate_home.display()
        )
        .map_err(|e| e.to_string())?;
        self.file.flush().map_err(|e| e.to_string())?;
        Ok(())
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

    /// True when the public host-contract TCP surfaces (canonical 9090-9094
    /// plus any `port_offset`) accept connections.
    pub fn host_contract_tcp_ready() -> bool {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        [
            cfg.gmcp_port(),
            cfg.gemi_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port(),
        ]
        .iter()
        .all(|port| {
            TcpStream::connect_timeout(
                &std::net::SocketAddr::from(([127, 0, 0, 1], *port)),
                Duration::from_millis(200),
            )
            .is_ok()
        })
    }

    /// True when UDP discovery answers a LAN ping (daemon owns the port).
    pub fn host_contract_udp_ready() -> bool {
        let cfg = crate::susi_sandbox::manager::SusiConfig::load_global().unwrap_or_default();
        let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
            return false;
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.udp_discovery_port()));
        if sock.send_to(b"SUSI_LAN_PING", target).is_err() {
            return false;
        }
        let mut buf = [0u8; 256];
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                let msg = String::from_utf8_lossy(&buf[..n]);
                msg.contains("SUSI_LAN_PONG")
            }
            Err(_) => false,
        }
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
            let https_only = cfg.get::<bool>("https_only").unwrap_or(false);
            format!(
                "\nRemote surface: http+https on {bind}:* (TLS auto-provisioned;\
                 https_only={https_only} {} remote plaintext)",
                if https_only { "refuses" } else { "permits" }
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
            if cfg.port_offset() == 0 {
                String::new()
            } else {
                format!(
                    "\nport_offset={} (canonical base 9090–9094)",
                    cfg.port_offset()
                )
            }
        )
    }

    /// Locate the single host daemon, if running.
    pub fn find_running_daemon(global_dir: &Path) -> Option<RunningDaemon> {
        let global_lock = Self::get_lock_file(global_dir);
        Self::check_status_path(&global_lock).map(|pid| {
            let substrate_home = Self::read_recorded_substrate_home(&global_lock)
                .unwrap_or_else(crate::susi_paths::SusiDirs::substrate_home);
            RunningDaemon {
                pid,
                substrate_home,
            }
        })
    }

    fn read_recorded_substrate_home(lock_file_path: &Path) -> Option<PathBuf> {
        let content = fs::read_to_string(lock_file_path).ok()?;
        let mut lines = content.lines();
        lines.next()?; // PID line
        lines.next().map(PathBuf::from)
    }

    #[allow(unsafe_code)]
    pub fn check_status_path(lock_file_path: &Path) -> Option<u32> {
        if !lock_file_path.exists() {
            return None;
        }

        if let Ok(content) = fs::read_to_string(lock_file_path)
            && let Ok(pid) = content.lines().next().unwrap_or("").trim().parse::<u32>()
        {
            #[cfg(unix)]
            {
                // SAFETY: kill(pid, 0) sends no signal; it only probes whether the pid exists.
                if unsafe { libc::kill(pid as i32, 0) } == 0 {
                    return Some(pid);
                } else {
                    let _ = fs::remove_file(lock_file_path);
                    return None;
                }
            }
            #[cfg(windows)]
            {
                if Self::is_process_alive(lock_file_path) {
                    return Some(pid);
                } else {
                    let _ = fs::remove_file(lock_file_path);
                    return None;
                }
            }
        }
        None
    }

    #[allow(unsafe_code)]
    fn is_process_alive(lock_file_path: &Path) -> bool {
        if let Ok(content) = fs::read_to_string(lock_file_path)
            && let Ok(pid) = content.lines().next().unwrap_or("").trim().parse::<u32>()
        {
            #[cfg(unix)]
            {
                // SAFETY: kill(pid, 0) sends no signal; it only probes whether the pid exists.
                return unsafe { libc::kill(pid as i32, 0) == 0 };
            }
            #[cfg(windows)]
            {
                use std::process::Command;
                return Command::new("tasklist")
                    .args(&["/FI", &format!("PID eq {}", pid)])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            }
        }
        false
    }

    pub fn get_hash_file(global_dir: &Path) -> PathBuf {
        global_dir.join("binary.hash")
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
    /// [`crate::susi_paths::SusiDirs::substrate_home`]. The caller's project cwd is
    /// irrelevant here — work context is attached per intent, not to the daemon.
    #[allow(unsafe_code)]
    pub fn ensure_daemon_running(_caller_cwd: &Path, global_dir: &Path) {
        let substrate_home = crate::susi_paths::SusiDirs::substrate_home();
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
            // a single 7B Q4 model needs ~5G, and the leaf services + rayon
            // swarm share the same cgroup. Floor at 8G/200%, then a quarter
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
            let spawned_via_systemd = run
                .arg(format!(
                    "--property=MemoryMax={}G",
                    mem_cap / (1024 * 1024 * 1024)
                ))
                .arg(format!("--property=CPUQuota={cpu_quota}%"))
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
                return;
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            use std::process::Stdio;
            let mut cmd = Command::new(&bin_to_run);
            cmd.arg("daemon-start")
                .arg("--workspace")
                .arg(substrate_home.to_str().unwrap_or("."))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: `pre_exec` requires the closure to only call
            // async-signal-safe functions, since it runs in the forked child
            // between fork() and execve() with the rest of this (possibly
            // multi-threaded, e.g. the rayon pool) process's threads gone but
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
            if let Ok(mut child) = cmd.spawn() {
                let _ = child.wait();
            }
            if !Self::wait_for_host_contract(Duration::from_secs(10)) {
                warn!("[SusiDaemon] daemon spawned but host contract never came up");
            }
        }

        #[cfg(not(unix))]
        {
            use std::process::Stdio;
            let mut cmd = Command::new(&bin_to_run);
            cmd.arg("daemon-start")
                .arg("--workspace")
                .arg(substrate_home.to_str().unwrap_or("."))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _ = cmd.spawn();
            if !Self::wait_for_host_contract(Duration::from_secs(10)) {
                warn!("[SusiDaemon] daemon spawned but host contract never came up");
            }
        }
    }

    pub fn run_daemon_loop(_legacy_workspace_arg: PathBuf, global_dir: PathBuf) {
        if cfg!(test) {
            return;
        }
        // Daemon is always bound to substrate home — ignore any stale
        // project-cwd passed via --workspace for backwards compatibility.
        let workspace = crate::susi_paths::SusiDirs::substrate_home();
        let _ = std::fs::create_dir_all(&workspace);
        // Zero-trust: seed host bearer token before opening world-facing ports.
        let _ = crate::susi_sandbox::manager::SusiConfig::ensure_api_auth_token_seeded();
        // Composition root: EngineHooks before any ToolRegistry / MCP dispatch.
        crate::composition::wire_engine_hooks();
        susi_core::context_graph::ContextGraph::init_global_storage(
            workspace.join("context_graph.jsonl"),
        );
        crate::privacy::wire_mac_policy(&workspace);
        crate::ambient::start_ambient_indexer(&workspace);
        // Zero-config cloud keys for always-on / systemd spawns (no shell env).
        susi_gemi::http_provider::apply_cloud_env_file();
        let lock_file_path = Self::get_lock_file(&global_dir);

        // Ensure lock file is cleaned if stale (> 1 hour old and process is dead)
        if let Ok(metadata) = std::fs::metadata(&lock_file_path)
            && let Ok(modified) = metadata.modified()
            && let Ok(age) = modified.elapsed()
            && age.as_secs() > 3600
            && !Self::is_process_alive(&lock_file_path)
        {
            let _ = std::fs::remove_file(&lock_file_path);
        }

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

        let ctx = DaemonContext::new(lock);
        if let Err(e) = ctx.setup_signal_handlers() {
            eprintln!("[SusiDaemon] Signal handler setup failed: {}", e);
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
        // Mandate: config may have been polluted by older "port randomization"
        // self-healing — always restore the public port contract before bind.
        Self::force_canonical_ports(&mut cfg);
        let _ = cfg.save(&global_dir);

        let bind_address = crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .get("bind_address")
            .unwrap_or_else(|| "127.0.0.1".to_string());

        // TLS for the public endpoints: configured cert/key, or an
        // auto-generated self-signed cert whenever the bind is reachable
        // off-host. Each listener sniffs the first byte per connection, so
        // plain HTTP (internal loopback callers) keeps working on the same
        // port. `https_only` drops non-TLS bytes from remote peers.
        let tls_acceptor = crate::tls::endpoint_acceptor(&bind_address, &global_dir);
        let require_tls_remote = crate::tls::https_only();

        // Initialize Global Stigmergic Blackboard (Swarm OS Points 5, 21, 25, 31)
        let blackboard = crate::blackboard::SwarmBlackboard::new();
        let blackboard_evaporator = blackboard.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                blackboard_evaporator.evaporate_pheromones();
            }
        });

        // Substrate Administration & Hardware Optimization (Pillar 1)
        crate::runtime_admin::SusiRuntimeAdmin::start_administration_cycle(
            &workspace,
            blackboard.clone(),
        );

        // Pillar 8: Zero-config discovery of local inference engines + MCP tools
        // run_daemon_loop is sync (invoked from CLI `daemon-start`); spin up a
        // short-lived runtime for the async probes, matching GMCP/GEMI bind paths.
        match tokio::runtime::Runtime::new() {
            Ok(runtime) => {
                runtime.block_on(crate::discovery_pipeline::bootstrap_zero_config_substrate());
            }
            Err(e) => {
                eprintln!(
                    "[SusiDaemon] Zero-config substrate bootstrap skipped: {}",
                    e
                );
            }
        }
        crate::discovery_pipeline::spawn_periodic_rediscovery(cfg.capability_rediscovery_secs());

        // Spawn the cells already present once; the watcher below spawns
        // only cells added afterwards.
        crate::auto_discovery::spawn_all_cells(&workspace);

        // Hot-pluggable cell watcher (Swarm OS Bullet 6): monitors ~/.susi/cells/
        // for new/removed files and triggers auto-discovery without daemon restart.
        let _cell_watcher = crate::cell_watcher::start_cell_watcher(
            crate::cell_watcher::CellWatcherConfig::for_substrate(&workspace),
        );

        // Durable workflow engine (Swarm OS Bullet 8): rehydrates pending tasks
        // from the JSONL append-only journal to survive daemon restarts.
        let workflow_engine =
            crate::workflows::WorkflowEngine::new(&workspace).unwrap_or_else(|e| {
                eprintln!(
                    "[SusiDaemon] Failed to initialize durable workflow engine: {}",
                    e
                );
                std::process::exit(1);
            });
        let pending_tasks = workflow_engine.get_pending_tasks();
        if !pending_tasks.is_empty() {
            println!(
                "[SusiDaemon] Rehydrated {} pending workflow tasks",
                pending_tasks.len()
            );
        }

        // Time-Travel Debugger (Swarm OS Bullet 7): immutable HMAC-signed event log
        let time_travel_logger = crate::event_log::TimeTravelDebugger::new(&workspace);
        time_travel_logger.log_state_change("DAEMON_BOOT", "Swarm OS kernel booting");

        // Autonomous Topology Manager (Swarm OS Bullet 24)
        let _topology_manager = crate::topology::TopologyManager::new();
        time_travel_logger
            .log_state_change("TOPOLOGY_ENGINE", "Autonomous P2P topology manager online");

        // Spawn Autonomous Background Model Provisioner & Resumable Downloader
        susi_gemi::models::ModelManager::spawn_background_hardware_model_provisioner(&workspace);

        // Canonical public ports + the uniform port_offset — never fall back
        // to ephemeral ports.
        let (gemi_port, gmcp_port, gmcp_http_port, a2a_port, udp_port) = (
            cfg.gemi_port(),
            cfg.gmcp_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port(),
            cfg.udp_discovery_port(),
        );
        let gemi_server =
            Self::bind_tcp_canonical(gemi_port, "GEMI HTTP", &global_dir, &bind_address);
        let gmcp_primary =
            Self::bind_tcp_canonical(gmcp_port, "GMCP/MCP HTTP", &global_dir, &bind_address);
        let gmcp_alias = Self::bind_tcp_canonical(
            gmcp_http_port,
            "GMCP HTTP alias",
            &global_dir,
            &bind_address,
        );
        let a2a_http = Self::bind_tcp_canonical(a2a_port, "A2A HTTP", &global_dir, &bind_address);
        let udp_socket =
            Self::bind_udp_canonical(udp_port, "A2A UDP discovery", &global_dir, &bind_address);

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

        let tls_gemi = tls_acceptor.clone();
        let workspace_gemi = workspace.clone();
        let require_tls_gemi = require_tls_remote;
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GemiServer::start_http_server(
                    workspace_gemi,
                    gemi_server,
                    tls_gemi,
                    require_tls_gemi,
                );
            })) {
                eprintln!("[GEMI] Thread panicked: {:?}", e);
            }
        });

        let tls_gmcp = tls_acceptor.clone();
        let workspace_gmcp = workspace.clone();
        let require_tls_gmcp = require_tls_remote;
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GmcpServer::start_http_server(
                    workspace_gmcp,
                    gmcp_primary,
                    tls_gmcp,
                    require_tls_gmcp,
                );
            })) {
                eprintln!("[GMCP] Thread panicked: {:?}", e);
            }
        });

        let tls_gmcp_alias = tls_acceptor.clone();
        let workspace_gmcp_alias = workspace.clone();
        let require_tls_gmcp_alias = require_tls_remote;
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GmcpServer::start_http_server(
                    workspace_gmcp_alias,
                    gmcp_alias,
                    tls_gmcp_alias,
                    require_tls_gmcp_alias,
                );
            })) {
                eprintln!("[GMCP alias] Thread panicked: {:?}", e);
            }
        });

        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                // The pong announces the effective GMCP port — a non-canonical
                // instance (port_offset) still advertises the right port.
                Self::start_udp_discovery_server(udp_socket, gmcp_port);
            })) {
                eprintln!("[UDP] Thread panicked: {:?}", e);
            }
        });

        // Same zero-trust policy as GMCP/GEMI: a bound member's Ed25519
        // signature over the exact body authorizes on its own, the bearer
        // covers unbound/standalone callers, and everything else fails
        // closed. The verifier closure receives the buffered body so v2
        // (body-bound) signatures verify, not just v1.
        let a2a_verifier: susi_gawd::a2a::server::Verifier =
            std::sync::Arc::new(|ctx: &susi_gawd::a2a::server::VerifierContext| {
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
        let tls_a2a = tls_acceptor;
        let require_tls_a2a = require_tls_remote;
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Err(e) =
                    susi_gawd::a2a::server::serve(a2a_http, a2a_verifier, tls_a2a, require_tls_a2a)
                {
                    eprintln!("[A2A] Server exited: {e}");
                }
            })) {
                eprintln!("[A2A] Thread panicked: {:?}", e);
            }
        });

        // Cluster scout: discovery broadcasts, liveness decay, roster
        // rehydration, and commit-ledger anti-entropy all live in the
        // thread `list_cluster_nodes` lazily spawns. Without this kick it
        // only ever starts when a mission happens to touch the roster —
        // the daemon would answer inbound pings but never scout outbound,
        // and ledger anti-entropy would stay dormant between missions.
        thread::spawn(|| {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = susi_gawd::amas::SusiSupervisor::list_cluster_nodes();
            })) {
                eprintln!("[SWARM] Scout thread failed to start: {:?}", e);
            }
        });

        // Continuous Interaction Substrate Worker
        // Each pulse carries the ingesting caller's cwd (`pulse.workspace`).
        // Never substitute the daemon's boot workspace — that made "susi" in
        // folder B silently operate on folder A whenever the daemon had been
        // started from A (same bug class as the cross-workspace binary restart).
        thread::spawn(move || {
            let queue = susi_gawd::queue::SubstratePulseQueue::global();
            let ama = susi_gawd::ama::SusiMasterAgent::new();

            loop {
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
        });

        // Process layer: bring the leaf services up and keep them up for
        // the life of the daemon. The supervisor terminates only pids it
        // spawned itself, recorded in the shared process table.
        let supervisor = crate::supervisor::start(Arc::clone(&ctx.shutdown_signal));

        while !ctx.is_shutdown_requested() {
            thread::sleep(Duration::from_secs(5));
        }

        eprintln!("[SusiDaemon] Graceful shutdown initiated");
        // Swarm Cells are the daemon's children; never orphan them.
        let stopped = crate::auto_discovery::stop_all_cells();
        if stopped > 0 {
            eprintln!("[SusiDaemon] Stopped {stopped} Swarm Cell(s)");
        }
        crate::supervisor::shutdown_all();
        // The monitor is shutdown-aware (aborts mid-pass, never spawns
        // or saves once the flag lands) — join it so no stale write or
        // in-flight spawn outlives the teardown.
        let _ = supervisor.join();
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

    /// Bind a TCP port that external clients hard-code. Reclaims stale susi
    /// holders; never randomizes — exit if a foreign process owns the port.
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
        let wildcard = matches!(bind_address, "0.0.0.0" | "::");
        let mut addrs = Vec::new();
        if !wildcard && !crate::tls::is_loopback(bind_address) {
            addrs.push(format!("127.0.0.1:{port}"));
        }
        addrs.push(format!("{bind_address}:{port}"));
        addrs
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
             External clients trust {} — free the port (or stop the foreign process) and restart susi.\n\
             Port randomization is disabled by contract.",
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
             External clients trust {}:{} — free the port and restart susi.",
            port, name, bind_address, port
        );
        std::process::exit(1);
    }

    /// Aggressive Port Reclaim: Interrogates the process holding a port and evicts it if it's a susi instance.
    #[allow(unsafe_code)]
    fn attempt_port_reclaim(port: u16, global_dir: &Path) -> bool {
        #[cfg(unix)]
        {
            let mut reclaimed = false;
            let scripts = [
                format!("fuser {}/tcp 2>/dev/null", port),
                format!("fuser {}/udp 2>/dev/null", port),
                format!(
                    "ss -lptn 'sport = :{}' 2>/dev/null | sed -n 's/.*pid=\\([0-9]\\+\\).*/\\1/p'",
                    port
                ),
            ];
            for script in scripts {
                let output = Command::new("sh").arg("-c").arg(&script).output();
                let Ok(out) = output else {
                    continue;
                };
                let pid_str = String::from_utf8_lossy(&out.stdout);
                for token in pid_str.split(|c: char| !c.is_ascii_digit()) {
                    if token.is_empty() {
                        continue;
                    }
                    let Ok(pid) = token.parse::<i32>() else {
                        continue;
                    };
                    if pid <= 1 {
                        continue;
                    }
                    if Self::is_trusted_susi_process(pid, global_dir) {
                        eprintln!(
                            "[Self-Healing] Evicting stale susi process (PID: {}) holding port {}...",
                            pid, port
                        );
                        // SAFETY: kill on a pid verified above to be a stale susi process holding our port; no memory is shared.
                        unsafe {
                            libc::kill(pid, libc::SIGKILL);
                        }
                        reclaimed = true;
                    } else {
                        eprintln!(
                            "[SusiDaemon] Port {} held by non-susi PID {} — will not evict",
                            port, pid
                        );
                    }
                }
            }
            if reclaimed {
                thread::sleep(Duration::from_millis(250));
            }
            reclaimed
        }
        #[cfg(not(unix))]
        {
            let _ = (port, global_dir);
            false
        }
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
    #[cfg(unix)]
    fn is_trusted_susi_process(pid: i32, global_dir: &Path) -> bool {
        let exe_path = match fs::read_link(format!("/proc/{}/exe", pid)) {
            Ok(p) => p,
            Err(_) => return false,
        };

        let name_ok = exe_path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "susi")
            .unwrap_or(false);
        if !name_ok {
            return false;
        }

        // Trust is per-instance: a sibling SUSI_HOME daemon runs the same
        // trusted binary and would pass the hash check below, but evicting
        // it to claim its port would kill a live independent node. Only
        // processes whose environ places them in THIS instance are ours to
        // reclaim.
        #[cfg(target_os = "linux")]
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

    fn start_udp_discovery_server(socket: std::net::UdpSocket, gmcp_port: u16) {
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
            let msg = String::from_utf8_lossy(&buf[..amt]);
            // Cluster-key handshake (VC-200-001): answer a signed ping with a
            // signed pong echoing the requester's nonce. Only peers that hold
            // ~/.susi/cluster.key can complete this — an unauthenticated LAN
            // host still gets legacy discovery but never roster admission.
            if let Some((pinger_id, caps_csv, checksum, bloom_hex, nonce, wants_v2)) =
                crate::susi_config::cluster_key::verify_signed_ping(&msg)
            {
                // Mutual admission: a correctly signed ping proves the
                // sender holds cluster.key — the same proof a signed pong
                // gives the pinger about us. Admitting the sender here
                // makes membership symmetric in one exchange: the pinger
                // admits us from our pong, we admit them from their ping.
                // Self-edge guard — persisting ourselves as Explicit lets
                // mission dispatch recurse into our own endpoint. Loopback
                // is never a peer (one daemon binds the discovery port per
                // host); our own scout's broadcast also reaches this
                // socket via the LAN interface, so refuse any local
                // interface address (the bind probe can only succeed
                // locally) and any ping attesting OUR node_id. The signed
                // pong below still goes out — a CLI probing its own
                // address needs the refusal signal, not silence.
                let is_self = src.ip().is_loopback()
                    || std::net::TcpListener::bind((src.ip(), 0)).is_ok()
                    || (!pinger_id.is_empty()
                        && pinger_id == crate::susi_config::cluster_key::wire_node_id());
                if !is_self {
                    // Peers advertise their effective HTTP alias port in
                    // `caps_csv` (`gmcp_http=`); offset-shifted nodes are
                    // reachable on their real port, canonical otherwise.
                    let peer_http = caps_csv
                        .split(',')
                        .find_map(|c| c.strip_prefix("gmcp_http="))
                        .and_then(|p| p.parse::<u16>().ok())
                        .unwrap_or(crate::susi_paths::ports::GMCP_HTTP);
                    let pinger_addr = format!("{}:{}", src.ip(), peer_http);
                    // The verified signature proves the daemon at src holds
                    // cluster.key — Explicit standing for the ADDRESS is
                    // earned. The advertised node_id is self-asserted,
                    // though: persisting it would let any member re-home
                    // another member's identity to its own address with a
                    // single forged ping. We record a synthetic id; the
                    // scout's reverse handshake upgrades to the attested
                    // id once this address's own responder confirms it,
                    // and committed member_add records bind real ids
                    // through the ledger path.
                    // A banned member gets no handshake at all: no
                    // admission on our side, and no signed pong for them
                    // to verify us with. Without this the ban only stops
                    // our roster — the banned node could still treat us
                    // as a verified peer and harvest our roster gossip.
                    if susi_gawd::swarm::peer_registry::is_banned(&pinger_id, &pinger_addr) {
                        continue;
                    }
                    let node = susi_gawd::swarm::amas::ClusterPeerNode {
                        node_id: format!("susi-peer-{}", src.ip()),
                        address: pinger_addr,
                        node_type: "PEER".into(),
                        is_active: true,
                        capabilities: caps_csv
                            .split(',')
                            .filter(|c| !c.is_empty())
                            .map(str::to_string)
                            .collect(),
                        registry_checksum: checksum,
                        latency_ms: 0,
                        uptime_secs: 0,
                        trust_score: 0.8,
                        capability_bloom: susi_gawd::swarm::amas::CapabilityBloom::from_hex(
                            &bloom_hex,
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
                // Identity-era pings get attested pongs — v3 (subject-
                // signed binding) plus v2+v1 fallbacks so every requester
                // generation finds a format it can verify.
                let pongs: Vec<Option<String>> = if wants_v2 {
                    vec![
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
                    ]
                } else {
                    vec![crate::susi_config::cluster_key::signed_pong(
                        &crate::susi_config::cluster_key::wire_node_id(),
                        0,
                        &bloom,
                        &nonce,
                        &roster,
                    )]
                };
                for pong in pongs.into_iter().flatten() {
                    let _ = socket.send_to(pong.as_bytes(), src);
                }
                continue;
            }
            // Own the host-contract UDP surface: answer both LAN and peer dialects.
            if msg.contains("SUSI_LAN_PING") || msg.starts_with("SUSI_PING") {
                let pong = if msg.contains("SUSI_LAN_PING") {
                    format!(
                        "SUSI_LAN_PONG:{}:{}",
                        crate::susi_config::cluster_key::wire_node_id(),
                        gmcp_port
                    )
                } else {
                    "SUSI_PONG:daemon:0:".to_string()
                };
                let _ = socket.send_to(pong.as_bytes(), src);
            }
        }
    }

    /// systemd unit name for this instance — `susi-daemon` for the primary,
    /// suffixed with a stable hash of `SUSI_HOME` for an isolated sibling so
    /// two instances never share a unit (and stop only kills the right one).
    /// The same absolute-path rule as `SusiDirs::instance_root` applies: a
    /// relative `SUSI_HOME` selects no instance root anywhere, so it must not
    /// mint a distinct unit either.
    fn daemon_unit_name() -> String {
        Self::unit_name_for(Self::instance_home().as_deref())
    }

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

    #[allow(unsafe_code)]
    pub fn stop_daemon(_workspace: &Path, global_dir: &Path) -> bool {
        let global_lock = Self::get_lock_file(global_dir);
        let target_lock = &global_lock;

        if target_lock.exists() {
            if let Ok(content) = fs::read_to_string(target_lock)
                && let Ok(pid) = content.lines().next().unwrap_or("").trim().parse::<i32>()
            {
                #[cfg(unix)]
                // SAFETY: kill with SIGTERM on the pid recorded in our own daemon lock file; no memory is shared.
                unsafe {
                    libc::kill(pid, libc::SIGTERM);
                }
            }
            let _ = fs::remove_file(&global_lock);
            true
        } else {
            false
        }
    }

    /// `susi stop` — kills only THIS instance's daemon: its systemd unit
    /// (named after `SUSI_HOME`), its lock-file pid, and orphan
    /// `daemon-start` processes whose environment matches this instance.
    /// A sibling `SUSI_HOME` instance keeps running.
    #[allow(unsafe_code)]
    pub fn stop_instance_daemons(global_dir: &Path) -> usize {
        Self::stop_daemons(global_dir, true)
    }

    /// Stops every susi daemon for this user — every `susi-daemon*` unit and
    /// every orphan `daemon-start` process, including instances rooted at
    /// other `SUSI_HOME`s. Used by `susi uninstall`, which must leave zero
    /// running daemons behind so a later reinstall starts clean.
    /// Returns how many daemon processes were signalled.
    #[allow(unsafe_code)]
    pub fn stop_all_daemons(global_dir: &Path) -> usize {
        Self::stop_daemons(global_dir, false)
    }

    #[allow(unsafe_code)]
    fn stop_daemons(global_dir: &Path, scoped_to_instance: bool) -> usize {
        let mut killed = 0usize;

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

        // 2. Lock-file path — the common case.
        if Self::stop_daemon(global_dir, global_dir) {
            killed += 1;
        }

        // 3. Orphan sweep: surviving `daemon-start` processes for this user,
        // lock file or not. Scoped stop keeps only the candidates whose
        // environment places them in this instance — a sibling's daemon has
        // a different SUSI_HOME, the primary's has none.
        #[cfg(target_os = "linux")]
        {
            if let Ok(entries) = fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let Some(name) = name.to_str() else { continue };
                    let Ok(pid) = name.parse::<i32>() else {
                        continue;
                    };
                    if pid == std::process::id() as i32 {
                        continue;
                    }
                    let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
                        continue;
                    };
                    // cmdline is NUL-separated argv.
                    let is_daemon = cmdline.split(|b| *b == 0).any(|arg| arg == b"daemon-start");
                    if is_daemon && (!scoped_to_instance || Self::daemon_in_this_instance(pid)) {
                        // SAFETY: kill with SIGTERM on a pid whose cmdline was just verified to be a susi daemon; no memory is shared.
                        unsafe {
                            libc::kill(pid, libc::SIGTERM);
                        }
                        killed += 1;
                    }
                }
            }
        }

        killed
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
    /// the `SUSI_HOME` recorded in its `/proc/<pid>/environ` against ours.
    /// A daemon with no `SUSI_HOME` is the primary instance; one with a
    /// different root is a sibling and must survive `susi stop`.
    /// Unreadable environ (permission, pid reused) answers false — the
    /// scoped sweep errs toward leaving a foreign process alive.
    #[cfg(target_os = "linux")]
    fn daemon_in_this_instance(pid: i32) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let Ok(environ) = fs::read(format!("/proc/{pid}/environ")) else {
            return false;
        };
        let theirs = environ.split(|b| *b == 0).find_map(|kv| {
            kv.strip_prefix(b"SUSI_HOME=")
                .map(std::ffi::OsStr::from_bytes)
                .map(PathBuf::from)
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_paths::ports;

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
        let contents = std::fs::read_to_string(&global_lock_path).unwrap_or_default();
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

    #[cfg(unix)]
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

    #[cfg(unix)]
    #[test]
    fn test_is_trusted_susi_process_matches_only_the_trusted_hash() {
        let base = std::env::temp_dir().join(format!("susi_trust_test_{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let global_dir = base.join("global");
        std::fs::create_dir_all(&global_dir).unwrap();

        // A real file literally named "susi" standing in for the trusted
        // binary, so /proc/{pid}/exe's basename check has something to match.
        let fake_bin = base.join("susi");
        std::fs::copy("/bin/sleep", &fake_bin).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_bin, perms).unwrap();
        }

        // ETXTBSY is transient: a parallel thread's fork can hold the
        // destination fd open-for-write between fork and exec while the
        // copy above is finishing. Retry past the window.
        let mut child = {
            let mut attempt = 0;
            loop {
                match Command::new(&fake_bin).arg("5").spawn() {
                    Ok(c) => break c,
                    Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && attempt < 20 => {
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
        assert!(SusiDaemon::is_trusted_susi_process(pid, &global_dir));

        // Trust anchor stale/mismatched: no longer trusted.
        std::fs::write(SusiDaemon::get_hash_file(&global_dir), "not-the-real-hash").unwrap();
        assert!(!SusiDaemon::is_trusted_susi_process(pid, &global_dir));

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&base);
    }
}
