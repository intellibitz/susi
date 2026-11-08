// Integration test crate: a failed setup step is a test failure, so the
// panic-path lints are allowed for the whole file.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The sealed peer channel in the hub topology (T-PEERCHAN-1,
//! `docs/peer-channel.md`), end to end over real sockets:
//!
//! - three daemons (`susi daemon-start`) in throwaway homes on explicit
//!   port offsets of 500 and up: a hub `H` that listens on a dedicated
//!   channel port, and two members `A`, `B` that have no listener at all and
//!   reach `H` only by dialing out, through a recording TCP forwarder that
//!   stands in for the home router's port forward;
//! - each roster advertises its members at TEST-NET addresses that are not
//!   where their connections come from (every real source is loopback via
//!   the forwarder), so every call below succeeds only because membership
//!   is keyed by identity, not by address;
//! - calls: member to hub, member to member relayed by the hub, and hub to
//!   member over the member's outbound uplink — each one an MCP
//!   `initialize` answered by the destination's GMCP surface with no bearer;
//! - refusals: plaintext HTTP on the channel port, a replayed handshake, a
//!   wrong identity, a node without the cluster key, and an unbound
//!   identity asking the hub to relay;
//! - confidentiality: the forwarder's capture (what the network path sees)
//!   and the hub's relay trace (what the hub reads of a relayed call after
//!   opening its own link encryption) hold no request line, header, JSON or
//!   node id.
//!
//! Every accept, read and wait is bounded: a regression fails, never hangs.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use susi_config::peer_channel::{Expect, LocalIdentity};
use susi_config::peer_channel_link::{self as link, Target};
use susi_core::commit_log;
use susi_paths::test_env::{isolate_home, EnvGuard};

const CLUSTER_KEY: [u8; 32] = [0x5Au8; 32];

/// Ports of one contract block at offset 0 (TCP true, UDP false).
const BLOCK: [(u16, bool); 6] = [
    (9090, true),
    (9091, true),
    (9092, false),
    (9093, true),
    (9094, true),
    (9095, false),
];

fn block_free(offset: u16) -> bool {
    BLOCK.iter().all(|(port, tcp)| {
        let port = port + offset;
        if *tcp {
            TcpListener::bind(("127.0.0.1", port)).is_ok()
        } else {
            std::net::UdpSocket::bind(("127.0.0.1", port)).is_ok()
        }
    })
}

/// Three free blocks at offsets of 500 and up.
fn free_offsets() -> [u16; 3] {
    let mut found = Vec::new();
    let mut offset = 500u16;
    while found.len() < 3 && offset <= 1900 {
        if block_free(offset) {
            found.push(offset);
        }
        offset += 100;
    }
    assert_eq!(
        found.len(),
        3,
        "no three free port blocks between offsets 500 and 1900"
    );
    [found[0], found[1], found[2]]
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Node {
    name: &'static str,
    home: PathBuf,
    instance: PathBuf,
    offset: u16,
    config: PathBuf,
    node_id: String,
    pubkey: String,
}

/// The env of a client process running as `node` (this test process
/// switches identity by switching homes).
fn env_for(node: &Node) -> EnvGuard {
    let mut env = EnvGuard::isolated();
    env.set("HOME", &node.home);
    env.set("USERPROFILE", &node.home);
    env.set("APPDATA", node.home.join("AppData").join("Roaming"));
    env.set("LOCALAPPDATA", node.home.join("AppData").join("Local"));
    env.set("XDG_CONFIG_HOME", node.home.join(".config"));
    env.set("XDG_DATA_HOME", node.home.join(".local").join("share"));
    env.set("XDG_CACHE_HOME", node.home.join(".cache"));
    env.set("SUSI_HOME", &node.instance);
    env.set("SUSI_PORT_OFFSET", node.offset.to_string());
    env
}

fn make_node(
    root: &Path,
    name: &'static str,
    offset: u16,
    seed: u8,
    cluster_key: [u8; 32],
) -> Node {
    let home = root.join(name);
    let instance = home.join("instance");
    std::fs::create_dir_all(&instance).unwrap();
    let mut node = Node {
        name,
        home,
        instance,
        offset,
        config: PathBuf::new(),
        node_id: format!("susi-node-pc{name}"),
        pubkey: String::new(),
    };
    let env = env_for(&node);
    node.config = susi_paths::SusiDirs::config_dir();
    drop(env);
    std::fs::create_dir_all(&node.config).unwrap();
    std::fs::write(node.config.join("cluster.key"), hex::encode(cluster_key)).unwrap();
    std::fs::write(node.config.join("node.key"), hex::encode([seed; 32])).unwrap();
    std::fs::write(node.config.join("node_id"), &node.node_id).unwrap();
    node.pubkey = LocalIdentity::from_seed(&[seed; 32], &node.node_id).pubkey_hex();
    node
}

fn write_json(path: &Path, value: &serde_json::Value) {
    std::fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

/// A bound roster row advertising `node` at `hint`, an address its traffic
/// never comes from.
fn roster_row(node: &Node, hint: &str) -> serde_json::Value {
    serde_json::json!({
        "node_id": node.node_id,
        "address": hint,
        "node_type": "PEER",
        "is_active": true,
        "capabilities": ["CORE"],
        "admission": "explicit",
        "pubkey": node.pubkey,
        "key_bound_at": 1u64,
        "last_seen_secs": 1u64,
    })
}

fn susi(node: &Node) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_susi"));
    isolate_home(&mut cmd, &node.home);
    cmd.env("SUSI_HOME", &node.instance)
        .env("SUSI_PORT_OFFSET", node.offset.to_string())
        .current_dir(&node.home);
    cmd
}

/// Stops the daemon child on every exit path.
struct Daemon {
    child: Child,
    stop: Command,
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
            self.stop.stdout(Stdio::null()).stderr(Stdio::null());
            if let Ok(mut stopper) = self.stop.spawn() {
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

fn start_daemon(node: &Node, extra_env: &[(&str, &Path)]) -> Daemon {
    let log = node.home.join("daemon.log");
    let mut cmd = susi(node);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.arg("daemon-start")
        .arg("--workspace")
        .arg(&node.instance)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap());
    let mut stop = susi(node);
    stop.arg("stop");
    Daemon {
        child: cmd.spawn().unwrap(),
        stop,
        log,
    }
}

/// Wait until `daemon` has published its ports (its listeners follow).
fn wait_published(node: &Node, daemon: &mut Daemon) {
    let pid = daemon.child.id();
    let ports_file = node.instance.join("ports.json");
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            panic!(
                "{} exited early ({status}); log:\n{}",
                node.name,
                daemon.log()
            );
        }
        let published = std::fs::read_to_string(&ports_file)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .is_some_and(|v| v["pid"].as_u64() == Some(u64::from(pid)));
        if published {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{}: no ports.json from pid {pid} within 180 s; log:\n{}",
            node.name,
            daemon.log()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Bytes a recording forwarder saw, per direction, over every connection.
#[derive(Default)]
struct Capture {
    to_hub: Vec<u8>,
    from_hub: Vec<u8>,
    /// Each connection's client-to-hub bytes, kept whole for the replay test.
    connections: Vec<Vec<u8>>,
}

/// A TCP forwarder in front of `target`: the stand-in for the router's port
/// forward. Every connection it relays reaches the hub from this process's
/// loopback socket, never from the member's own address.
fn recording_forwarder(target: SocketAddr) -> (SocketAddr, Arc<Mutex<Capture>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let capture = Arc::new(Mutex::new(Capture::default()));
    let cap = Arc::clone(&capture);
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { continue };
            let Ok(upstream) = TcpStream::connect_timeout(&target, Duration::from_secs(5)) else {
                continue;
            };
            let index = {
                let mut c = cap.lock().unwrap();
                c.connections.push(Vec::new());
                c.connections.len() - 1
            };
            for (mut from, mut to, toward_hub) in [
                (
                    client.try_clone().unwrap(),
                    upstream.try_clone().unwrap(),
                    true,
                ),
                (upstream, client, false),
            ] {
                let cap = Arc::clone(&cap);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 16 * 1024];
                    loop {
                        match from.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                {
                                    let mut c = cap.lock().unwrap();
                                    if toward_hub {
                                        c.to_hub.extend_from_slice(&buf[..n]);
                                        c.connections[index].extend_from_slice(&buf[..n]);
                                    } else {
                                        c.from_hub.extend_from_slice(&buf[..n]);
                                    }
                                }
                                if to.write_all(&buf[..n]).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    let _ = to.shutdown(std::net::Shutdown::Both);
                });
            }
        }
    });
    (addr, capture)
}

/// An MCP `initialize` over `stream`, as the destination's GMCP surface
/// answers it. Returns the raw HTTP response.
fn mcp_initialize(mut stream: link::ChannelStream) -> String {
    stream.set_read_timeout(Some(Duration::from_secs(30)));
    let body = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": {"name": "peer-channel-test", "version": "1"}
        }
    }))
    .unwrap();
    let head = format!(
        "POST /mcp HTTP/1.1\r\nhost: 127.0.0.1\r\naccept: application/json, text/event-stream\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(&body).unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(e) => panic!("reading the response over the channel: {e}"),
        }
        if out.len() > 1 << 20 {
            break;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Run a `susi` client command as `node` to completion within `limit`.
fn run_cli(node: &Node, args: &[&str], limit: Duration) -> (bool, String, String) {
    let mut cmd = susi(node);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
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
                "susi {args:?} as {} did not finish within {limit:?}; stderr:\n{}",
                node.name,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `susi peers status --json` as `node`: the status row each member's
/// daemon answered through the real peer client, keyed by node id.
fn peers_status(node: &Node) -> std::collections::HashMap<String, serde_json::Value> {
    let (ok, stdout, stderr) = run_cli(
        node,
        &["peers", "status", "--json"],
        Duration::from_secs(120),
    );
    assert!(
        ok,
        "susi peers status as {} failed: {stdout}{stderr}",
        node.name
    );
    let rows: Vec<serde_json::Value> = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("peers status as {} is not JSON ({e}): {stdout}", node.name));
    rows.into_iter()
        .filter_map(|r| Some((r.get("node")?.as_str()?.to_string(), r)))
        .collect()
}

fn allow_all(_: &SocketAddr) -> Result<(), String> {
    Ok(())
}

/// Call `target` as `caller` (this process takes `caller`'s home and key)
/// until it answers, within `limit`; the uplinks come up asynchronously.
fn call_until_up(caller: &Node, target: &Node, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    loop {
        let env = env_for(caller);
        link::close_links();
        let t = Target {
            node_id: target.node_id.clone(),
            pubkey: target.pubkey.clone(),
            direct: Vec::new(),
        };
        let outcome = link::open(&t, Duration::from_secs(10), &allow_all).map(mcp_initialize);
        link::close_links();
        drop(env);
        match outcome {
            Ok(reply) if reply.starts_with("HTTP/1.1 200") => return reply,
            Ok(reply) => panic!(
                "{} -> {}: the channel opened but GMCP answered: {reply}",
                caller.name, target.name
            ),
            Err(e) => {
                assert!(
                    Instant::now() < deadline,
                    "{} -> {} never answered within {limit:?}: {e}",
                    caller.name,
                    target.name
                );
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    }
}

/// Read whatever a socket sends within `limit` (empty when it closes or
/// stays silent).
fn read_some(stream: &mut TcpStream, limit: Duration) -> Vec<u8> {
    stream.set_read_timeout(Some(limit)).unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = stream.read(&mut buf) {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

/// Nothing a call carries may appear in `bytes`.
fn assert_opaque(what: &str, bytes: &[u8], ids: &[&str]) {
    assert!(!bytes.is_empty(), "{what}: nothing was captured");
    let mut needles: Vec<&[u8]> = vec![
        b"POST",
        b"HTTP/1.1",
        b"/mcp",
        b"jsonrpc",
        b"initialize",
        b"mcp-session-id",
        b"content-type",
        b"protocolVersion",
        b"peer-channel-test",
    ];
    needles.extend(ids.iter().map(|id| id.as_bytes()));
    for needle in needles {
        assert!(
            !bytes
                .windows(needle.len())
                .any(|w| w.eq_ignore_ascii_case(needle)),
            "{what} shows {:?} in clear",
            String::from_utf8_lossy(needle)
        );
    }
}

#[test]
fn a_hub_and_two_outbound_only_members_talk_only_through_the_sealed_channel() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!(
        "susi-pc-hub-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let [oh, oa, ob] = free_offsets();
    let hub = make_node(&root, "hub", oh, 0x11, CLUSTER_KEY);
    let a = make_node(&root, "a", oa, 0x22, CLUSTER_KEY);
    let b = make_node(&root, "b", ob, 0x33, CLUSTER_KEY);
    // Identities that are not members: an unbound cluster-key holder, and a
    // host with a different cluster key.
    let stranger = make_node(&root, "c", oa, 0x44, CLUSTER_KEY);
    let outsider = make_node(&root, "d", oa, 0x55, [0x77u8; 32]);

    // The hub's dedicated channel port, and the "router" in front of it.
    let channel_port = free_port();
    let channel = SocketAddr::from(([127, 0, 0, 1], channel_port));
    let (forward, capture) = recording_forwarder(channel);

    // Rosters advertise every member at a TEST-NET address it never
    // connects from: identity, not address, decides.
    write_json(
        &hub.config.join("peers.json"),
        &serde_json::json!([
            roster_row(&a, "203.0.113.10:9093"),
            roster_row(&b, "203.0.113.11:9093")
        ]),
    );
    for (me, other) in [(&a, &b), (&b, &a)] {
        write_json(
            &me.config.join("peers.json"),
            &serde_json::json!([
                roster_row(&hub, "198.51.100.1:29090"),
                roster_row(
                    other,
                    if other.name == "a" {
                        "203.0.113.10:9093"
                    } else {
                        "203.0.113.11:9093"
                    }
                )
            ]),
        );
        // The member knows its hub by key; the address is a hint (here the
        // forwarder, as the router's public address would be).
        write_json(
            &me.config.join(link::HUBS_FILE),
            &serde_json::json!([{
                "node_id": hub.node_id,
                "pubkey": hub.pubkey,
                "address": forward.to_string(),
            }]),
        );
        write_json(
            &me.config.join("config.json"),
            &serde_json::json!({"bind_address": "127.0.0.1"}),
        );
    }
    write_json(
        &hub.config.join("config.json"),
        &serde_json::json!({
            "bind_address": "127.0.0.1",
            "peer_channel_listen": channel.to_string(),
        }),
    );

    let relay_trace = root.join("hub-relay-trace.bin");
    let mut hub_d = start_daemon(&hub, &[("SUSI_CHANNEL_RELAY_TRACE", relay_trace.as_path())]);
    let mut a_d = start_daemon(&a, &[]);
    let mut b_d = start_daemon(&b, &[]);
    wait_published(&hub, &mut hub_d);
    wait_published(&a, &mut a_d);
    wait_published(&b, &mut b_d);

    // Member to hub, member to member (relayed), hub to member (over the
    // member's outbound uplink): each answered by the destination's GMCP
    // surface, authorized by the identity the channel proved, no bearer.
    let up = Duration::from_secs(120);
    let reply = call_until_up(&a, &hub, up);
    assert!(
        reply.to_ascii_lowercase().contains("mcp-session-id"),
        "{reply}"
    );
    let reply = call_until_up(&a, &b, up);
    assert!(
        reply.to_ascii_lowercase().contains("mcp-session-id"),
        "{reply}"
    );
    let reply = call_until_up(&b, &a, up);
    assert!(
        reply.to_ascii_lowercase().contains("mcp-session-id"),
        "{reply}"
    );
    let reply = call_until_up(&hub, &a, up);
    assert!(
        reply.to_ascii_lowercase().contains("mcp-session-id"),
        "{reply}"
    );
    // The product path: `susi peers status` asks every member's daemon for
    // its cluster status through the real peer client, which reaches other
    // machines only inside the channel (a member's CLI via its hub; the
    // hub's CLI via its own daemon and the members' uplinks).
    for (caller, others) in [(&a, [&hub, &b]), (&hub, [&a, &b])] {
        let rows = peers_status(caller);
        for other in others {
            let row = rows.get(&other.node_id).unwrap_or_else(|| {
                panic!("{} lists no row for {}: {rows:?}", caller.name, other.name)
            });
            assert!(
                row.get("error").is_none(),
                "{} could not reach {} over the channel: {row}",
                caller.name,
                other.name
            );
        }
    }
    assert!(
        a_d.log().contains("[CHANNEL] uplink to hub")
            && b_d.log().contains("[CHANNEL] uplink to hub"),
        "both members dialed out to the hub;\nA:\n{}\nB:\n{}",
        a_d.log(),
        b_d.log()
    );

    // The network path (the forwarder) saw only the preamble and ciphertext.
    let ids = [hub.node_id.as_str(), a.node_id.as_str(), b.node_id.as_str()];
    {
        let c = capture.lock().unwrap();
        assert_opaque("member-to-hub traffic", &c.to_hub, &ids);
        assert_opaque("hub-to-member traffic", &c.from_hub, &ids);
    }
    // What the hub itself read of the calls it relayed: end-to-end
    // ciphertext, no request, no response.
    let trace = std::fs::read(&relay_trace).unwrap_or_default();
    assert_opaque("the hub's relay trace", &trace, &ids);

    // Plaintext HTTP on the channel port gets nothing back.
    let mut plain = TcpStream::connect_timeout(&channel, Duration::from_secs(5)).unwrap();
    plain
        .write_all(b"POST /mcp HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: 2\r\n\r\n{}")
        .unwrap();
    assert!(
        read_some(&mut plain, Duration::from_secs(5)).is_empty(),
        "the channel port answered plaintext HTTP"
    );

    // A captured handshake replayed verbatim, inside its freshness window,
    // gets no answer: a fresh call through the "router", then its bytes.
    call_until_up(&a, &hub, up);
    let recorded = capture.lock().unwrap().connections.last().cloned().unwrap();
    assert!(
        recorded.starts_with(link::MAGIC),
        "the capture holds a channel handshake"
    );
    let mut replay = TcpStream::connect_timeout(&channel, Duration::from_secs(5)).unwrap();
    replay.write_all(&recorded).unwrap();
    assert!(
        read_some(&mut replay, Duration::from_secs(5)).is_empty(),
        "the hub answered a replayed handshake"
    );
    assert!(
        hub_d.log().contains("replayed handshake refused"),
        "{}",
        hub_d.log()
    );

    // Dialing the hub while expecting another node's key fails closed.
    {
        let env = env_for(&a);
        let err = link::dial(
            &channel.to_string(),
            Expect::Key(b.pubkey.clone()),
            link::DialMode {
                persistent: false,
                hub: true,
            },
            &allow_all,
        )
        .err()
        .expect("a hub proving another key is refused");
        assert!(err.contains("not the node key this call expects"), "{err}");
        link::close_links();
        drop(env);
    }

    // A node without this cluster's key gets no handshake answer at all.
    {
        let env = env_for(&outsider);
        let err = link::dial(
            &channel.to_string(),
            Expect::Key(hub.pubkey.clone()),
            link::DialMode {
                persistent: false,
                hub: true,
            },
            &allow_all,
        )
        .err()
        .expect("an outsider is refused");
        assert!(err.contains("no handshake answer"), "{err}");
        link::close_links();
        drop(env);
    }

    // An unbound cluster-key holder may call the hub, but the hub relays
    // nothing for it.
    {
        let env = env_for(&stranger);
        write_json(
            &stranger.config.join(link::HUBS_FILE),
            &serde_json::json!([{"node_id": hub.node_id, "pubkey": hub.pubkey, "address": channel.to_string()}]),
        );
        let to_b = Target {
            node_id: b.node_id.clone(),
            pubkey: b.pubkey.clone(),
            direct: Vec::new(),
        };
        let err = link::open(&to_b, Duration::from_secs(10), &allow_all)
            .err()
            .expect("the hub refuses to relay for an unbound identity");
        assert!(err.contains("relay refused"), "{err}");
        link::close_links();
        drop(env);
    }

    // Admission by identity: the unbound cluster-key holder runs `susi
    // peers add` against the hub's public endpoint (the forwarder). The
    // channel handshake proves the hub's key; the endpoint proof runs in the
    // channel; the hub is bound in the new member's roster and kept as its
    // uplink, though the hub never sees the member's own address.
    {
        std::fs::remove_file(stranger.config.join(link::HUBS_FILE)).unwrap();
        let (ok, stdout, stderr) = run_cli(
            &stranger,
            &["peers", "add", &forward.to_string()],
            Duration::from_secs(120),
        );
        assert!(ok, "peers add over the channel failed: {stdout}{stderr}");
        assert!(
            stdout.contains("verified + admitted over the sealed peer channel"),
            "{stdout}{stderr}"
        );
        let roster: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(stranger.config.join("peers.json")).unwrap())
                .unwrap();
        assert!(
            roster
                .iter()
                .any(|r| r["node_id"] == hub.node_id.as_str() && r["pubkey"] == hub.pubkey.as_str()),
            "the hub is bound by key in the new member's roster: {roster:?}"
        );
        let hubs: Vec<serde_json::Value> =
            serde_json::from_slice(&std::fs::read(stranger.config.join(link::HUBS_FILE)).unwrap())
                .unwrap();
        assert!(
            hubs.iter().any(|h| h["pubkey"] == hub.pubkey.as_str()),
            "the hub is kept as the new member's uplink: {hubs:?}"
        );
        eprintln!("peers add over the channel:\n{stdout}{stderr}");
    }

    // Someone claiming member A's node id under another key is refused.
    {
        let impostor = make_node(&root, "e", oa, 0x66, CLUSTER_KEY);
        std::fs::write(impostor.config.join("node_id"), &a.node_id).unwrap();
        let env = env_for(&impostor);
        let link = link::dial(
            &channel.to_string(),
            Expect::Key(hub.pubkey.clone()),
            link::DialMode {
                persistent: false,
                hub: true,
            },
            &allow_all,
        );
        // The handshake completes cryptographically; the hub then refuses
        // the identity and closes, so no stream opens.
        if let Ok(l) = link {
            let deadline = Instant::now() + Duration::from_secs(10);
            while l.is_open() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            assert!(!l.is_open(), "the hub kept a link for an impostor");
        }
        link::close_links();
        drop(env);
        assert!(
            hub_d.log().contains("is bound to a different node key"),
            "{}",
            hub_d.log()
        );
    }

    drop(hub_d);
    drop(a_d);
    drop(b_d);
    let _ = std::fs::remove_dir_all(&root);
}

/// Rows of `node`'s roster, keyed by node id.
fn roster_of(node: &Node) -> std::collections::HashMap<String, serde_json::Value> {
    std::fs::read(node.config.join("peers.json"))
        .ok()
        .and_then(|raw| serde_json::from_slice::<Vec<serde_json::Value>>(&raw).ok())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|r| Some((r.get("node_id")?.as_str()?.to_string(), r)))
        .collect()
}

/// Whether `node`'s roster binds `other` by its key.
fn binds(node: &Node, other: &Node) -> bool {
    roster_of(node)
        .get(&other.node_id)
        .is_some_and(|r| r["pubkey"] == other.pubkey.as_str())
}

/// `susi peers add <endpoint>` as `node`, repeated until the hub reports
/// that it admitted this node (its leader election runs on a cadence).
fn join(node: &Node, endpoint: &str, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    loop {
        let (ok, stdout, stderr) =
            run_cli(node, &["peers", "add", endpoint], Duration::from_secs(120));
        assert!(ok, "peers add as {} failed: {stdout}{stderr}", node.name);
        if stdout.contains("admitted this node") {
            return format!("{stdout}{stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "{} was never admitted by the hub within {limit:?}: {stdout}{stderr}",
            node.name
        );
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// The CEO topology from nothing: a fresh hub with one forwarded channel
/// port, two fresh members behind it with no listener. Each member runs
/// `susi peers add <hub public endpoint>` once; the hub binds each member
/// by key (the second admission needs the first member's endorsement,
/// fetched by the hub over that member's outbound uplink), the members
/// learn each other from the hub's ledger, and a member-to-member status
/// call then travels member -> hub -> member.
#[test]
fn fresh_members_behind_nat_join_a_hub_with_peers_add_alone() {
    let _g = commit_log::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let root = std::env::temp_dir().join(format!(
        "susi-pc-join-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let [oh, oa, ob] = free_offsets();
    let hub = make_node(&root, "jhub", oh, 0x71, CLUSTER_KEY);
    let a = make_node(&root, "ja", oa, 0x72, CLUSTER_KEY);
    let b = make_node(&root, "jb", ob, 0x73, CLUSTER_KEY);
    let channel = SocketAddr::from(([127, 0, 0, 1], free_port()));
    let (forward, capture) = recording_forwarder(channel);
    write_json(
        &hub.config.join("config.json"),
        &serde_json::json!({"bind_address": "127.0.0.1", "peer_channel_listen": channel.to_string()}),
    );
    for m in [&a, &b] {
        write_json(
            &m.config.join("config.json"),
            &serde_json::json!({"bind_address": "127.0.0.1"}),
        );
    }
    let mut hub_d = start_daemon(&hub, &[]);
    let mut a_d = start_daemon(&a, &[]);
    let mut b_d = start_daemon(&b, &[]);
    wait_published(&hub, &mut hub_d);
    wait_published(&a, &mut a_d);
    wait_published(&b, &mut b_d);

    let endpoint = forward.to_string();
    let limit = Duration::from_secs(150);
    let joined_a = join(&a, &endpoint, limit);
    let deadline = Instant::now() + limit;
    while !binds(&hub, &a) {
        assert!(
            Instant::now() < deadline,
            "the hub never bound A's key: {:?}\n{joined_a}\nhub log:\n{}",
            roster_of(&hub),
            hub_d.log()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(binds(&a, &hub), "A binds the hub: {:?}", roster_of(&a));
    let joined_b = join(&b, &endpoint, limit);
    let deadline = Instant::now() + limit;
    while !(binds(&hub, &b) && binds(&a, &b) && binds(&b, &a)) {
        assert!(
            Instant::now() < deadline,
            "membership did not converge: hub {:?}\nA {:?}\nB {:?}\n{joined_b}\nhub log:\n{}",
            roster_of(&hub),
            roster_of(&a),
            roster_of(&b),
            hub_d.log()
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    // Member to member through the hub, on the product path.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let rows = peers_status(&a);
        if rows
            .get(&b.node_id)
            .is_some_and(|r| r.get("error").is_none())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "A never reached B through the hub: {rows:?}"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    let ids = [hub.node_id.as_str(), a.node_id.as_str(), b.node_id.as_str()];
    let c = capture.lock().unwrap();
    assert_opaque("join traffic toward the hub", &c.to_hub, &ids);
    assert_opaque("join traffic from the hub", &c.from_hub, &ids);
    drop(c);
    drop(hub_d);
    drop(a_d);
    drop(b_d);
    let _ = std::fs::remove_dir_all(&root);
}
