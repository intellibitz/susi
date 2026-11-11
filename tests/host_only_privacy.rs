// Integration test crate: a failed setup step is a test failure, so the
// panic-path lints are allowed for the whole file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The `host_only` privacy mode (T-HOSTONLY-1), end to end over real sockets:
//!
//! - in process, through the real peer client: a call to a routed
//!   non-loopback IPv4 address of this host, attested member or not, is
//!   refused naming host-only mode and the listener there sees no
//!   connection; a loopback peer is still reached; switching to
//!   `local_only` reaches the same off-host listener again, and what
//!   arrives there is the sealed peer channel's preamble, never a
//!   plaintext request;
//! - a real daemon (`susi daemon-start`) bound to every interface: under
//!   `host_only` a signed discovery ping from the routed address gets no
//!   pong and no listener answers a client from that address, not even a
//!   TLS handshake, while loopback pings and clients are served; `susi
//!   privacy mode local_only` on the running daemon restores the pong and
//!   the TLS answer, and `host_only` closes them again. Plaintext HTTP from
//!   the routed address is refused in every mode (remote plaintext is
//!   retired; machines talk over the sealed peer channel).
//!
//! Both need a routed non-loopback IPv4 interface (the gate host has one);
//! without one they fail naming that, rather than pass vacuously. Every
//! accept, read and wait is bounded, so a regression fails, never hangs.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use susi_config::cluster_key;
use susi_core::commit_log;
use susi_core::mac_policy::{self, MacPolicy, PrivacyMode};
use susi_core::mcp_client;
use susi_paths::test_env::{isolate_home, EnvGuard};

mod common;
use common::HomeGuard;

/// This host's non-loopback IPv4 address: the source address the OS would
/// use toward a TEST-NET destination. UDP `connect` sends nothing.
fn lan_ip() -> IpAddr {
    let probe = UdpSocket::bind("0.0.0.0:0").expect("bind probe socket");
    probe
        .connect("192.0.2.1:9")
        .expect("this test needs a non-loopback IPv4 interface with a route");
    let ip = probe.local_addr().expect("probe local address").ip();
    assert!(
        !ip.is_loopback() && !ip.is_unspecified(),
        "this test needs a routed non-loopback IPv4 address on this host (found {ip})"
    );
    ip
}

/// A one-shot listener on `ip` that forwards the head of the first request
/// it receives and answers 401. It stops accepting after `window`.
fn fake_peer(ip: IpAddr, window: Duration) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind((ip, 0)).expect("bind fake peer");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let addr = listener
        .local_addr()
        .expect("fake peer address")
        .to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            };
            stream.set_nonblocking(false).expect("blocking stream");
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && head.len() < 64 * 1024 {
                match stream.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&head).to_ascii_lowercase());
            let _ = stream.write_all(
                b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
            return;
        }
    });
    (addr, rx)
}

fn write_roster(dir: &Path, node_id: &str, addr: &str, pubkey: &str) {
    let row = serde_json::json!({
        "node_id": node_id,
        "address": addr,
        "channel_endpoint": addr,
        "node_type": "PEER",
        "admission": "explicit",
        "pubkey": pubkey,
        "key_bound_at": 1u64,
    });
    std::fs::write(
        dir.join("peers.json"),
        serde_json::to_string_pretty(&vec![row]).unwrap(),
    )
    .unwrap();
}

#[test]
fn host_only_refuses_peer_calls_off_host_before_any_connection() {
    let _socket = common::SocketTestGuard::acquire();
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = HomeGuard::new("host-only-peer");
    std::fs::write(
        home.config().join("cluster.key"),
        susi_paths::hex::encode([0x6Bu8; 32]),
    )
    .unwrap();
    // The persisted mode the wired policy reads on every check: this
    // binary's process-wide policy is created here, under this home.
    let substrate = susi_paths::SusiDirs::substrate_home();
    std::fs::create_dir_all(&substrate).unwrap();
    std::fs::write(substrate.join("privacy_mode"), "host_only").unwrap();
    let policy = MacPolicy::global();
    assert_eq!(policy.mode(), PrivacyMode::HostOnly);

    let ip = lan_ip();
    let (addr, heads) = fake_peer(ip, Duration::from_secs(20));
    // An attested member: a node key bound to the address. Host-only
    // refuses it all the same.
    let member_pk = cluster_key::node_pubkey_hex().expect("node.key generates");
    write_roster(&home.config(), "member-b", &addr, &member_pk);
    let bearer = cluster_key::peer_bearer().expect("cluster.key yields the peer bearer");

    for credential in [Some(bearer.as_str()), None] {
        let err =
            mcp_client::call_tool(&addr, "cluster_status", &serde_json::json!({}), credential)
                .unwrap_err();
        assert!(err.contains("host_only"), "{err}");
        assert!(err.contains("host-only privacy mode"), "{err}");
    }
    assert!(
        !mac_policy::egress_permitted(&format!("http://{addr}/mcp")),
        "the egress gate refuses the LAN target too"
    );
    assert!(
        heads.recv_timeout(Duration::from_millis(1500)).is_err(),
        "host-only must refuse before a connection opens"
    );

    // A loopback peer is this machine: still reached.
    let (local, local_heads) = fake_peer(IpAddr::from([127, 0, 0, 1]), Duration::from_secs(10));
    let err =
        mcp_client::call_tool(&local, "cluster_status", &serde_json::json!({}), None).unwrap_err();
    assert!(err.contains("HTTP 401"), "{err}");
    let head = local_heads
        .recv_timeout(Duration::from_secs(8))
        .expect("the loopback peer receives the request");
    assert!(head.starts_with("post /mcp"), "{head}");

    // The refusal is the mode's: under local_only the same off-host
    // listener is reached by the same call — inside the sealed peer
    // channel (the listener is no channel peer, so the handshake fails),
    // never as a plaintext request.
    policy.set_mode(PrivacyMode::LocalOnly).unwrap();
    let err =
        mcp_client::call_tool(&addr, "cluster_status", &serde_json::json!({}), None).unwrap_err();
    assert!(err.contains("peer channel"), "{err}");
    let head = heads
        .recv_timeout(Duration::from_secs(15))
        .expect("local_only reaches the LAN listener");
    assert!(head.starts_with("susipc1"), "{head}");
    assert!(!head.contains("/mcp"), "{head}");
    policy.set_mode(PrivacyMode::HostOnly).unwrap();
    assert!(!mac_policy::egress_permitted(&format!("http://{addr}/mcp")));
}

/// The six ports the daemon binds as one block, with their transport.
const BLOCK: [(u16, bool); 6] = [
    (9090, true),
    (9091, true),
    (9092, false),
    (9093, true),
    (9094, true),
    (9095, false),
];

/// A block of host ports free on every interface, away from the blocks the
/// port-fallback test and a default daemon use.
fn free_wide_offset() -> u16 {
    // Reserved machine-wide until this test process exits, so another
    // agent's gate running the same test takes the next block instead.
    common::reserve_port_block((40..=90u16).map(|k| k * 100), |offset| {
        BLOCK.iter().all(|(port, tcp)| {
            let port = port + offset;
            if *tcp {
                TcpListener::bind(("0.0.0.0", port)).is_ok()
                    && TcpListener::bind(("127.0.0.1", port)).is_ok()
            } else {
                UdpSocket::bind(("0.0.0.0", port)).is_ok()
                    && UdpSocket::bind(("127.0.0.1", port)).is_ok()
            }
        })
    })
    .expect("no free block of host ports on this machine")
}

/// `susi` under the throwaway `home`, as instance `instance`, pinned to
/// `offset`.
fn susi(home: &Path, instance: &Path, offset: u16) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    isolate_home(&mut cmd, home);
    cmd.env("SUSI_HOME", instance)
        .env("SUSI_PORT_OFFSET", offset.to_string())
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
    offset: u16,
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
            let mut stop = susi(&self.home, &self.instance, self.offset);
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

/// Whether the daemon's discovery responder at `target` answers a signed
/// ping sent from `source` with a signed pong echoing its nonce.
fn discovery_answers(source: IpAddr, target: SocketAddr) -> bool {
    let (ping, nonce) = cluster_key::signed_ping_v3("CORE", 0, "0000000000000000", target)
        .expect("a signed ping toward the responder");
    let socket = UdpSocket::bind((source, 0)).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    socket.send_to(ping.as_bytes(), target).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buf = [0u8; 4096];
    while Instant::now() < deadline {
        match socket.recv_from(&mut buf) {
            Ok((n, _)) => {
                let msg = String::from_utf8_lossy(&buf[..n]);
                if cluster_key::verify_signed_pong(&msg, &nonce).is_some() {
                    return true;
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            // Windows reports an ICMP port-unreachable on the next recv.
            Err(_) => return false,
        }
    }
    false
}

/// A well-formed TLS 1.2 ClientHello record (ECDHE with X25519 or P-256,
/// AES-128-GCM, ECDSA and RSA signature schemes, secure renegotiation):
/// a TLS listener that negotiates with the client, whichever TLS stack the
/// platform gives it, answers with a ServerHello.
fn client_hello() -> Vec<u8> {
    let mut ext = Vec::new();
    // supported_groups: x25519, secp256r1
    ext.extend_from_slice(&[0x00, 0x0a, 0x00, 0x06, 0x00, 0x04, 0x00, 0x1d, 0x00, 0x17]);
    // ec_point_formats: uncompressed
    ext.extend_from_slice(&[0x00, 0x0b, 0x00, 0x02, 0x01, 0x00]);
    // signature_algorithms: ecdsa_secp256r1_sha256, rsa_pss_rsae_sha256,
    // rsa_pkcs1_sha256
    ext.extend_from_slice(&[
        0x00, 0x0d, 0x00, 0x08, 0x00, 0x06, 0x04, 0x03, 0x08, 0x04, 0x04, 0x01,
    ]);
    // renegotiation_info: empty (initial handshake)
    ext.extend_from_slice(&[0xff, 0x01, 0x00, 0x01, 0x00]);
    let mut body = vec![0x03, 0x03];
    body.extend_from_slice(&[0x5a; 32]); // random
    body.push(0x00); // session id
                     // ECDHE_ECDSA / ECDHE_RSA with AES-128-GCM-SHA256
    body.extend_from_slice(&[0x00, 0x04, 0xc0, 0x2b, 0xc0, 0x2f]);
    body.extend_from_slice(&[0x01, 0x00]); // no compression
    body.extend_from_slice(&u16::try_from(ext.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&ext);
    let mut handshake = vec![0x01, 0x00];
    handshake.extend_from_slice(&u16::try_from(body.len()).unwrap().to_be_bytes());
    handshake.extend_from_slice(&body);
    let mut record = vec![0x16, 0x03, 0x01];
    record.extend_from_slice(&u16::try_from(handshake.len()).unwrap().to_be_bytes());
    record.extend_from_slice(&handshake);
    record
}

/// What a listener at `target` sends back to a TLS ClientHello from this
/// host's address on the route to it: a handshake record (ServerHello)
/// when it negotiates TLS with that client, nothing when it drops the
/// client first.
fn tls_reply(target: SocketAddr) -> Vec<u8> {
    let Ok(mut stream) = TcpStream::connect_timeout(&target, Duration::from_secs(3)) else {
        return Vec::new();
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let _ = stream.write_all(&client_hello());
    let mut reply = Vec::new();
    let mut chunk = [0u8; 64];
    while reply.len() < 64 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => reply.extend_from_slice(&chunk[..n]),
        }
    }
    reply
}

/// What an HTTP listener at `target` sends back to a plain request from
/// this host's address on the route to it: the first bytes of a response,
/// or empty when the connection is closed without one.
fn http_reply(target: SocketAddr) -> String {
    let Ok(mut stream) = TcpStream::connect_timeout(&target, Duration::from_secs(3)) else {
        return String::new();
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let _ = stream.write_all(
        format!("GET / HTTP/1.1\r\nhost: {target}\r\nconnection: close\r\n\r\n").as_bytes(),
    );
    let mut reply = Vec::new();
    let mut chunk = [0u8; 256];
    while reply.len() < 64 {
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => reply.extend_from_slice(&chunk[..n]),
        }
    }
    String::from_utf8_lossy(&reply).into_owned()
}

/// Wait for a loopback listener to answer after another listener's policy
/// probe. Under the concurrent native gate, an accept loop can be briefly
/// delayed even after the daemon has published all of its ports.
fn loopback_http_reply(target: SocketAddr, deadline: Instant) -> bool {
    while Instant::now() < deadline {
        if http_reply(target).starts_with("HTTP/") {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

fn set_mode(daemon: &Daemon, mode: &str) {
    let mut cmd = susi(&daemon.home, &daemon.instance, daemon.offset);
    cmd.args(["privacy", "mode", mode]);
    let (ok, stdout, stderr) = run_bounded(cmd, Duration::from_secs(60));
    assert!(ok, "susi privacy mode {mode} failed: {stdout}{stderr}");
    assert!(
        stdout.contains(&format!("\"mode\": \"{mode}\"")),
        "{stdout}"
    );
}

#[test]
fn a_daemon_bound_beyond_loopback_serves_this_machine_alone_under_host_only() {
    let _socket = common::SocketTestGuard::acquire();
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let ip = lan_ip();
    let offset = free_wide_offset();
    let home = std::env::temp_dir().join(format!(
        "susi-host-only-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let instance = home.join("instance");
    std::fs::create_dir_all(&instance).unwrap();
    // This process resolves the instance exactly as the daemon child does,
    // so the pings it signs carry the daemon's cluster key.
    let mut env = EnvGuard::isolated();
    env.set("HOME", &home);
    env.set("XDG_CONFIG_HOME", home.join(".config"));
    env.set("XDG_DATA_HOME", home.join(".local").join("share"));
    env.set("XDG_CACHE_HOME", home.join(".cache"));
    env.set("SUSI_HOME", &instance);
    env.set("SUSI_PORT_OFFSET", offset.to_string());
    let config_dir = susi_paths::SusiDirs::config_dir();
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
        config_dir.join("cluster.key"),
        susi_paths::hex::encode([0x3Cu8; 32]),
    )
    .unwrap();
    // Every interface: a self-signed certificate is provisioned, so only the
    // mode stands between an off-host TLS client and the listeners. The
    // retired `allow_remote_plaintext: true` stays in the file to show that
    // it opens nothing.
    std::fs::write(
        config_dir.join("config.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "bind_address": "0.0.0.0",
            "allow_remote_plaintext": true,
        }))
        .unwrap(),
    )
    .unwrap();
    let substrate = susi_paths::SusiDirs::substrate_home();
    std::fs::create_dir_all(&substrate).unwrap();
    std::fs::write(substrate.join("privacy_mode"), "host_only").unwrap();

    let log = home.join("daemon.log");
    let mut cmd = susi(&home, &instance, offset);
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
        offset,
        log,
    };
    let pid = daemon.child.id();
    let ports_file = instance.join("ports.json");
    let deadline = Instant::now() + Duration::from_secs(150);
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            panic!("daemon exited early ({status}); log:\n{}", daemon.log());
        }
        let published = std::fs::read_to_string(&ports_file)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .is_some_and(|v| v["pid"].as_u64() == Some(u64::from(pid)));
        if published {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no ports.json from pid {pid} within 150 s; log:\n{}",
            daemon.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }

    let loopback = IpAddr::from([127, 0, 0, 1]);
    let udp = 9092 + offset;
    let tcp = [9090 + offset, 9091 + offset, 9093 + offset, 9094 + offset];
    // The listeners come up right after the publication, but each HTTP
    // surface has its own accept loop. Under the native gate, another test
    // binary can delay one surface after GMCP is already answering, so wait
    // for every advertised loopback listener before asserting policy.
    let deadline = Instant::now() + Duration::from_secs(60);
    for port in tcp {
        while !http_reply(SocketAddr::new(loopback, port)).starts_with("HTTP/") {
            assert!(
                Instant::now() < deadline,
                "listener on port {port} never answered on loopback; log:\n{}",
                daemon.log()
            );
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    let assert_host_only = |daemon: &Daemon| {
        assert!(
            discovery_answers(loopback, SocketAddr::new(loopback, udp)),
            "the loopback readiness ping is still answered; log:\n{}",
            daemon.log()
        );
        assert!(
            !discovery_answers(ip, SocketAddr::new(ip, udp)),
            "host_only answered a discovery ping from {ip}"
        );
        for port in tcp {
            let reply = http_reply(SocketAddr::new(ip, port));
            assert!(
                reply.is_empty(),
                "host_only served an off-host client on port {port}: {reply:?}"
            );
            let tls = tls_reply(SocketAddr::new(ip, port));
            assert!(
                tls.is_empty(),
                "host_only negotiated TLS with an off-host client on port {port}: {tls:?}"
            );
            assert!(
                loopback_http_reply(
                    SocketAddr::new(loopback, port),
                    Instant::now() + Duration::from_secs(15),
                ),
                "loopback client on port {port} must be served; log:\n{}",
                daemon.log()
            );
        }
    };
    assert_host_only(&daemon);

    // The running daemon follows a mode switch with no restart.
    set_mode(&daemon, "local_only");
    assert!(
        discovery_answers(ip, SocketAddr::new(ip, udp)),
        "local_only must answer the LAN discovery ping again; log:\n{}",
        daemon.log()
    );
    for port in tcp {
        let tls = tls_reply(SocketAddr::new(ip, port));
        assert!(
            tls.first() == Some(&0x16),
            "local_only must negotiate TLS with the off-host client on port {port}: {tls:?}; log:\n{}",
            daemon.log()
        );
        let reply = http_reply(SocketAddr::new(ip, port));
        assert!(
            reply.is_empty(),
            "remote plaintext is refused in every mode, port {port}: {reply:?}"
        );
    }

    set_mode(&daemon, "host_only");
    assert_host_only(&daemon);

    drop(daemon);
    drop(env);
    let _ = std::fs::remove_dir_all(&home);
}
