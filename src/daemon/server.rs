// Always-On SUSI Substrate Daemon Process Manager
// 100% Rust implementation managing GMCP (Port 9090), GEMI (Port 9091) & A2A Cluster UDP (Port 9092)

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};

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
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tracing::{info, warn};

use crate::error::{EaiError, EaiResult};
use crate::gawd::ama::SusiMasterAgent;
use crate::gawd::queue::SubstratePulseQueue;
use crate::gemi::GemiServer;
use crate::gmcp::server::GmcpServer;
use crate::sandbox::manager::SusiConfig;

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
                    let msgs = crate::sandbox::manager::SusiMessages::load_global();
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

    fn write_pid(&mut self) -> Result<(), String> {
        self.file.set_len(0).map_err(|e| e.to_string())?;
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|e| e.to_string())?;
        write!(self.file, "{}", std::process::id()).map_err(|e| e.to_string())?;
        self.file.flush().map_err(|e| e.to_string())?;
        Ok(())
    }
}

impl SusiDaemon {
    pub fn get_lock_file(global_dir: &Path) -> PathBuf {
        global_dir.join("substrate.lock")
    }

    pub fn check_status(global_dir: &Path) -> Option<u32> {
        let lock_file_path = Self::get_lock_file(global_dir);
        if !lock_file_path.exists() {
            return None;
        }

        if let Ok(content) = fs::read_to_string(&lock_file_path) {
            if let Ok(pid) = content.trim().parse::<u32>() {
                #[cfg(unix)]
                {
                    if unsafe { libc::kill(pid as i32, 0) } == 0 {
                        return Some(pid);
                    } else {
                        let _ = fs::remove_file(&lock_file_path);
                        return None;
                    }
                }
                #[cfg(windows)]
                {
                    if Self::is_process_alive(&lock_file_path) {
                        return Some(pid);
                    } else {
                        let _ = fs::remove_file(&lock_file_path);
                        return None;
                    }
                }
            }
        }
        None
    }

    fn is_process_alive(lock_file_path: &Path) -> bool {
        if let Ok(content) = fs::read_to_string(lock_file_path) {
            if let Ok(pid) = content.trim().parse::<u32>() {
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
    /// a spurious mismatch (self-healing only after one overwrite cycle).
    pub fn verify_binary_integrity(bin_path: &Path, global_dir: &Path) -> EaiResult<bool> {
        let hash_file = Self::get_hash_file(global_dir);
        let current_sig = Self::calculate_binary_hash(bin_path)?;

        if hash_file.exists() {
            if let Ok(saved_sig) = fs::read_to_string(&hash_file) {
                if saved_sig.trim() == current_sig.trim() {
                    return Ok(true);
                }
            }
            let _ = fs::write(&hash_file, &current_sig);
            return Ok(false);
        }

        let _ = fs::write(&hash_file, &current_sig);
        Ok(true)
    }

    pub fn calculate_binary_hash(path: &Path) -> EaiResult<String> {
        use sha2::{Digest, Sha256};
        let mut file = fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 65536];
        while let Ok(n) = file.read(&mut buffer) {
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    pub fn ensure_daemon_running(workspace: &Path, global_dir: &Path) {
        let current_exe = std::env::current_exe().ok();
        let msgs = crate::sandbox::manager::SusiMessages::load_global();
        if let Some(pid) = Self::check_status(global_dir) {
            if let Some(ref exe) = current_exe {
                if let Ok(false) = Self::verify_binary_integrity(exe, global_dir) {
                    let def_recompiled = "[SusiDaemon] Binary recompiled. Restarting daemon PID {}...".to_string();
                    let msg = msgs.get("daemon", "binary_recompiled").unwrap_or(&def_recompiled);
                    info!("{}", msg.replace("{}", &pid.to_string()));
                    Self::stop_daemon(global_dir);
                } else {
                    return;
                }
            } else {
                return;
            }
        }
        let bin_name = if cfg!(target_os = "windows") {
            "bin/susi-engine.exe"
        } else {
            "bin/susi-engine"
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

        // Binary Integrity Check (Aspiration 4 Hardening)
        match Self::verify_binary_integrity(&bin_to_run, global_dir) {
            Ok(true) => {
                let def_verified = "[SusiDaemon] Binary integrity verified.".to_string();
                info!("{}", msgs.get("daemon", "binary_verified").unwrap_or(&def_verified));
            }
            Ok(false) => {
                let def_tampered = "[SusiDaemon] Binary integrity check FAILED.".to_string();
                warn!("{}", msgs.get("daemon", "binary_tampered").unwrap_or(&def_tampered));
            }
            Err(e) => warn!("[SusiDaemon] Could not verify binary integrity: {}", e),
        }

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            use std::process::Stdio;
            let mut cmd = Command::new(&bin_to_run);
            cmd.arg("daemon-start")
                .arg(workspace.to_str().unwrap_or("."))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
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
        if let Ok(metadata) = std::fs::metadata(&lock_file_path) {
            if let Ok(modified) = metadata.modified() {
                if let Ok(age) = modified.elapsed() {
                    if age.as_secs() > 3600 && !Self::is_process_alive(&lock_file_path) {
                        let _ = std::fs::remove_file(&lock_file_path);
                    }
                }
            }
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

        if let Err(e) = lock.write_pid() {
            eprintln!("[SusiDaemon] Failed to write PID to lock file: {}", e);
            return;
        }

        let ctx = DaemonContext::new(lock);
        if let Err(e) = ctx.setup_signal_handlers() {
            eprintln!("[SusiDaemon] Signal handler setup failed: {}", e);
        }

        let mut cfg = SusiConfig::load(&global_dir).expect("Fatal: Malformed configuration");
        let mut config_changed = false;

        // Substrate Administration & Hardware Optimization (Pillar 1)
        crate::daemon::runtime_admin::SusiRuntimeAdmin::start_administration_cycle(&workspace);

        // Spawn Autonomous Background Model Provisioner & Resumable Downloader
        crate::gemi::models::ModelManager::spawn_background_hardware_model_provisioner(&workspace);

        // 1. Bind GEMI HTTP Server (Port 9091 / Dynamic)
        let (gemi_server, gemi_port) =
            Self::bind_http_with_fallback(cfg.gemi_port(), "GEMI", &workspace);
        if gemi_port != cfg.gemi_port() {
            cfg.settings.insert("gemi_port".to_string(), serde_json::json!(gemi_port));
            config_changed = true;
        }

        // 2. Bind GMCP HTTP/SSE Server (Port 9093 / Dynamic)
        let (gmcp_http_server, gmcp_http_port) =
            Self::bind_http_with_fallback(cfg.gmcp_http_port(), "GMCP HTTP", &workspace);
        if gmcp_http_port != cfg.gmcp_http_port() {
            cfg.settings.insert("gmcp_http_port".to_string(), serde_json::json!(gmcp_http_port));
            config_changed = true;
        }

        // 3. Bind A2A Cluster UDP Discovery Socket (Port 9092 / Dynamic)
        let (udp_socket, udp_port) =
            Self::bind_udp_with_fallback(cfg.udp_discovery_port(), &workspace);
        if udp_port != cfg.udp_discovery_port() {
            cfg.settings.insert("udp_discovery_port".to_string(), serde_json::json!(udp_port));
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

        // Aspiration 32: Continuous Interaction Substrate Worker
        let workspace_pulse = workspace.clone();
        thread::spawn(move || {
            let queue = SubstratePulseQueue::global();
            let ama = SusiMasterAgent::new();

            loop {
                if let Some(pulse) = queue.pop() {
                    // Serialized Execution (Mandate 31)
                    info!("[SubstratePulseQueue] Processing Pulse: {}", pulse.intent);
                    let _ = ama.solve_stream(&pulse.intent, &workspace_pulse, crate::SUSI_VERSION, &|_| {});
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
    ) -> (std::net::TcpListener, u16) {
        let addr = format!("127.0.0.1:{}", port);
        match std::net::TcpListener::bind(&addr) {
            Ok(listener) => (listener, port),
            Err(_) => {
                // AGGRESSIVE SELF-HEALING REFLEX: Attempt to reclaim constitutional port
                if Self::attempt_port_reclaim(port) {
                    if let Ok(listener) = std::net::TcpListener::bind(&addr) {
                        return (listener, port);
                    }
                }

                let listener = std::net::TcpListener::bind("127.0.0.1:0")
                    .expect("Failed to bind to random port");
                let new_port = listener.local_addr().unwrap().port();
                crate::sandbox::manager::SusiAuditLogger::log(
                    workspace,
                    crate::sandbox::manager::LogLevel::Warning,
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

    fn bind_udp_with_fallback(port: u16, workspace: &Path) -> (std::net::UdpSocket, u16) {
        let addr = format!("127.0.0.1:{}", port);
        match std::net::UdpSocket::bind(&addr) {
            Ok(socket) => (socket, port),
            Err(_) => {
                // AGGRESSIVE SELF-HEALING REFLEX: Attempt to reclaim constitutional port
                if Self::attempt_port_reclaim(port) {
                    if let Ok(socket) = std::net::UdpSocket::bind(&addr) {
                        return (socket, port);
                    }
                }

                let socket =
                    std::net::UdpSocket::bind("127.0.0.1:0").expect("Failed to bind random UDP port");
                let new_port = socket.local_addr().unwrap().port();
                crate::sandbox::manager::SusiAuditLogger::log(
                    workspace,
                    crate::sandbox::manager::LogLevel::Warning,
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
    fn attempt_port_reclaim(port: u16) -> bool {
        #[cfg(unix)]
        {
            // Use fuser or lsof to find the PID
            let output = Command::new("sh")
                .arg("-c")
                .arg(format!("fuser {}/tcp 2>/dev/null", port))
                .output();

            if let Ok(out) = output {
                let pid_str = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if let Ok(pid) = pid_str.parse::<i32>() {
                    // Check if this process is a susi-engine or susi
                    let comm_output = fs::read_to_string(format!("/proc/{}/comm", pid));
                    if let Ok(comm) = comm_output {
                        if comm.contains("susi") {
                            eprintln!("[Self-Healing] Evicting stale susi process (PID: {}) holding port {}...", pid, port);
                            unsafe {
                                libc::kill(pid, libc::SIGKILL);
                            }
                            thread::sleep(Duration::from_millis(100)); // Allow OS to release socket
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    fn start_udp_discovery_server(socket: std::net::UdpSocket, gmcp_port: u16) {
        
        eprintln!(
            "[A2A Cluster UDP] Discovery listener active on {}",
            socket.local_addr().unwrap()
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
    pub fn stop_daemon(global_dir: &Path) -> bool {
        let lock_file = Self::get_lock_file(global_dir);
        if lock_file.exists() {
            if let Ok(content) = fs::read_to_string(&lock_file) {
                if let Ok(pid) = content.trim().parse::<i32>() {
                    #[cfg(unix)]
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
            let _ = fs::remove_file(&lock_file);
            true
        } else {
            false
        }
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
    fn test_daemon_status_and_lifecycle_when_not_running() {
        let tmp_dir = std::env::temp_dir();
        let lock_file = SusiDaemon::get_lock_file(&tmp_dir);
        let _ = std::fs::remove_file(&lock_file);

        let status = SusiDaemon::check_status(&tmp_dir);
        assert!(status.is_none());

        let stopped = SusiDaemon::stop_daemon(&tmp_dir);
        assert!(!stopped);
    }
}
