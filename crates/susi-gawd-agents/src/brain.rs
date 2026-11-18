// Combines compiled-in version/rule counts, detected hardware, and loaded
// config into one status snapshot.

use crate::self_core::AlphaSelf;
use crate::susi_core::plane_bus::gemi::HardwareProfiler;
use crate::susi_sandbox::manager::SusiConfig;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AlphaBrainContext {
    pub self_version: &'static str,
    pub system_cpus: usize,
    pub system_gpu: String,
    pub system_ram_gb: usize,
    pub workspace_path: PathBuf,
    pub default_engine: String,
    pub default_model: String,
    pub gmcp_port: u16,
    pub gemi_port: u16,
}

impl AlphaBrainContext {
    pub fn initialize(workspace: &Path) -> Self {
        let global_dir = susi_paths::SusiDirs::config_dir();
        Self::initialize_with_config_dir(workspace, &global_dir)
    }

    pub fn initialize_with_config_dir(workspace: &Path, config_dir: &Path) -> Self {
        let hardware = HardwareProfiler::get_profile();
        // Reachable on every `identity` query during normal operation, not
        // just boot: degrade to bundled defaults rather than panic this
        // request's thread if config.json is torn by a concurrent writer.
        let cfg = SusiConfig::load(config_dir).unwrap_or_default();

        Self {
            self_version: AlphaSelf::VERSION,
            system_cpus: hardware.cpus,
            system_gpu: hardware.gpu_info,
            system_ram_gb: hardware.ram_gb,
            workspace_path: workspace.to_path_buf(),
            default_engine: cfg.default_engine(),
            default_model: cfg.default_model(),
            gmcp_port: cfg.gmcp_port(),
            gemi_port: cfg.gemi_port(),
        }
    }

    pub fn inspect_tri_state(&self) -> String {
        format!(
            "SUSI Core Substrate Status:\n\
             1. [CORE - Compiled System]: Version {}, {}, {} Baked Components\n\
             2. [HARDWARE - System Environment]: {} CPUs | {} | {}GB RAM\n\
             3. [DYNAMIC - Runtime Configuration]: Workspace: {} | Engine: {} | Model: {} | GMCP Port: {} | GEMI Port: {}",
            self.self_version,
            AlphaSelf::genome_summary(),
            AlphaSelf::COMPONENTS.len(),
            self.system_cpus,
            self.system_gpu,
            self.system_ram_gb,
            self.workspace_path.display(),
            self.default_engine,
            self.default_model,
            self.gmcp_port,
            self.gemi_port
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_with_config_dir_degrades_on_torn_json_and_renders_tri_state() {
        let dir = std::env::temp_dir().join(format!(
            "susi-brain-ctx-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), b"{torn-json").unwrap();

        let ws = dir.join("workspace");
        let ctx = AlphaBrainContext::initialize_with_config_dir(&ws, &dir);
        assert_eq!(ctx.self_version, AlphaSelf::VERSION);
        assert_eq!(ctx.workspace_path, ws);
        assert!(!ctx.default_engine.is_empty());
        assert!(!ctx.default_model.is_empty());

        let rendered = ctx.inspect_tri_state();
        assert!(rendered.contains("1. [CORE - Compiled System]"));
        assert!(rendered.contains(&AlphaSelf::genome_summary()));
        assert!(rendered.contains("2. [HARDWARE - System Environment]"));
        assert!(rendered.contains("3. [DYNAMIC - Runtime Configuration]"));
        assert!(rendered.contains(&ws.display().to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn initialize_with_config_dir_loads_custom_engine_and_model_and_formats_all_sections() {
        let dir = std::env::temp_dir().join(format!(
            "susi-brain-custom-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            r#"{"default_engine":"custom-tensor-engine","default_model":"qwen-custom-7b"}"#,
        )
        .unwrap();

        let ws = dir.join("project-ws");
        let ctx = AlphaBrainContext::initialize_with_config_dir(&ws, &dir);
        assert_eq!(ctx.default_engine, "custom-tensor-engine");
        assert_eq!(ctx.default_model, "qwen-custom-7b");
        assert_eq!(
            ctx.gmcp_port,
            susi_paths::ports::effective(susi_paths::ports::GMCP)
        );
        assert_eq!(
            ctx.gemi_port,
            susi_paths::ports::effective(susi_paths::ports::GEMI)
        );

        let tri = ctx.inspect_tri_state();
        assert!(tri.contains("1. [CORE - Compiled System]"));
        assert!(tri.contains("2. [HARDWARE - System Environment]"));
        assert!(tri.contains("3. [DYNAMIC - Runtime Configuration]"));
        assert!(tri.contains("Engine: custom-tensor-engine"));
        assert!(tri.contains("Model: qwen-custom-7b"));
        assert!(tri.contains(&format!("GMCP Port: {}", ctx.gmcp_port)));
        assert!(tri.contains(&format!("GEMI Port: {}", ctx.gemi_port)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn initialize_with_config_dir_on_empty_directory_uses_default_config_values() {
        let dir = std::env::temp_dir().join(format!(
            "susi-brain-empty-cfg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let ws = dir.join("empty-ws");
        let ctx = AlphaBrainContext::initialize_with_config_dir(&ws, &dir);
        let default_cfg = SusiConfig::default();
        assert_eq!(ctx.self_version, AlphaSelf::VERSION);
        assert_eq!(ctx.workspace_path, ws);
        assert_eq!(ctx.default_engine, default_cfg.default_engine());
        assert_eq!(ctx.default_model, default_cfg.default_model());
        assert_eq!(ctx.gmcp_port, default_cfg.gmcp_port());
        assert_eq!(ctx.gemi_port, default_cfg.gemi_port());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn alpha_brain_context_clone_debug_and_custom_hardware_port_reflection_in_tri_state() {
        let ctx = AlphaBrainContext {
            self_version: AlphaSelf::VERSION,
            system_cpus: 64,
            system_gpu: "NVIDIA H100 80GB".to_string(),
            system_ram_gb: 256,
            workspace_path: PathBuf::from("/custom/workspace"),
            default_engine: "gemi-native".to_string(),
            default_model: "gemi-ultra".to_string(),
            gmcp_port: 19090,
            gemi_port: 19091,
        };
        let cloned = ctx.clone();
        let dbg = format!("{cloned:?}");
        assert!(dbg.contains("AlphaBrainContext"));
        assert!(dbg.contains("NVIDIA H100 80GB"));

        let tri = cloned.inspect_tri_state();
        assert!(tri.contains("64 CPUs | NVIDIA H100 80GB | 256GB RAM"));
        assert!(tri.contains("GMCP Port: 19090 | GEMI Port: 19091"));
    }

    #[test]
    fn inspect_tri_state_starts_with_header_and_lists_all_three_numbered_tiers() {
        let ctx = AlphaBrainContext {
            self_version: AlphaSelf::VERSION,
            system_cpus: 8,
            system_gpu: "CPU-Only".to_string(),
            system_ram_gb: 32,
            workspace_path: PathBuf::from("."),
            default_engine: "candle".to_string(),
            default_model: "qwen2.5".to_string(),
            gmcp_port: 9090,
            gemi_port: 9091,
        };
        let report = ctx.inspect_tri_state();
        assert!(report.starts_with("SUSI Core Substrate Status:\n"));
        assert_eq!(report.lines().count(), 4);
    }
}
