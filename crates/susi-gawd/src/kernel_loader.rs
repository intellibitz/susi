// Parses the built-in list of core module manifests at startup and checks
// that the configured GMCP/GEMI/UDP ports are set.

use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubstrateModuleManifest {
    pub module_id: String,
    pub version: String,
    pub entry_point: String,
    pub capabilities: Vec<String>,
    pub memory_footprint_mb: usize,
}

pub struct SubstrateKernelLoader;

impl SubstrateKernelLoader {
    /// Parses the fixed set of core module manifests below into `SubstrateModuleManifest`s.
    pub fn boot_kernel(workspace: &Path) -> EaiResult<Vec<SubstrateModuleManifest>> {
        let is_verbose = std::env::var("SUSI_VERBOSE").is_ok();
        if is_verbose {
            println!("\n[SUSI KERNEL BOOTLOADER] Initializing Substrate Self-Assembly...");
            let _ = std::io::stdout().flush();
            Self::verify_port_endpoints(workspace)?;
            let profile = crate::susi_core::plane_bus::gemi::HardwareProfiler::get_profile();
            println!(
                "  [Bootloader] Hardware Introspection: {} CPUs | {}GB RAM | Acceleration: {}",
                profile.cpus, profile.ram_gb, profile.native_acceleration
            );
            let _ = std::io::stdout().flush();
        }

        let core_manifests = [
            r#"{ "module_id": "gawd-swarm", "version": "0.1.0", "entry_point": "GawdAgentFleet", "capabilities": ["swarm", "agents"], "memory_footprint_mb": 128 }"#,
            r#"{ "module_id": "gmcp-protocol", "version": "0.1.0", "entry_point": "ToolRegistry", "capabilities": ["mcp", "tools", "rpc"], "memory_footprint_mb": 64 }"#,
            r#"{ "module_id": "gemi-inference", "version": "0.1.0", "entry_point": "GemiEngine", "capabilities": ["candle", "llm", "reasoning"], "memory_footprint_mb": 1024 }"#,
            r#"{ "module_id": "truth-transformer", "version": "0.1.0", "entry_point": "TruthTransformer", "capabilities": ["verification", "reality"], "memory_footprint_mb": 32 }"#,
        ];

        let mut loaded_modules = Vec::new();
        for json in core_manifests.iter() {
            let manifest = serde_json::from_str::<SubstrateModuleManifest>(json)
                .map_err(|e| EaiError::config(format!("Invalid module manifest JSON: {}", e)))?;
            if is_verbose {
                println!(
                    "  [Bootloader Assembly] Hot-plugged [{}] (v{})",
                    manifest.module_id, manifest.version
                );
            }
            loaded_modules.push(manifest);
        }

        Ok(loaded_modules)
    }

    /// Prints the configured GMCP/GEMI/UDP ports and probes each endpoint.
    pub fn verify_port_endpoints(_workspace: &Path) -> EaiResult<()> {
        println!("  [Bootloader] Verifying core substrate port endpoints...");
        use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream};
        use std::time::Duration;

        let cfg = crate::susi_config::SusiConfig::load_global().unwrap_or_default();
        let udp = std::net::SocketAddr::from(([127, 0, 0, 1], cfg.udp_discovery_port()));
        let signed_ping =
            crate::susi_config::cluster_key::signed_ping_v3("bootloader", 0, "0", udp);
        let checks: &[(&str, u16, bool)] = &[
            ("GMCP/MCP HTTP", cfg.gmcp_port(), true),
            ("GEMI HTTP", cfg.gemi_port(), true),
            ("A2A UDP discovery", cfg.udp_discovery_port(), false),
            ("GMCP HTTP alias", cfg.gmcp_http_port(), true),
            ("A2A HTTP", cfg.a2a_http_port(), true),
        ];

        for (name, port, tcp) in checks {
            let ok = if *tcp {
                TcpStream::connect_timeout(
                    &SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), *port),
                    Duration::from_millis(400),
                )
                .is_ok()
            } else {
                probe_udp_discovery_port(
                    *port,
                    signed_ping
                        .as_ref()
                        .map(|(frame, nonce)| (frame.as_str(), nonce.as_str())),
                    Duration::from_millis(200),
                )
            };
            println!(
                "    - {} (127.0.0.1:{}) ... {}",
                name,
                port,
                if ok { "OK" } else { "WAITING" }
            );
        }
        let _ = std::io::stdout().flush();
        Ok(())
    }
}

/// Probe the authenticated UDP discovery endpoint (`B7`): send a bound `SUSI_PING_SIG3`
/// frame to `127.0.0.1:{port}`, wait up to `timeout` for a `SUSI_PONG_SIG*` reply whose
/// HMAC and echoed `nonce` verify (`cluster_key::verify_signed_pong`). Fails closed
/// (`false`) with no or an unsigned frame, a dropped ping, an unverified pong, or no listener.
pub fn probe_udp_discovery_port(
    port: u16,
    signed_ping_and_nonce: Option<(&str, &str)>,
    timeout: std::time::Duration,
) -> bool {
    use std::net::UdpSocket;

    let Some((frame, nonce)) = signed_ping_and_nonce else {
        return false;
    };
    let (frame, nonce) = (frame.trim(), nonce.trim());
    if !frame.starts_with("SUSI_PING_SIG") || nonce.is_empty() {
        return false;
    }
    let Ok(socket) = UdpSocket::bind("127.0.0.1:0") else {
        return false;
    };
    if socket.set_write_timeout(Some(timeout)).is_err()
        || socket.set_read_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    if socket
        .send_to(frame.as_bytes(), format!("127.0.0.1:{port}"))
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 4096];
    let Ok((amt, _src)) = socket.recv_from(&mut buf) else {
        return false;
    };
    let Ok(reply) = std::str::from_utf8(&buf[..amt]) else {
        return false;
    };
    crate::susi_config::cluster_key::verify_signed_pong(reply.trim(), nonce).is_some()
}

#[cfg(test)]
mod tests {
    use super::probe_udp_discovery_port;
    use crate::susi_config::cluster_key::{signed_ping_v3, signed_pong, verify_bound_ping};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
    use std::net::UdpSocket;
    use std::time::Duration;

    #[test]
    fn probe_udp_discovery_port_succeeds_on_signed_pong() {
        // The cluster key is read from the environment-resolved config dir
        // when the ping is signed and again when the responder verifies it;
        // the HOME-swapping tests in this binary hold the env lock while
        // they swap, so this one holds it across both reads.
        let _env = env_lock();
        let (frame, nonce) = signed_ping_v3("bootloader", 0, "0", ([127, 0, 0, 1], 9).into())
            .expect("cluster.key must produce signed ping");
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral UDP responder");
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let port = server.local_addr().expect("local_addr").port();

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            if let Ok((amt, src)) = server.recv_from(&mut buf) {
                let msg = String::from_utf8_lossy(&buf[..amt]);
                if let Ok(ping) = verify_bound_ping(&msg, src) {
                    if let Some(pong) = signed_pong("test-daemon", 0, "0", &ping.nonce, &[]) {
                        let _ = server.send_to(pong.as_bytes(), src);
                    }
                }
            }
        });

        assert!(probe_udp_discovery_port(
            port,
            Some((&frame, &nonce)),
            Duration::from_millis(500),
        ));
        let _ = handle.join();
    }

    #[test]
    fn probe_udp_discovery_port_rejects_unsigned_mismatched_nonce_and_closed_port() {
        // The cluster key is read from the environment-resolved config dir
        // when the ping is signed and again when the responder verifies it;
        // the HOME-swapping tests in this binary hold the env lock while
        // they swap, so this one holds it across both reads.
        let _env = env_lock();
        let (frame, nonce) = signed_ping_v3("bootloader", 0, "0", ([127, 0, 0, 1], 9).into())
            .expect("cluster.key must produce signed ping");
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral UDP responder");
        server
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("set read timeout");
        let port = server.local_addr().expect("local_addr").port();

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            if let Ok((_amt, src)) = server.recv_from(&mut buf) {
                // Responder replies with a pong bound to a different nonce.
                if let Some(wrong_pong) =
                    signed_pong("test-daemon", 0, "0", "deadbeef00112233", &[])
                {
                    let _ = server.send_to(wrong_pong.as_bytes(), src);
                }
            }
        });

        assert!(!probe_udp_discovery_port(
            port,
            None,
            Duration::from_millis(100),
        ));
        assert!(!probe_udp_discovery_port(
            port,
            Some(("SUSI_PING:plaintext-secret", &nonce)),
            Duration::from_millis(100),
        ));
        assert!(!probe_udp_discovery_port(
            port,
            Some((&frame, "")),
            Duration::from_millis(100),
        ));
        assert!(!probe_udp_discovery_port(
            port,
            Some((&frame, &nonce)),
            Duration::from_millis(200),
        ));
        let _ = handle.join();

        // Port is now closed after `server` dropped in the responder thread.
        assert!(!probe_udp_discovery_port(
            port,
            Some((&frame, &nonce)),
            Duration::from_millis(100),
        ));
    }

    #[test]
    fn boot_kernel_loads_all_four_core_substrate_manifests() {
        let modules = super::SubstrateKernelLoader::boot_kernel(std::path::Path::new("."))
            .expect("boot_kernel must parse built-in core manifests");
        assert_eq!(modules.len(), 4);
        let ids: Vec<&str> = modules.iter().map(|m| m.module_id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "gawd-swarm",
                "gmcp-protocol",
                "gemi-inference",
                "truth-transformer"
            ]
        );
        assert!(modules
            .iter()
            .all(|m| !m.capabilities.is_empty() && m.memory_footprint_mb > 0));
    }

    #[test]
    fn substrate_module_manifest_roundtrips_json_serialization() {
        let manifest = super::SubstrateModuleManifest {
            module_id: "test-mod".to_string(),
            version: "0.2.0".to_string(),
            entry_point: "TestEntry".to_string(),
            capabilities: vec!["cap-a".to_string(), "cap-b".to_string()],
            memory_footprint_mb: 256,
        };
        let json = serde_json::to_string(&manifest).expect("serialize manifest");
        let decoded: super::SubstrateModuleManifest =
            serde_json::from_str(&json).expect("deserialize manifest");
        assert_eq!(decoded.module_id, "test-mod");
        assert_eq!(decoded.version, "0.2.0");
        assert_eq!(decoded.entry_point, "TestEntry");
        assert_eq!(decoded.capabilities, vec!["cap-a", "cap-b"]);
        assert_eq!(decoded.memory_footprint_mb, 256);
    }

    #[test]
    fn probe_udp_discovery_port_rejects_non_pong_reply_and_whitespace_nonce() {
        // The cluster key is read from the environment-resolved config dir
        // when the ping is signed and again when the responder verifies it;
        // the HOME-swapping tests in this binary hold the env lock while
        // they swap, so this one holds it across both reads.
        let _env = env_lock();
        let (frame, nonce) = signed_ping_v3("bootloader", 0, "0", ([127, 0, 0, 1], 9).into())
            .expect("cluster.key must produce signed ping");
        assert!(!probe_udp_discovery_port(
            9,
            Some((&frame, "   ")),
            Duration::from_millis(50),
        ));

        let server = UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral UDP responder");
        server
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("set read timeout");
        let port = server.local_addr().expect("local_addr").port();

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            if let Ok((_amt, src)) = server.recv_from(&mut buf) {
                let _ = server.send_to(b"NOT_A_SIGNED_PONG", src);
            }
        });

        assert!(!probe_udp_discovery_port(
            port,
            Some((&frame, &nonce)),
            Duration::from_millis(200),
        ));
        let _ = handle.join();
    }
}
