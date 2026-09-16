// susi Sandbox Manager: Neural Checkpoints, Memory & State Isolation
// 100% Rust implementation for sandboxed execution environment

use crate::error::{EaiError, EaiResult};
use crate::gmcp::GlobalMcpEntry;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelTier {
    Reflex,
    Specialist,
    Premier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProviderType {
    NativeCandle,
    LocalVault,
    LocalGGUF,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub name: String,
    pub registry: String,
    pub model_id: String,
    pub description: String,
    pub is_local: bool,
    pub tier: ModelTier,
    pub latency_ms: Option<u128>,
    pub provider: ProviderType,
    pub checksum: Option<String>,
    pub provenance: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeuralCheckpoint {
    pub intent: String,
    pub timestamp: u64,
    pub completed_tools: Vec<String>,
    pub blackboard: std::collections::HashMap<String, String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLadderConfigStep {
    pub step: usize,
    pub min_ram_gb: usize,
    pub label: String,
    pub hf_repo: String,
    pub hf_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AdminPulsesConfig {
    #[serde(alias = "install_mission")]
    pub install_pulse: String,
    #[serde(alias = "uninstall_mission")]
    pub uninstall_pulse: String,
    #[serde(alias = "select_model_mission")]
    pub select_model_pulse: String,
    #[serde(alias = "deep_scan_mission")]
    pub deep_scan_pulse: String,
    #[serde(alias = "mcp_scout_mission")]
    pub mcp_scout_pulse: String,
    #[serde(alias = "audit_mission")]
    pub audit_pulse: String,
    #[serde(alias = "verify_mission")]
    pub verify_pulse: String,
    #[serde(alias = "release_mission")]
    pub release_pulse: String,
    #[serde(alias = "lint_mission")]
    pub lint_pulse: String,
    #[serde(alias = "audit_deps_mission")]
    pub audit_deps_pulse: String,
}

impl Default for AdminPulsesConfig {
    fn default() -> Self {
        Self {
            install_pulse: "admin pulse: initialize sandboxed .susi environment and provision weights".to_string(),
            uninstall_pulse: "admin pulse: remove and clean up sandboxed .susi environment".to_string(),
            select_model_pulse: "admin pulse: select and override active model substrate to {}".to_string(),
            deep_scan_pulse: "admin pulse: perform parallel deep-scan of substrate home for local models and register them".to_string(),
            mcp_scout_pulse: "admin pulse: perform autonomous web-scouting of open-source MCP servers and benchmark them".to_string(),
            audit_pulse: "admin pulse: perform compliance audit and technical verification".to_string(),
            verify_pulse: "admin pulse: verify version alignment across manifest and documents".to_string(),
            release_pulse: "admin pulse: execute full release orchestration sequence".to_string(),
            lint_pulse: "admin pulse: run linting and static analysis (clippy)".to_string(),
            audit_deps_pulse: "admin pulse: run dependency security audit".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InferenceEndpointsConfig {
    pub vllm_api_base: String,
    pub sglang_api_base: String,
    pub llama_api_base: String,
    pub triton_api_base: String,
    pub lmdeploy_api_base: String,
}

impl Default for InferenceEndpointsConfig {
    fn default() -> Self {
        Self {
            vllm_api_base: "http://localhost:8000/v1".to_string(),
            sglang_api_base: "http://localhost:30000/v1".to_string(),
            llama_api_base: "http://localhost:8080/v1".to_string(),
            triton_api_base: "http://localhost:8001/v2/models/susi_model/generate".to_string(),
            lmdeploy_api_base: "http://localhost:23333/v1".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoverableAssetConfig {
    pub tier: String,
    pub name: String,
    pub provider: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GovernancePatterns {
    pub destructive_commands: Vec<String>,
    pub critical_system_paths: Vec<String>,
    pub secret_tokens: Vec<String>,
    pub exfiltration_vectors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SusiConfig {
    pub gmcp_port: u16,
    pub gmcp_http_port: u16,
    pub gemi_port: u16,
    pub udp_discovery_port: u16,
    pub execution_lease_secs: u64,
    pub max_concurrent_agents: usize,
    pub reflex_training_threshold: usize,
    pub max_stdin_size_bytes: usize,
    pub stdin_timeout_secs: u64,
    pub default_engine: String,
    pub default_model: String,
    pub auto_download_models: bool,
    pub susi_repo: String,
    pub mcp_registry_url: String,
    pub bootstrap_mcp_servers: Vec<GlobalMcpEntry>,
    pub cloud_scout_timeout_secs: u64,
    pub beacon_interval_secs: u64,
    pub local_scan_paths: Vec<String>,
    pub agent_rank_threshold: f32,
    pub alpha_weights_url: String,
    pub model_ladder: Vec<ModelLadderConfigStep>,
    #[serde(alias = "admin_templates")]
    pub admin_pulses: AdminPulsesConfig,
    pub inference_endpoints: InferenceEndpointsConfig,
    pub discoverable_assets: Vec<DiscoverableAssetConfig>,
    pub governance: GovernancePatterns,
}

impl Default for SusiConfig {
    fn default() -> Self {
        SusiConfig {
            gmcp_port: 9090,
            gmcp_http_port: 9093,
            gemi_port: 9091,
            udp_discovery_port: 9092,
            execution_lease_secs: 30,
            max_concurrent_agents: 32,
            reflex_training_threshold: 50,
            max_stdin_size_bytes: 100 * 1024 * 1024,
            stdin_timeout_secs: 120,
            default_engine: "susi-offline".to_string(),
            default_model: "susi-alpha".to_string(),
            auto_download_models: true,
            susi_repo: "intellibitz/susi".to_string(),
            mcp_registry_url:
                "https://raw.githubusercontent.com/intellibitz/susi/main/registry.json".to_string(),
            bootstrap_mcp_servers: vec![
                GlobalMcpEntry {
                    name: "database".to_string(),
                    description: "Standard Protocol SQL Database Server".to_string(),
                    package: "mcp-server-postgres".to_string(),
                    category: "database".to_string(),
                    trust_score: Some(0.95),
                    latency_ms: Some(10),
                },
                GlobalMcpEntry {
                    name: "search".to_string(),
                    description: "Standard Protocol Web Search Server".to_string(),
                    package: "mcp-server-search".to_string(),
                    category: "search".to_string(),
                    trust_score: Some(0.90),
                    latency_ms: Some(50),
                },
                GlobalMcpEntry {
                    name: "vcs".to_string(),
                    description: "Standard Protocol Version Control Server".to_string(),
                    package: "mcp-server-github".to_string(),
                    category: "vcs".to_string(),
                    trust_score: Some(0.92),
                    latency_ms: Some(30),
                },
            ],
            cloud_scout_timeout_secs: 8,
            beacon_interval_secs: 30,
            local_scan_paths: Vec::new(),
            agent_rank_threshold: 0.6,
            alpha_weights_url:
                "https://huggingface.co/intellibitz/susi-alpha/resolve/main/susi-alpha.safetensors"
                    .to_string(),
            model_ladder: vec![
                ModelLadderConfigStep {
                    step: 1,
                    min_ram_gb: 0,
                    label: "1.5B Parameters (Fast Local Edge)".to_string(),
                    hf_repo: "Qwen/Qwen2.5-1.5B-Instruct-GGUF".to_string(),
                    hf_file: "qwen2.5-1.5b-instruct-q4_k_m.gguf".to_string(),
                },
                ModelLadderConfigStep {
                    step: 2,
                    min_ram_gb: 8,
                    label: "7B Parameters (Mid-Range Desktop)".to_string(),
                    hf_repo: "bartowski/Qwen2.5-7B-Instruct-GGUF".to_string(),
                    hf_file: "Qwen2.5-7B-Instruct-Q4_K_M.gguf".to_string(),
                },
                ModelLadderConfigStep {
                    step: 3,
                    min_ram_gb: 16,
                    label: "14B Parameters (High-Accuracy Workstation)".to_string(),
                    hf_repo: "bartowski/Qwen2.5-14B-Instruct-GGUF".to_string(),
                    hf_file: "Qwen2.5-14B-Instruct-Q4_K_M.gguf".to_string(),
                },
                ModelLadderConfigStep {
                    step: 4,
                    min_ram_gb: 32,
                    label: "32B Parameters (High-End Workstation)".to_string(),
                    hf_repo: "bartowski/Qwen2.5-32B-Instruct-GGUF".to_string(),
                    hf_file: "Qwen2.5-32B-Instruct-Q4_K_M.gguf".to_string(),
                },
                ModelLadderConfigStep {
                    step: 5,
                    min_ram_gb: 64,
                    label: "72B Parameters (Ultra-Capacity Workstation)".to_string(),
                    hf_repo: "bartowski/Qwen2.5-72B-Instruct-GGUF".to_string(),
                    hf_file: "Qwen2.5-72B-Instruct-Q4_K_M.gguf".to_string(),
                },
            ],
            admin_pulses: AdminPulsesConfig::default(),
            inference_endpoints: InferenceEndpointsConfig::default(),
            discoverable_assets: vec![
                DiscoverableAssetConfig {
                    tier: "Tier 0: SUSI-Alpha (Reflex)".to_string(),
                    name: "SusiReflexCloud-v2".to_string(),
                    provider: "SUSI Hub".to_string(),
                    url: "https://susi.ai/reflex/v2".to_string(),
                },
                DiscoverableAssetConfig {
                    tier: "Tier 0: SUSI-Alpha (Reflex)".to_string(),
                    name: "DistilledRouter-1B".to_string(),
                    provider: "HuggingFace".to_string(),
                    url: "https://huggingface.co/susi/distilled-router".to_string(),
                },
            ],
            governance: GovernancePatterns {
                destructive_commands: vec![
                    "rm -rf /".to_string(),
                    "rm -rf $HOME".to_string(),
                    "rm -rf ~".to_string(),
                    "mkfs".to_string(),
                    "dd if=".to_string(),
                    "> /dev/sda".to_string(),
                    ":(){ :|:& };:".to_string(),
                    "chmod -R 777 /".to_string(),
                    "chown -R".to_string(),
                    "shred".to_string(),
                ],
                critical_system_paths: vec![
                    "/etc/passwd".to_string(),
                    "/etc/shadow".to_string(),
                    "/boot".to_string(),
                    "/proc".to_string(),
                    "/sys".to_string(),
                    "/dev".to_string(),
                ],
                secret_tokens: vec![
                    "sk-".to_string(),
                    "ghp_".to_string(),
                    "AIza".to_string(),
                    "xoxb-".to_string(),
                    "AWS_ACCESS_KEY_ID".to_string(),
                    "AWS_SECRET_ACCESS_KEY".to_string(),
                    "-----BEGIN RSA PRIVATE KEY-----".to_string(),
                ],
                exfiltration_vectors: vec![
                    "curl -x post".to_string(),
                    "wget --post-data".to_string(),
                    "netcat".to_string(),
                    "nc -e".to_string(),
                    "/dev/tcp/".to_string(),
                    "base64 | curl".to_string(),
                ],
            },
        }
    }
}

pub struct SandboxManager;

impl SusiConfig {
    pub fn get_config_path(global_dir: &Path) -> PathBuf {
        global_dir.join("config.json")
    }

    pub fn load(global_dir: &Path) -> EaiResult<Self> {
        let path = Self::get_config_path(global_dir);
        if path.is_file() {
            let content = fs::read_to_string(&path)
                .map_err(|e| EaiError::config(format!("Failed to read config: {}", e)))?;
            return serde_json::from_str(&content)
                .map_err(|e| EaiError::config(format!("Malformed configuration: {}", e)));
        }
        Ok(Self::default())
    }

    pub fn reload(global_dir: &Path) -> EaiResult<Self> {
        let loaded = Self::load(global_dir)?;
        let _ = loaded.save(global_dir);
        Ok(loaded)
    }

    pub fn load_global() -> EaiResult<Self> {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let global_dir = home.join(".susi");
        Self::load(&global_dir)
    }

    pub fn save(&self, global_dir: &Path) -> EaiResult<()> {
        let path = Self::get_config_path(global_dir);
        let json =
            serde_json::to_string_pretty(self).map_err(|e| EaiError::config(e.to_string()))?;
        fs::write(path, json).map_err(|e| EaiError::filesystem(e.to_string()))
    }
}

impl SandboxManager {
    pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
        use bollard::container::{
            Config, CreateContainerOptions, LogOutput, StartContainerOptions,
        };
        use bollard::Docker;
        use futures::stream::StreamExt;

        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| EaiError::process(format!("Docker connection failed: {}", e)))?;

        let config = Config {
            image: Some("alpine:latest"),
            cmd: Some(vec!["sh", "-c", cmd]),
            ..Default::default()
        };

        let container = docker
            .create_container(None::<CreateContainerOptions<String>>, config)
            .await
            .map_err(|e| EaiError::process(format!("Container creation failed: {}", e)))?;

        docker
            .start_container(&container.id, None::<StartContainerOptions<String>>)
            .await
            .map_err(|e| EaiError::process(format!("Container start failed: {}", e)))?;

        let mut logs = docker.logs::<String>(&container.id, None);
        let mut output = String::new();
        while let Some(log) = logs.next().await {
            if let Ok(LogOutput::StdOut { message }) = log {
                output.push_str(&String::from_utf8_lossy(&message));
            }
        }

        Ok(output)
    }

    pub fn ensure_global_sandbox(global_dir: &Path) -> EaiResult<()> {
        if !global_dir.exists() {
            fs::create_dir_all(global_dir).map_err(|e| EaiError::filesystem(e.to_string()))?;
        }
        let config_path = SusiConfig::get_config_path(global_dir);
        if !config_path.exists() {
            let default_cfg_file = Path::new("config.default.json");
            let default_cfg = if default_cfg_file.is_file() {
                fs::read_to_string(default_cfg_file)
                    .ok()
                    .and_then(|c| serde_json::from_str::<SusiConfig>(&c).ok())
                    .unwrap_or_default()
            } else {
                SusiConfig::default()
            };
            let json = serde_json::to_string_pretty(&default_cfg).unwrap();
            fs::write(config_path, json).map_err(|e| EaiError::filesystem(e.to_string()))?;
        }
        Ok(())
    }

    pub fn save_mission_checkpoint(workspace: &Path, checkpoint: &NeuralCheckpoint) {
        let susi_dir = workspace.join(".susi");
        if !susi_dir.exists() {
            let _ = fs::create_dir_all(&susi_dir);
        }
        let checkpoint_file = workspace.join(".susi/mission_checkpoint.json");
        let _ = fs::write(
            checkpoint_file,
            serde_json::to_string_pretty(checkpoint).unwrap_or_default(),
        );
    }

    pub fn check_interrupted_checkpoint(workspace: &Path) -> Option<NeuralCheckpoint> {
        let checkpoint_file = workspace.join(".susi/mission_checkpoint.json");
        if checkpoint_file.is_file() {
            if let Ok(content) = fs::read_to_string(checkpoint_file) {
                return serde_json::from_str(&content).ok();
            }
        }
        None
    }
}

pub struct SusiMemory;

impl SusiMemory {
    pub fn save_interaction(workspace: &Path, intent: &str, outcome: &str) {
        let susi_dir = workspace.join(".susi");
        if !susi_dir.exists() {
            let _ = fs::create_dir_all(&susi_dir);
        }
        let memory_file = workspace.join(".susi/memory.jsonl");

        // Structured Memory Validation
        if intent.trim().is_empty() || outcome.trim().is_empty() {
            return;
        }

        let entry = serde_json::json!({
            "intent": intent,
            "outcome": outcome,
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "provenance": {
                "workspace": workspace.display().to_string(),
                "engine_version": crate::SUSI_VERSION,
            }
        });
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(memory_file)
        {
            use std::io::Write;
            let _ = writeln!(f, "{}", entry);
        }

        // Substrate Ingestion Motion: Stage successful reasoning for distillation
        if outcome.len() > 50 && !outcome.contains("[FAIL]") && !outcome.contains("error") {
            let exp_file = workspace.join(".susi/reasoning_experience.jsonl");
            let exp_entry = serde_json::json!({
                "intent": intent,
                "blackboard_context": "converged",
                "successful_outcome": outcome,
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                "validation": "STRICT_SEMANTIC_PASS"
            });
            if let Ok(mut f) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(exp_file)
            {
                use std::io::Write;
                let _ = writeln!(f, "{}", exp_entry);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
    Axiomatic,
    Debug,
    Trace,
}

pub struct SusiAuditLogger;

impl SusiAuditLogger {
    pub fn log_event(workspace: &Path, event_type: &str, details: &str) {
        Self::log(workspace, LogLevel::Info, event_type, details);
    }

    pub fn log(workspace: &Path, level: LogLevel, event_type: &str, details: &str) {
        let susi_dir = workspace.join(".susi");
        if !susi_dir.exists() {
            let _ = fs::create_dir_all(&susi_dir);
        }
        let audit_file = workspace.join(".susi/audit.log");
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let log_entry = serde_json::json!({
            "ts": ts,
            "level": format!("{:?}", level),
            "type": event_type,
            "details": details,
            "pid": std::process::id(),
        });

        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(audit_file)
        {
            use std::io::Write;
            let _ = writeln!(f, "{}", log_entry);
        }
    }

    pub fn read_audit_log(workspace: &Path, limit: usize) -> String {
        let audit_file = workspace.join(".susi/audit.log");
        if let Ok(content) = fs::read_to_string(audit_file) {
            let lines: Vec<&str> = content.lines().collect();
            let start = if lines.len() > limit {
                lines.len() - limit
            } else {
                0
            };
            return lines[start..].join("\n");
        }
        String::new()
    }
}

pub struct SusiBackupManager;

impl SusiBackupManager {
    pub fn backup_work(workspace: &Path) -> EaiResult<String> {
        let backups_dir = workspace.join(".susi/backups");
        let _ = fs::create_dir_all(&backups_dir);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup_path = backups_dir.join(format!("backup_{}", ts));

        Self::recursive_copy(workspace, &backup_path, &backups_dir)?;

        Ok(format!("Backup created at {}", backup_path.display()))
    }

    fn recursive_copy(src: &Path, dst: &Path, exclude: &Path) -> EaiResult<()> {
        if src == exclude {
            return Ok(());
        }

        if src.is_dir() {
            fs::create_dir_all(dst)?;
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                let path = entry.path();
                let dest_path = dst.join(entry.file_name());
                Self::recursive_copy(&path, &dest_path, exclude)?;
            }
        } else {
            fs::copy(src, dst)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_lifecycle() {
        let ws = Path::new(".");
        let cp = NeuralCheckpoint {
            intent: "test".to_string(),
            timestamp: 0,
            completed_tools: vec![],
            blackboard: std::collections::HashMap::new(),
            status: "IN_PROGRESS".to_string(),
        };
        SandboxManager::save_mission_checkpoint(ws, &cp);
        let loaded = SandboxManager::check_interrupted_checkpoint(ws);
        assert!(loaded.is_some());
        let _ = fs::remove_dir_all(ws.join(".susi"));
    }

    #[test]
    fn test_susi_config_lifecycle() {
        let dir = Path::new("test_cfg");
        let _ = fs::create_dir_all(dir);
        let _ = SandboxManager::ensure_global_sandbox(dir);
        let cfg = SusiConfig::load(dir).expect("Failed to load config");
        assert_eq!(cfg.gmcp_port, 9090);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_susi_config_hot_reload_lifecycle() {
        let dir = Path::new("test_hot_reload_cfg");
        let _ = fs::create_dir_all(dir);
        let _ = SandboxManager::ensure_global_sandbox(dir);

        let mut cfg = SusiConfig::load(dir).expect("Failed to load initial config");
        assert_eq!(cfg.execution_lease_secs, 30);
        assert_eq!(cfg.max_concurrent_agents, 32);

        // Modify config externally and verify dynamic reload
        cfg.execution_lease_secs = 45;
        cfg.max_concurrent_agents = 64;
        cfg.save(dir).expect("Failed to save updated config");

        let reloaded = SusiConfig::reload(dir).expect("Failed to reload config");
        assert_eq!(reloaded.execution_lease_secs, 45);
        assert_eq!(reloaded.max_concurrent_agents, 64);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_susi_memory_lifecycle() {
        let ws = Path::new("test_mem");
        let _ = fs::create_dir_all(ws);
        SusiMemory::save_interaction(ws, "hello", "world");
        let memory_file = ws.join(".susi/memory.jsonl");
        assert!(memory_file.is_file());
        let _ = fs::remove_dir_all(ws);
    }
}
