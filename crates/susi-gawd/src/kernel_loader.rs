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
        let host_token = susi_paths::host_token();
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
                probe_udp_discovery_port(*port, host_token.as_deref(), Duration::from_millis(200))
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

/// Probe the authenticated UDP discovery endpoint (`B7`).
///
/// Sends `SUSI_PING:<api_token>` to `127.0.0.1:{port}` and waits up to
/// `timeout` for an authenticated `SUSI_ACK:<version>` reply. Fails closed
/// (`false`) when no non-empty host token is available, when the responder
/// drops an unauthenticated/mismatched ping, or when nothing is listening.
pub fn probe_udp_discovery_port(
    port: u16,
    token: Option<&str>,
    timeout: std::time::Duration,
) -> bool {
    use std::net::UdpSocket;

    let Some(tok) = token.map(str::trim).filter(|t| !t.is_empty()) else {
        return false;
    };
    let Ok(socket) = UdpSocket::bind("127.0.0.1:0") else {
        return false;
    };
    if socket.set_write_timeout(Some(timeout)).is_err()
        || socket.set_read_timeout(Some(timeout)).is_err()
    {
        return false;
    }
    let frame = format!("SUSI_PING:{tok}");
    if socket
        .send_to(frame.as_bytes(), format!("127.0.0.1:{port}"))
        .is_err()
    {
        return false;
    }
    let mut buf = [0u8; 256];
    let Ok((amt, _src)) = socket.recv_from(&mut buf) else {
        return false;
    };
    buf[..amt].starts_with(b"SUSI_ACK:")
}

#[cfg(test)]
mod tests {
    use super::probe_udp_discovery_port;
    use std::net::UdpSocket;
    use std::time::Duration;

    #[test]
    fn probe_udp_discovery_port_succeeds_on_authenticated_ack() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral UDP responder");
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        let port = server.local_addr().expect("local_addr").port();

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            if let Ok((amt, src)) = server.recv_from(&mut buf) {
                if &buf[..amt] == b"SUSI_PING:test-token-b7" {
                    let _ = server.send_to(b"SUSI_ACK:0.21.47", src);
                }
            }
        });

        assert!(probe_udp_discovery_port(
            port,
            Some("  test-token-b7\n"),
            Duration::from_millis(500),
        ));
        let _ = handle.join();
    }

    #[test]
    fn probe_udp_discovery_port_rejects_missing_wrong_token_and_closed_port() {
        let server = UdpSocket::bind("127.0.0.1:0").expect("bind ephemeral UDP responder");
        server
            .set_read_timeout(Some(Duration::from_millis(300)))
            .expect("set read timeout");
        let port = server.local_addr().expect("local_addr").port();

        let handle = std::thread::spawn(move || {
            let mut buf = [0u8; 256];
            if let Ok((amt, src)) = server.recv_from(&mut buf) {
                if &buf[..amt] == b"SUSI_PING:expected-secret" {
                    let _ = server.send_to(b"SUSI_ACK:0.21.47", src);
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
            Some("   "),
            Duration::from_millis(100),
        ));
        assert!(!probe_udp_discovery_port(
            port,
            Some("wrong-token"),
            Duration::from_millis(150),
        ));
        let _ = handle.join();

        // Port is now closed after `server` dropped in the responder thread.
        assert!(!probe_udp_discovery_port(
            port,
            Some("expected-secret"),
            Duration::from_millis(100),
        ));
    }
}
