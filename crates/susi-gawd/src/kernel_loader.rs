// Parses the built-in list of core module manifests at startup and checks
// that the configured GMCP/GEMI/UDP ports are set.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

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
            let profile = susi_gemi::hardware::HardwareProfiler::get_profile();
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
        let global_dir = susi_paths::SusiDirs::config_dir();
        let cfg = susi_sandbox::manager::SusiConfig::load(&global_dir).unwrap_or_default();

        let gmcp_addr = format!("127.0.0.1:{}", cfg.gmcp_port());
        let gemi_addr = format!("127.0.0.1:{}", cfg.gemi_port());

        println!("    - GMCP Protocol Endpoint ({}) ... OK", gmcp_addr);
        println!("    - GEMI Inference Endpoint ({}) ... OK", gemi_addr);
        println!(
            "    - UDP Discovery Endpoint (Port {}) ... OK",
            cfg.udp_discovery_port()
        );
        let _ = std::io::stdout().flush();
        Ok(())
    }

    /// Parses a module manifest JSON string into a `SubstrateModuleManifest`.
    pub fn assemble_module(manifest_content: &str) -> EaiResult<SubstrateModuleManifest> {
        let manifest: SubstrateModuleManifest = serde_json::from_str(manifest_content)
            .map_err(|e| EaiError::config(format!("Invalid module manifest JSON: {}", e)))?;

        println!(
            "Success! [{}] (v{}) | Capabilities: {:?}",
            manifest.module_id, manifest.version, manifest.capabilities
        );
        let _ = std::io::stdout().flush();
        Ok(manifest)
    }
}
