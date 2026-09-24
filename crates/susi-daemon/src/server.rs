//! Always-On SUSI Substrate Daemon Process Manager
//!
//! External clients may hard-code these ports — the daemon must never drift them:
//! - **9090** GMCP / MCP HTTP
//! - **9091** GEMI HTTP
//! - **9092** A2A UDP discovery
//! - **9093** GMCP HTTP (streamable / SSE alias)

use crate::susi_paths::ports;

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
use susi_gawd::ama::SusiMasterAgent;
use susi_gawd::queue::SubstratePulseQueue;
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
            let mut overlapped = unsafe { std::mem::zeroed() };
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

    /// True when the public host-contract TCP surfaces (9090/9091/9093) accept connections.
    pub fn host_contract_tcp_ready() -> bool {
        [ports::GMCP, ports::GEMI, ports::GMCP_HTTP]
            .iter()
            .all(|port| {
                TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], *port)),
                    Duration::from_millis(200),
                )
                .is_ok()
            })
    }

    /// True when UDP discovery on 9092 answers a LAN ping (daemon owns the port).
    pub fn host_contract_udp_ready() -> bool {
        let Ok(sock) = UdpSocket::bind("127.0.0.1:0") else {
            return false;
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(300)));
        let target = std::net::SocketAddr::from(([127, 0, 0, 1], ports::UDP_DISCOVERY));
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

    /// Full host contract: TCP 9090/9091/9093 + UDP 9092 discovery.
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
        format!(
            "Host contract endpoints:\n\
             - GMCP/MCP  http://127.0.0.1:{}/mcp\n\
             - GEMI      http://127.0.0.1:{}/\n\
             - UDP disco 127.0.0.1:{}\n\
             - GMCP alias http://127.0.0.1:{}/mcp",
            ports::GMCP,
            ports::GEMI,
            ports::UDP_DISCOVERY,
            ports::GMCP_HTTP
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
        // Host contract: prefer the canonical ~/.susi/bin/susi.
        let bin_to_run = if global_bin.exists() {
            global_bin.clone()
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
            let spawned_via_systemd = Command::new("systemd-run")
                .args([
                    "--user",
                    "--collect",
                    "--quiet",
                    "--unit=susi-daemon",
                    "--property=MemoryMax=2G",
                    "--property=CPUQuota=50%",
                    "--",
                ])
                .arg(&bin_to_run)
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
                let _ = Self::wait_for_host_contract(Duration::from_secs(10));
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
            let _ = Self::wait_for_host_contract(Duration::from_secs(10));
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
            let _ = Self::wait_for_host_contract(Duration::from_secs(10));
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

        // Substrate Administration & Hardware Optimization (Pillar 1)
        crate::runtime_admin::SusiRuntimeAdmin::start_administration_cycle(&workspace);

        // Pillar 8: Zero-config discovery of local inference engines + MCP tools
        // run_daemon_loop is sync (invoked from CLI `daemon-start`); spin up a
        // short-lived runtime for the async probes, matching GMCP/GEMI bind paths.
        match tokio::runtime::Runtime::new() {
            Ok(runtime) => {
                runtime.block_on(crate::auto_discovery::bootstrap_zero_config_substrate());
            }
            Err(e) => {
                eprintln!(
                    "[SusiDaemon] Zero-config substrate bootstrap skipped: {}",
                    e
                );
            }
        }
        crate::auto_discovery::spawn_periodic_rediscovery(cfg.capability_rediscovery_secs());

        // Spawn Autonomous Background Model Provisioner & Resumable Downloader
        susi_gemi::models::ModelManager::spawn_background_hardware_model_provisioner(&workspace);

        // Canonical public ports — never fall back to ephemeral ports.
        let gemi_server =
            Self::bind_tcp_canonical(ports::GEMI, "GEMI HTTP", &global_dir, &bind_address);
        let gmcp_primary =
            Self::bind_tcp_canonical(ports::GMCP, "GMCP/MCP HTTP", &global_dir, &bind_address);
        let gmcp_alias = Self::bind_tcp_canonical(
            ports::GMCP_HTTP,
            "GMCP HTTP alias",
            &global_dir,
            &bind_address,
        );
        let udp_socket = Self::bind_udp_canonical(
            ports::UDP_DISCOVERY,
            "A2A UDP discovery",
            &global_dir,
            &bind_address,
        );

        eprintln!(
            "[SusiDaemon] Public endpoints ready:\n\
             - GMCP/MCP  http://{}:{}/mcp\n\
             - GEMI      http://{}:{}/\n\
             - UDP disco {}:{}\n\
             - GMCP alias http://{}:{}/mcp",
            bind_address,
            ports::GMCP,
            bind_address,
            ports::GEMI,
            bind_address,
            ports::UDP_DISCOVERY,
            bind_address,
            ports::GMCP_HTTP
        );

        let workspace_gemi = workspace.clone();
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GemiServer::start_http_server(workspace_gemi, gemi_server);
            })) {
                eprintln!("[GEMI] Thread panicked: {:?}", e);
            }
        });

        let workspace_gmcp = workspace.clone();
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GmcpServer::start_http_server(workspace_gmcp, gmcp_primary);
            })) {
                eprintln!("[GMCP] Thread panicked: {:?}", e);
            }
        });

        let workspace_gmcp_alias = workspace.clone();
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GmcpServer::start_http_server(workspace_gmcp_alias, gmcp_alias);
            })) {
                eprintln!("[GMCP alias] Thread panicked: {:?}", e);
            }
        });

        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Self::start_udp_discovery_server(udp_socket, ports::GMCP);
            })) {
                eprintln!("[UDP] Thread panicked: {:?}", e);
            }
        });

        // Continuous Interaction Substrate Worker
        // Each pulse carries the ingesting caller's cwd (`pulse.workspace`).
        // Never substitute the daemon's boot workspace — that made "susi" in
        // folder B silently operate on folder A whenever the daemon had been
        // started from A (same bug class as the cross-workspace binary restart).
        thread::spawn(move || {
            let queue = SubstratePulseQueue::global();
            let ama = SusiMasterAgent::new();

            loop {
                if let Some(pulse) = queue.pop() {
                    info!(
                        "[SubstratePulseQueue] Processing Pulse: {} (workspace: {})",
                        pulse.intent,
                        pulse.workspace.display()
                    );
                    let _ = ama.solve_stream(
                        &pulse.intent,
                        SubstratePulseQueue::execution_workspace(&pulse),
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
        crate::supervisor::shutdown_all();
        // The monitor is shutdown-aware (aborts mid-pass, never spawns
        // or saves once the flag lands) — join it so no stale write or
        // in-flight spawn outlives the teardown.
        let _ = supervisor.join();
    }

    fn force_canonical_ports(cfg: &mut SusiConfig) {
        cfg.settings
            .insert("gmcp_port".to_string(), serde_json::json!(ports::GMCP));
        cfg.settings
            .insert("gemi_port".to_string(), serde_json::json!(ports::GEMI));
        cfg.settings.insert(
            "udp_discovery_port".to_string(),
            serde_json::json!(ports::UDP_DISCOVERY),
        );
        cfg.settings.insert(
            "gmcp_http_port".to_string(),
            serde_json::json!(ports::GMCP_HTTP),
        );
    }

    /// Bind a TCP port that external clients hard-code. Reclaims stale susi
    /// holders; never randomizes — exit if a foreign process owns the port.
    fn bind_tcp_canonical(
        port: u16,
        name: &str,
        global_dir: &Path,
        bind_address: &str,
    ) -> std::net::TcpListener {
        let addr = format!("{}:{}", bind_address, port);
        for attempt in 1..=5 {
            match std::net::TcpListener::bind(&addr) {
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
             External clients trust {}:{} — free the port (or stop the foreign process) and restart susi.\n\
             Port randomization is disabled by contract.",
            port, name, bind_address, port
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
        let mut buf = [0u8; 1024];
        while let Ok((amt, src)) = socket.recv_from(&mut buf) {
            let msg = String::from_utf8_lossy(&buf[..amt]);
            // Cluster-key handshake (VC-200-001): answer a signed ping with a
            // signed pong echoing the requester's nonce. Only peers that hold
            // ~/.susi/cluster.key can complete this — an unauthenticated LAN
            // host still gets legacy discovery but never roster admission.
            if let Some((pinger_id, caps_csv, checksum, bloom_hex, nonce)) =
                crate::susi_config::cluster_key::verify_signed_ping(&msg)
            {
                // Mutual admission: a correctly signed ping proves the
                // sender holds cluster.key — the same proof a signed pong
                // gives the pinger about us. Admitting the sender here
                // makes membership symmetric in one exchange: the pinger
                // admits us from our pong, we admit them from their ping.
                // Loopback is ourselves — never a peer (one daemon binds
                // the discovery port per host). The advertised node_id is
                // signed, so it is trusted as-is; a legacy ping without
                // one gets a synthesized per-address id.
                if !src.ip().is_loopback() {
                    let pinger_addr =
                        format!("{}:{}", src.ip(), crate::susi_paths::ports::GMCP_HTTP);
                    let pinger_id = if pinger_id.is_empty() {
                        format!("susi-peer-{}", src.ip())
                    } else {
                        pinger_id
                    };
                    if !susi_gawd::swarm::peer_registry::is_banned(&pinger_id, &pinger_addr) {
                        let node = susi_gawd::swarm::amas::ClusterPeerNode {
                            node_id: pinger_id,
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
                        };
                        susi_gawd::swarm::peer_registry::persist_verified_peer(&node);
                    }
                }
                let bloom = susi_gawd::swarm::amas::CapabilityBloom::local_snapshot().to_hex();
                // Gossip our verified roster so one handshake teaches the
                // joiner the whole cluster — transitive membership.
                let roster: Vec<(String, String)> =
                    susi_gawd::swarm::peer_registry::load_persisted_peers()
                        .into_iter()
                        .map(|p| (p.node_id, p.address))
                        .collect();
                if let Some(pong) = crate::susi_config::cluster_key::signed_pong(
                    &crate::susi_config::cluster_key::wire_node_id(),
                    0,
                    &bloom,
                    &nonce,
                    &roster,
                ) {
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

    #[allow(dead_code)]
    #[allow(unsafe_code)]
    pub fn stop_daemon(_workspace: &Path, global_dir: &Path) -> bool {
        let global_lock = Self::get_lock_file(global_dir);
        let target_lock = &global_lock;

        if target_lock.exists() {
            if let Ok(content) = fs::read_to_string(target_lock)
                && let Ok(pid) = content.lines().next().unwrap_or("").trim().parse::<i32>()
            {
                #[cfg(unix)]
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

    /// Stops every susi daemon for this user, including orphans whose lock
    /// file is already gone (e.g. a prior uninstall removed substrate.lock
    /// but left the daemon running). Used by `susi uninstall`, which must
    /// leave zero running daemons behind so a later reinstall starts clean.
    /// Returns how many daemon processes were signalled.
    #[allow(unsafe_code)]
    pub fn stop_all_daemons(global_dir: &Path) -> usize {
        let mut killed = 0usize;

        // 1. The systemd transient unit spawned by ensure_daemon_running's
        // systemd-run path (Linux only; no-op everywhere else).
        #[cfg(target_os = "linux")]
        {
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "susi-daemon.service"])
                .status();
        }

        // 2. Lock-file path — the common case.
        if Self::stop_daemon(global_dir, global_dir) {
            killed += 1;
        }

        // 3. Orphan sweep: any surviving `daemon-start` process for this
        // user, lock file or not. Scan /proc on Linux; on other platforms
        // the lock file is the only handle we have.
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
                    if is_daemon {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_contract_endpoint_report_lists_canonical_ports() {
        let report = SusiDaemon::host_contract_endpoints_report();
        assert!(report.contains(":9090/mcp"));
        assert!(report.contains(":9091/"));
        assert!(report.contains(":9092"));
        assert!(report.contains(":9093/mcp"));
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
            "wait_for_host_contract must return false when nothing owns 9090–9093"
        );
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
        let cfg = SusiConfig::default();
        assert_eq!(cfg.gmcp_port(), ports::GMCP);
        assert_eq!(cfg.gemi_port(), ports::GEMI);
        assert_eq!(cfg.udp_discovery_port(), ports::UDP_DISCOVERY);
        assert_eq!(cfg.gmcp_http_port(), ports::GMCP_HTTP);
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
