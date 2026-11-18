#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test-only assertions

//! Automatic host-port fallback, end to end (CEO decision 2026-11-08).
//!
//! A plain `TcpListener` stands in for Prometheus: it holds the GMCP port of
//! the first port block that is free on this host. A daemon of a throwaway
//! instance, with no offset pinned, must then serve its whole host contract
//! at another block of 100, publish that block with its pid in `ports.json`,
//! and a client command of the same instance must resolve it. `susi stop`
//! then removes the publication.
//!
//! The daemon runs as this test's own child (`susi daemon-start`), so a
//! failure can never leave it running: every wait is bounded and the child
//! is stopped, then killed, on every exit path.

use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use susi_paths::test_env::isolate_home;

/// The six ports the daemon binds as one block, with their transport.
const BLOCK: [(u16, bool); 6] = [
    (9090, true),
    (9091, true),
    (9092, false),
    (9093, true),
    (9094, true),
    (9095, false),
];

/// Same bind probe the daemon uses for a loopback instance.
fn block_free(offset: u16) -> bool {
    BLOCK.iter().all(|(port, tcp)| {
        let port = port + offset;
        if *tcp {
            TcpListener::bind(("127.0.0.1", port)).is_ok()
        } else {
            UdpSocket::bind(("127.0.0.1", port)).is_ok()
                && UdpSocket::bind(("0.0.0.0", port)).is_ok()
        }
    })
}

fn scratch_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "susi-port-fallback-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `susi` under the throwaway `home`, as instance `instance` with no offset.
fn susi(home: &Path, instance: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    isolate_home(&mut cmd, home);
    cmd.env("SUSI_HOME", instance)
        .env_remove("SUSI_PORT_OFFSET")
        .current_dir(home);
    cmd
}

/// Run a client command to completion within `limit`, killing it after.
fn run_bounded(mut cmd: Command, limit: Duration) -> (bool, String, String) {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + limit;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let out = child.wait_with_output().unwrap();
            return (
                status.success(),
                String::from_utf8_lossy(&out.stdout).into_owned(),
                String::from_utf8_lossy(&out.stderr).into_owned(),
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "command did not finish within {limit:?}; stderr:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Stops the daemon child on every exit path.
struct Daemon {
    child: Child,
    home: PathBuf,
    instance: PathBuf,
    log: PathBuf,
}

impl Daemon {
    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let mut stop = susi(&self.home, &self.instance);
            stop.arg("stop").stdout(Stdio::null()).stderr(Stdio::null());
            if let Ok(mut stopper) = stop.spawn() {
                let deadline = Instant::now() + Duration::from_secs(40);
                while matches!(stopper.try_wait(), Ok(None)) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = stopper.kill();
                let _ = stopper.wait();
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[test]
fn an_unpinned_daemon_serves_the_next_free_block_and_publishes_it() {
    let held_offset = (0..=20u16)
        .map(|k| k * 100)
        .find(|offset| block_free(*offset))
        .expect("no free block of host ports on this machine");
    // The stand-in for a foreign service on the block the daemon would take.
    let foreign = TcpListener::bind(("127.0.0.1", 9090 + held_offset)).unwrap();

    let home = scratch_home("home");
    let instance = home.join("instance");
    std::fs::create_dir_all(&instance).unwrap();
    let log = home.join("daemon.log");
    let mut cmd = susi(&home, &instance);
    cmd.arg("daemon-start")
        .arg("--workspace")
        .arg(&instance)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap());
    let mut daemon = Daemon {
        child: cmd.spawn().unwrap(),
        home: home.clone(),
        instance: instance.clone(),
        log,
    };
    let pid = daemon.child.id();

    // The daemon publishes ports.json once its contract is bound.
    let ports_file = instance.join("ports.json");
    let deadline = Instant::now() + Duration::from_secs(150);
    let published = loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            panic!("daemon exited early ({status}); log:\n{}", daemon.log());
        }
        let parsed = std::fs::read_to_string(&ports_file)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .filter(|v| v["pid"].as_u64() == Some(u64::from(pid)));
        if let Some(v) = parsed {
            break v;
        }
        assert!(
            Instant::now() < deadline,
            "no ports.json from pid {pid} within 150 s; log:\n{}",
            daemon.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    };

    let offset = u16::try_from(published["offset"].as_u64().unwrap()).unwrap();
    assert_ne!(offset, held_offset, "the held block must not be served");
    assert_eq!(offset % 100, 0, "uniform block of 100: {published}");
    assert!(offset <= 2000, "{published}");
    assert_eq!(published["fallback_from"], 0, "{published}");
    assert_eq!(published["fallback"], true, "{published}");
    assert_eq!(
        published["ports"]["gmcp"].as_u64(),
        Some(u64::from(9090 + offset))
    );
    assert_eq!(
        published["ports"]["a2a_http"].as_u64(),
        Some(u64::from(9094 + offset))
    );
    let substrate_pid = std::fs::read_to_string(instance.join("substrate.pid")).unwrap();
    assert_eq!(
        substrate_pid.lines().next().map(str::trim),
        Some(pid.to_string().as_str()),
        "ports.json names the substrate-lock holder"
    );
    let log = daemon.log();
    assert!(
        log.contains("[SusiDaemon] canonical port 90")
            && log.contains(") held by ")
            && log.contains(&format!("serving the host contract at offset {offset}:")),
        "the daemon logs the holder and the block; log:\n{log}"
    );
    TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], 9090 + offset)),
        Duration::from_secs(2),
    )
    .expect("the daemon serves GMCP at the fallback block");

    // A client of the same instance, with no offset of its own, resolves the
    // fallback from the live daemon's publication.
    let mut status = susi(&home, &instance);
    status.args(["os", "status", "--json"]);
    let (ok, stdout, stderr) = run_bounded(status, Duration::from_secs(60));
    assert!(ok, "susi os status failed: {stderr}");
    let report: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("os status --json is not JSON ({e}): {stdout}"));
    assert_eq!(
        report["port_block"]["offset"], offset,
        "{}",
        report["port_block"]
    );
    assert_eq!(
        report["port_block"]["fallback"], true,
        "{}",
        report["port_block"]
    );
    assert_eq!(
        report["port_block"]["daemon_pid"], pid,
        "{}",
        report["port_block"]
    );

    // A clean stop withdraws the publication.
    let mut stop = susi(&home, &instance);
    stop.arg("stop");
    let (ok, stdout, stderr) = run_bounded(stop, Duration::from_secs(60));
    assert!(ok, "susi stop failed: {stdout}{stderr}");
    let deadline = Instant::now() + Duration::from_secs(20);
    while matches!(daemon.child.try_wait(), Ok(None)) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        daemon.child.try_wait().unwrap().is_some(),
        "the daemon outlived susi stop"
    );
    assert!(
        !ports_file.exists(),
        "ports.json must not claim a block after the daemon stopped"
    );
    drop(daemon);
    drop(foreign);
    let _ = std::fs::remove_dir_all(&home);
}
