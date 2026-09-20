// Always-On SUSI Substrate Daemon Process Manager
// 100% Rust implementation managing GMCP (Port 9090), GEMI (Port 9091) & A2A Cluster UDP (Port 9092)

use std::fs;
use std::io::{Seek, SeekFrom, Write};

use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

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

use susi_error::EaiError;
use susi_gawd::ama::SusiMasterAgent;
use susi_gawd::queue::SubstratePulseQueue;
use susi_gmcp::server::GmcpServer;
use susi_sandbox::manager::SusiConfig;
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
                    let msgs = susi_sandbox::manager::SusiMessages::load_global();
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

/// A daemon process found running, plus the workspace it was actually
/// started for and whether that was discovered via this caller's own
/// workspace-scoped lock (`same_workspace: true`) or only via the global
/// cross-workspace fallback lock (`false`). See `find_running_daemon`.
pub struct RunningDaemon {
    pub pid: u32,
    pub workspace: PathBuf,
    pub same_workspace: bool,
}

pub struct DaemonLock {
    file: fs::File,
}

impl DaemonLock {
    fn acquire(path: &Path) -> Result<Self, String> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|e| e.to_string())?;

        #[cfg(unix)]
        {
            // SAFETY: We just opened the file, so fd is valid.
            // flock doesn't access memory unsafely or take ownership.
            let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                return Err("Locked by another process".into());
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

    /// Writes `pid\nworkspace`. The second line lets a *different*
    /// invocation that later finds this daemon only via the global lock's
    /// cross-workspace fallback (see `find_running_daemon`) recover which
    /// workspace it was actually started for, instead of assuming its own —
    /// assuming wrong previously caused a binary-change-triggered restart
    /// to tear down a daemon for workspace A and respawn it scoped to
    /// unrelated workspace B, just because a CLI invocation from B happened
    /// to be the one that noticed the binary had changed.
    fn write_pid_for_workspace(&mut self, workspace: &Path) -> Result<(), String> {
        self.file.set_len(0).map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        write!(self.file, "{}\n{}", std::process::id(), workspace.display())
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

    /// Like `check_status`, but also reports which workspace the running
    /// daemon was actually started for and whether it was found via this
    /// caller's own workspace-scoped lock or only via the global
    /// cross-workspace fallback — the information `ensure_daemon_running`
    /// needs to never again restart the wrong workspace's daemon just
    /// because a different workspace's CLI invocation was the one that
    /// noticed a binary change.
    pub fn find_running_daemon(workspace: &Path, global_dir: &Path) -> Option<RunningDaemon> {
        let global_lock = Self::get_lock_file(global_dir);
        Self::check_status_path(&global_lock).map(|pid| {
            let running_workspace = Self::read_recorded_workspace(&global_lock)
                .unwrap_or_else(|| workspace.to_path_buf());
            let same_workspace = Self::paths_identify_same_workspace(&running_workspace, workspace);

            RunningDaemon {
                pid,
                workspace: running_workspace,
                same_workspace,
            }
        })
    }

    fn paths_identify_same_workspace(left: &Path, right: &Path) -> bool {
        let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
        let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
        left == right
    }

    fn read_recorded_workspace(lock_file_path: &Path) -> Option<PathBuf> {
        let content = fs::read_to_string(lock_file_path).ok()?;
        let mut lines = content.lines();
        lines.next()?; // PID line
        lines.next().map(PathBuf::from)
    }

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
    pub fn ensure_daemon_running(workspace: &Path, global_dir: &Path) {
        let current_exe = std::env::current_exe().ok();
        let msgs = susi_sandbox::manager::SusiMessages::load_global();
        if let Some(running) = Self::find_running_daemon(workspace, global_dir) {
            if let Some(ref exe) = current_exe {
                if let Ok(false) =
                    susi_sandbox::daemon_state::SusiDaemonState::verify_binary_integrity(
                        exe, global_dir,
                    )
                {
                    if !running.same_workspace {
                        // A daemon IS running (globally, for some other
                        // workspace) with a stale binary - but restarting
                        // it from here would use *our* workspace, tearing
                        // down and replacing a daemon we don't own with one
                        // scoped to somewhere unrelated. Measured live: this
                        // exact mistake killed a real daemon and replaced it
                        // with one for whatever directory a stray CLI
                        // invocation happened to run from. Leave it running
                        // stale rather than risk that; it self-heals the
                        // next time a CLI invocation from ITS OWN workspace
                        // notices the same mismatch.
                        return;
                    }
                    let def_recompiled =
                        "[SusiDaemon] Binary recompiled. Restarting daemon PID {}...".to_string();
                    let msg = msgs
                        .get("daemon", "binary_recompiled")
                        .unwrap_or(&def_recompiled);
                    info!("{}", msg.replace("{}", &running.pid.to_string()));
                    Self::stop_daemon(workspace, global_dir);
                } else {
                    return;
                }
            } else {
                return;
            }
        }
        let bin_name = if cfg!(target_os = "windows") {
            "bin/susi.exe"
        } else {
            "bin/susi"
        };
        let global_bin = global_dir.join(bin_name);

        let bin_to_run = if let Some(ref exe) = current_exe {
            exe.clone()
        } else if global_bin.exists() {
            global_bin
        } else {
            PathBuf::from(if cfg!(target_os = "windows") {
                "susi.exe"
            } else {
                "susi"
            })
        };

        // Binary Integrity Check
        match susi_sandbox::daemon_state::SusiDaemonState::verify_binary_integrity(
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
                let def_tampered = "[SusiDaemon] Binary integrity check FAILED.".to_string();
                warn!(
                    "{}",
                    msgs.get("daemon", "binary_tampered")
                        .unwrap_or(&def_tampered)
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
                .arg(workspace.to_str().unwrap_or("."))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if spawned_via_systemd {
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
                .arg(workspace.to_str().unwrap_or("."))
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
        }

        #[cfg(not(unix))]
        {
            use std::process::Stdio;
            let mut cmd = Command::new(&bin_to_run);
            cmd.arg("daemon-start")
                .arg("--workspace")
                .arg(workspace.to_str().unwrap_or("."))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let _ = cmd.spawn();
        }
    }

    pub fn run_daemon_loop(workspace: PathBuf, global_dir: PathBuf) {
        if cfg!(test) {
            return;
        }
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

        if let Err(e) = lock.write_pid_for_workspace(&workspace) {
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
        let bind_address = susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .get("bind_address")
            .unwrap_or_else(|| "0.0.0.0".to_string());
        let mut config_changed = false;

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

        // 1. Bind GEMI HTTP Server (Port 9091 / Dynamic)
        let (gemi_server, gemi_port) = Self::bind_http_with_fallback(
            cfg.gemi_port(),
            "GEMI",
            &workspace,
            &global_dir,
            &bind_address,
        );
        if gemi_port != cfg.gemi_port() {
            cfg.settings
                .insert("gemi_port".to_string(), serde_json::json!(gemi_port));
            config_changed = true;
        }

        // 2. Bind GMCP HTTP/SSE Server (Port 9093 / Dynamic)
        let (gmcp_http_server, gmcp_http_port) = Self::bind_http_with_fallback(
            cfg.gmcp_http_port(),
            "GMCP HTTP",
            &workspace,
            &global_dir,
            &bind_address,
        );
        if gmcp_http_port != cfg.gmcp_http_port() {
            cfg.settings.insert(
                "gmcp_http_port".to_string(),
                serde_json::json!(gmcp_http_port),
            );
            config_changed = true;
        }

        // 3. Bind A2A Cluster UDP Discovery Socket (Port 9092 / Dynamic)
        let (udp_socket, udp_port) = Self::bind_udp_with_fallback(
            cfg.udp_discovery_port(),
            &workspace,
            &global_dir,
            &bind_address,
        );
        if udp_port != cfg.udp_discovery_port() {
            cfg.settings.insert(
                "udp_discovery_port".to_string(),
                serde_json::json!(udp_port),
            );
            config_changed = true;
        }

        if config_changed {
            let _ = cfg.save(&global_dir);
            eprintln!(
                "[SusiDaemon] Port collisions detected. Updated configuration with active ports."
            );
        }

        // Spawn services with panic handling
        let workspace_gemi = workspace.clone();
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GemiServer::start_http_server(workspace_gemi, gemi_server);
            })) {
                eprintln!("[GEMI] Thread panicked: {:?}", e);
            }
        });

        let workspace_gmcp_http = workspace.clone();
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                GmcpServer::start_http_server(workspace_gmcp_http, gmcp_http_server);
            })) {
                eprintln!("[GMCP HTTP] Thread panicked: {:?}", e);
            }
        });

        let gmcp_actual_port = cfg.gmcp_port();
        thread::spawn(move || {
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Self::start_udp_discovery_server(udp_socket, gmcp_actual_port);
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
                    // Serialized Execution
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

        // Keep main daemon thread alive with graceful shutdown check
        while !ctx.is_shutdown_requested() {
            thread::sleep(Duration::from_secs(5));
        }

        eprintln!("[SusiDaemon] Graceful shutdown initiated");
    }

    fn bind_http_with_fallback(
        port: u16,
        name: &str,
        workspace: &Path,
        global_dir: &Path,
        bind_address: &str,
    ) -> (std::net::TcpListener, u16) {
        let addr = format!("{}:{}", bind_address, port);
        match std::net::TcpListener::bind(&addr) {
            Ok(listener) => (listener, port),
            Err(_) => {
                // AGGRESSIVE SELF-HEALING REFLEX: Attempt to reclaim constitutional port
                if Self::attempt_port_reclaim(port, global_dir)
                    && let Ok(listener) = std::net::TcpListener::bind(&addr)
                {
                    return (listener, port);
                }

                // Last resort: if even a random ephemeral port on the
                // requested bind address fails, try loopback specifically.
                // This runs on the daemon's main thread before any subsystem
                // thread is spawned, so there is genuinely no further
                // fallback left — but exit cleanly with a clear diagnostic
                // rather than an opaque panic trace.
                let listener = std::net::TcpListener::bind(format!("{}:0", bind_address))
                    .unwrap_or_else(|_| {
                        std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| {
                            eprintln!(
                                "[SusiDaemon] Fatal: could not bind any TCP port for {}, including the 127.0.0.1:0 last resort: {}",
                                name, e
                            );
                            std::process::exit(1);
                        })
                    });
                let new_port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
                susi_sandbox::manager::SusiAuditLogger::log(
                    workspace,
                    susi_sandbox::manager::LogLevel::Warning,
                    "SELF_HEALING_RANDOMIZATION",
                    &format!(
                        "{} default port {} occupied. Reclaim failed. Randomized to {}",
                        name, port, new_port
                    ),
                );
                eprintln!(
                    "[SusiDaemon] {} port collision! Randomized to {}",
                    name, new_port
                );
                (listener, new_port)
            }
        }
    }

    fn bind_udp_with_fallback(
        port: u16,
        workspace: &Path,
        global_dir: &Path,
        bind_address: &str,
    ) -> (std::net::UdpSocket, u16) {
        let addr = format!("{}:{}", bind_address, port);
        match std::net::UdpSocket::bind(&addr) {
            Ok(socket) => (socket, port),
            Err(_) => {
                // AGGRESSIVE SELF-HEALING REFLEX: Attempt to reclaim constitutional port
                if Self::attempt_port_reclaim(port, global_dir)
                    && let Ok(socket) = std::net::UdpSocket::bind(&addr)
                {
                    return (socket, port);
                }

                // Last resort, same reasoning as bind_http_with_fallback above.
                let socket = std::net::UdpSocket::bind(format!("{}:0", bind_address))
                    .unwrap_or_else(|_| {
                        std::net::UdpSocket::bind("127.0.0.1:0").unwrap_or_else(|e| {
                            eprintln!(
                                "[SusiDaemon] Fatal: could not bind any UDP port for discovery, including the 127.0.0.1:0 last resort: {}",
                                e
                            );
                            std::process::exit(1);
                        })
                    });
                let new_port = socket.local_addr().map(|a| a.port()).unwrap_or(0);
                susi_sandbox::manager::SusiAuditLogger::log(
                    workspace,
                    susi_sandbox::manager::LogLevel::Warning,
                    "SELF_HEALING_RANDOMIZATION",
                    &format!(
                        "UDP Discovery port {} occupied. Reclaim failed. Randomized to {}",
                        port, new_port
                    ),
                );
                eprintln!(
                    "[SusiDaemon] UDP port collision! Randomized to {}",
                    new_port
                );
                (socket, new_port)
            }
        }
    }

    /// Aggressive Port Reclaim: Interrogates the process holding a port and evicts it if it's a susi instance.
    fn attempt_port_reclaim(port: u16, global_dir: &Path) -> bool {
        #[cfg(unix)]
        {
            // Use fuser or lsof to find the PID
            let output = Command::new("sh")
                .arg("-c")
                .arg(format!("fuser {}/tcp 2>/dev/null", port))
                .output();

            if let Ok(out) = output {
                let pid_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if let Ok(pid) = pid_str.parse::<i32>()
                    && Self::is_trusted_susi_process(pid, global_dir)
                {
                    eprintln!(
                        "[Self-Healing] Evicting stale susi process (PID: {}) holding port {}...",
                        pid, port
                    );
                    unsafe {
                        libc::kill(pid, libc::SIGKILL);
                    }
                    thread::sleep(Duration::from_millis(100)); // Allow OS to release socket
                    return true;
                }
            }
        }
        false
    }

    /// Confirms `pid` is genuinely running a trusted susi binary before the
    /// Sovereign Eviction protocol (IDENTITY.md Mandate 22) is allowed to
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
        susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&exe_path)
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
        let mut buf = [0u8; 512];
        while let Ok((amt, src)) = socket.recv_from(&mut buf) {
            let msg = String::from_utf8_lossy(&buf[..amt]);
            if msg.contains("SUSI_LAN_PING") {
                let pong = format!("SUSI_LAN_PONG:susi-daemon-node:{}", gmcp_port);
                let _ = socket.send_to(pong.as_bytes(), src);
            }
        }
    }

    #[allow(dead_code)]
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
    fn test_lock_file_path() {
        let tmp_dir = std::env::temp_dir();
        let path = SusiDaemon::get_lock_file(&tmp_dir);
        assert_eq!(path, tmp_dir.join("substrate.lock"));
    }

    #[test]
    fn test_global_lock_is_mutually_exclusive_across_different_workspace_paths() {
        // Regression test for the bug this pass fixed: two `run_daemon_loop`
        // calls for two *different* workspaces both bind the same global
        // GEMI/GMCP/UDP ports, so the second one starting must refuse
        // rather than race the first and get evicted by its self-healing
        // "Sovereign Eviction" reflex. The actual mutual-exclusion
        // primitive is the global lock's flock, independent of which
        // workspace path is asking - this proves that property directly,
        // without needing to spin up two real daemon processes.
        let global_dir =
            std::env::temp_dir().join(format!("susi_global_lock_test_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&global_dir);
        let global_lock_path = SusiDaemon::get_lock_file(&global_dir);
        let _ = std::fs::remove_file(&global_lock_path);

        let first = DaemonLock::acquire(&global_lock_path);
        assert!(
            first.is_ok(),
            "the first daemon (any workspace) must be able to acquire the global lock"
        );

        let second = DaemonLock::acquire(&global_lock_path);
        assert!(
            second.is_err(),
            "a second daemon for a DIFFERENT workspace must be refused the global lock while the first is still running, since they'd otherwise fight over the same shared network ports"
        );

        drop(first);
        let third = DaemonLock::acquire(&global_lock_path);
        assert!(
            third.is_ok(),
            "once the first daemon releases the lock, a new one must be able to acquire it"
        );

        let _ = std::fs::remove_dir_all(&global_dir);
    }

    #[test]
    fn test_find_running_daemon_recovers_the_real_workspace_via_global_fallback() {
        // Regression test for the second bug this pass fixed: even after
        // the global lock closed the cross-workspace race, restarting a
        // daemon detected as "binary changed" via the global fallback used
        // the CALLING invocation's own workspace - silently moving the
        // daemon to wherever the caller happened to be, tearing down the
        // real one. `find_running_daemon` must report the daemon's actual
        // recorded workspace (workspace_a below), not the caller's
        // (workspace_b), and must say `same_workspace: false` so
        // `ensure_daemon_running` knows not to touch it.
        let global_dir =
            std::env::temp_dir().join(format!("susi_find_running_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&global_dir);
        std::fs::create_dir_all(&global_dir).unwrap();

        let workspace_a = global_dir.join("workspace_a");
        let workspace_b = global_dir.join("workspace_b");
        std::fs::create_dir_all(&workspace_a).unwrap();
        std::fs::create_dir_all(&workspace_b).unwrap();

        // Simulate workspace_a's daemon having started. The global lock
        // records the workspace that owns the daemon.
        let global_lock_path = SusiDaemon::get_lock_file(&global_dir);
        let mut global_lock = DaemonLock::acquire(&global_lock_path).unwrap();
        global_lock.write_pid_for_workspace(&workspace_a).unwrap();

        // A CLI invocation from workspace_b asks "is a daemon running?" -
        // it has no lock of its own, so it must fall back to the global
        // lock and correctly report workspace_a's daemon, not its own path.
        let found = SusiDaemon::find_running_daemon(&workspace_b, &global_dir).expect(
            "a daemon is running (workspace_a's) and must be found via the global fallback",
        );
        assert_eq!(
            found.workspace, workspace_a,
            "must recover the daemon's REAL recorded workspace, not the caller's own workspace_b"
        );
        assert!(
            !found.same_workspace,
            "found only via the global fallback, not workspace_b's own lock - must not be reported as same_workspace"
        );
        assert_eq!(found.pid, std::process::id());

        // The same query from workspace_a's own perspective must report
        // same_workspace: true (found via its own lock directly).
        let found_own = SusiDaemon::find_running_daemon(&workspace_a, &global_dir).unwrap();
        assert!(found_own.same_workspace);
        assert_eq!(found_own.workspace, workspace_a);

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
            susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&bin_path).unwrap();
        let cached_hash_1 =
            susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash_cached(
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
            susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash_cached(
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
            susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash_cached(
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
            susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&bin_path).unwrap()
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

        let mut child = Command::new(&fake_bin).arg("5").spawn().unwrap();
        let pid = child.id() as i32;
        // Give the kernel a moment to populate /proc/{pid}/exe.
        thread::sleep(Duration::from_millis(50));

        // No trust anchor written yet: fails closed even though the name matches.
        assert!(!SusiDaemon::is_trusted_susi_process(pid, &global_dir));

        // Trust anchor matches the real binary's hash: now trusted.
        let real_hash =
            susi_sandbox::daemon_state::SusiDaemonState::calculate_binary_hash(&fake_bin).unwrap();
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
