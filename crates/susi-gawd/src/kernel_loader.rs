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

    /// Prints the configured GMCP/GEMI/UDP ports (no actual connectivity check).
    pub fn verify_port_endpoints(_workspace: &Path) -> EaiResult<()> {
        println!("  [Bootloader] Verifying core substrate port endpoints...");
        use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, UdpSocket};
        use std::time::Duration;

        let cfg = crate::susi_config::SusiConfig::load_global().unwrap_or_default();
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
                UdpSocket::bind("127.0.0.1:0")
                    .and_then(|s| {
                        s.set_write_timeout(Some(Duration::from_millis(200)))?;
                        s.send_to(b"SUSI_LAN_PING", format!("127.0.0.1:{}", port))?;
                        Ok(())
                    })
                    .is_ok()
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
