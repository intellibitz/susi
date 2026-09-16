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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum TrustLevel {
    Conservative, // Level 1: Propose everything
    #[default]
    Balanced,     // Level 2 (Default): Silent auto-fix formatting/caches, Grouped cards for code edits
    Autonomous,   // Level 3: Hands-free execution + 1-click susi undo
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RiskTier {
    #[default]
    Tier0ZeroRisk,
    Tier1LowRisk,
    Tier2HighRisk,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedFix {
    pub file_path: String,
    pub original_content: String,
    pub staged_content: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IntentBundle {
    pub bundle_id: String,
    pub title: String,
    pub risk_tier: RiskTier,
    pub description: String,
    pub staged_fixes: Vec<StagedFix>,
    pub applied: bool,
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
pub struct ChatTemplateConfig {
    #[serde(flatten)]
    pub templates: std::collections::HashMap<String, String>,
}

impl Default for ChatTemplateConfig {
    fn default() -> Self {
        let mut templates = std::collections::HashMap::new();
        templates.insert("chatml".to_string(), "<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n".to_string());
        templates.insert("llama3".to_string(), "<|start_header_id|>system<|end_header_id|>\n{system}<|eot_id|><|start_header_id|>user<|end_header_id|>\n{prompt}<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n".to_string());
        templates.insert("gemma".to_string(), "<start_of_turn>user\n{system}\n{prompt}<end_of_turn>\n<start_of_turn>model\n".to_string());
        templates.insert("mistral".to_string(), "[INST] {system}\n{prompt} [/INST]".to_string());
        templates.insert("phi3".to_string(), "<|system|>\n{system}<|end|>\n<|user|>\n{prompt}<|end|>\n<|assistant|>\n".to_string());
        Self { templates }
    }
}

impl ChatTemplateConfig {
    pub fn render(&self, model_name: &str, system_prompt: &str, user_prompt: &str) -> String {
        let lower = model_name.to_lowercase();
        let lower_clean = lower.replace('-', "").replace('_', "");

        let matched_key = self.templates.keys().find(|k| {
            let k_clean = k.to_lowercase().replace('-', "").replace('_', "");
            lower_clean.contains(&k_clean)
        }).map(|s| s.as_str());

        let template_key = matched_key.unwrap_or("chatml");

        let template = self.templates.get(template_key)
            .or_else(|| self.templates.get("chatml"))
            .cloned()
            .unwrap_or_else(|| "<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n".to_string());

        template
            .replace("{system}", system_prompt)
            .replace("{prompt}", user_prompt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SusiPrompts {
    pub system_identity: String,
    pub agent_factory_prompt: String,
    pub truth_verifier_prompt: String,
    pub intent_planner_prompt: String,
    pub consensus_wisdom_prompt: String,
    pub chat_templates: ChatTemplateConfig,
}

impl Default for SusiPrompts {
    fn default() -> Self {
        Self {
            system_identity: "You are SUSI, Exponential Intelligence Substrate v{version}.".to_string(),
            agent_factory_prompt: "MISSION_GOAL: {goal}\n\n[INSTRUCTION]: You are the SUSI Agent Factory. Detect the capability gap and synthesize a specialist agent specification in JSON.".to_string(),
            truth_verifier_prompt: "Verify if tool {tool} output '{result}' matches physical reality in {workspace}.".to_string(),
            intent_planner_prompt: "MISSION_GOAL: {goal}\n\n[INSTRUCTION]: Decompose this intent into a sequence of executable sub-goals. Output as a comma-separated list.".to_string(),
            consensus_wisdom_prompt: "MISSION_GOAL: {goal}\n\n[WEIGHTED_WISDOM]:\n{wisdom}\n\n[INSTRUCTION]: Resolve conflicts using rank-weighted consensus. Output final verified answer.".to_string(),
            chat_templates: ChatTemplateConfig::default(),
        }
    }
}

impl SusiPrompts {
    pub fn load_global() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let prompts_file = home.join(".susi/prompts.json");
        if prompts_file.is_file() {
            if let Ok(content) = fs::read_to_string(&prompts_file) {
                if let Ok(p) = serde_json::from_str::<SusiPrompts>(&content) {
                    return p;
                }
            }
        }
        let default_prompts = Self::default();
        let _ = fs::create_dir_all(home.join(".susi"));
        if let Ok(json) = serde_json::to_string_pretty(&default_prompts) {
            let _ = fs::write(&prompts_file, json);
        }
        default_prompts
    }

    pub fn format_chat_prompt(&self, model_name: &str, system_prompt: &str, user_prompt: &str) -> String {
        self.chat_templates.render(model_name, system_prompt, user_prompt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonMessagesConfig {
    pub signal_received: String,
    pub binary_recompiled: String,
    pub binary_verified: String,
    pub binary_tampered: String,
    pub shutdown_initiated: String,
    pub lock_failed: String,
}

impl Default for DaemonMessagesConfig {
    fn default() -> Self {
        Self {
            signal_received: "[SusiDaemon] Received signal: {}".to_string(),
            binary_recompiled: "[SusiDaemon] Binary recompiled. Restarting daemon PID {}...".to_string(),
            binary_verified: "[SusiDaemon] Binary integrity verified.".to_string(),
            binary_tampered: "[SusiDaemon] Binary integrity check FAILED. Potential tampering detected or build out of sync.".to_string(),
            shutdown_initiated: "[SusiDaemon] Graceful shutdown initiated.".to_string(),
            lock_failed: "[SusiDaemon] Failed to acquire lock: {}. Daemon likely already running.".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadinessMessagesConfig {
    pub auditing_models: String,
    pub scanning_security: String,
    pub security_engaged: String,
}

impl Default for ReadinessMessagesConfig {
    fn default() -> Self {
        Self {
            auditing_models: "[Readiness] Auditing model substrate optimal state...".to_string(),
            scanning_security: "[Readiness] Scanning for exfiltration vectors and security leaks...".to_string(),
            security_engaged: "[READINESS: SECURITY PROTOCOLS ENGAGED]".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SwarmMessagesConfig {
    pub dispatch_init: String,
    pub consensus_reached: String,
    pub consensus_failed: String,
}

impl Default for SwarmMessagesConfig {
    fn default() -> Self {
        Self {
            dispatch_init: "- [Swarm Dispatch] Initializing Rayon work-stealing parallel execution for {} agents...".to_string(),
            consensus_reached: "[SWARM COMPLETE] Consensus reached.".to_string(),
            consensus_failed: "[SWARM FAILED] {}".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SusiMessages {
    pub daemon: DaemonMessagesConfig,
    pub readiness: ReadinessMessagesConfig,
    pub swarm: SwarmMessagesConfig,
}

impl SusiMessages {
    pub fn load_global() -> Self {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let msgs_file = home.join(".susi/messages.json");
        if msgs_file.is_file() {
            if let Ok(content) = fs::read_to_string(&msgs_file) {
                if let Ok(m) = serde_json::from_str::<SusiMessages>(&content) {
                    return m;
                }
            }
        }
        let default_msgs = Self::default();
        let _ = fs::create_dir_all(home.join(".susi"));
        if let Ok(json) = serde_json::to_string_pretty(&default_msgs) {
            let _ = fs::write(&msgs_file, json);
        }
        default_msgs
    }
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
pub struct InferenceEndpointEntry {
    pub name: String,
    pub api_base: String,
    pub protocol_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InferenceEndpointsConfig {
    pub endpoints: Vec<InferenceEndpointEntry>,
    pub vllm_api_base: String,
    pub sglang_api_base: String,
    pub llama_api_base: String,
    pub triton_api_base: String,
    pub lmdeploy_api_base: String,
}

impl Default for InferenceEndpointsConfig {
    fn default() -> Self {
        Self {
            endpoints: vec![
                InferenceEndpointEntry {
                    name: "vLLM".to_string(),
                    api_base: "http://localhost:8000/v1".to_string(),
                    protocol_type: "completions".to_string(),
                },
                InferenceEndpointEntry {
                    name: "SGLang".to_string(),
                    api_base: "http://localhost:30000/v1".to_string(),
                    protocol_type: "chat".to_string(),
                },
                InferenceEndpointEntry {
                    name: "llama.cpp".to_string(),
                    api_base: "http://localhost:8080/v1".to_string(),
                    protocol_type: "completions".to_string(),
                },
                InferenceEndpointEntry {
                    name: "Triton".to_string(),
                    api_base: "http://localhost:8001/v2/models/susi_model/generate".to_string(),
                    protocol_type: "triton".to_string(),
                },
                InferenceEndpointEntry {
                    name: "LMDeploy".to_string(),
                    api_base: "http://localhost:23333/v1".to_string(),
                    protocol_type: "completions".to_string(),
                },
            ],
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
    pub trust_level: TrustLevel,
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
            trust_level: TrustLevel::Balanced,
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

    pub fn ensure_gitignore_purity(workspace: &Path) {
        let gitignore = workspace.join(".gitignore");
        if gitignore.exists() {
            if let Ok(content) = fs::read_to_string(&gitignore) {
                if !content.contains(".susi") {
                    if let Ok(mut f) = fs::OpenOptions::new().append(true).open(&gitignore) {
                        use std::io::Write;
                        let _ = writeln!(f, "\n# SUSI Substrate ephemeral state\n.susi/");
                    }
                }
            }
        }
    }

    pub fn ensure_global_sandbox(global_dir: &Path) -> EaiResult<()> {
        Self::ensure_gitignore_purity(global_dir);
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

        tracing::info!(
            target: "susi_audit",
            event_type = event_type,
            level = ?level,
            workspace = %workspace.display(),
            details = details,
            "audit_event"
        );

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

pub struct IntentBundleManager;

impl IntentBundleManager {
    pub fn get_staged_bundles(workspace: &Path) -> Vec<IntentBundle> {
        let bundles_file = workspace.join(".susi/staged_bundles.json");
        if bundles_file.is_file() {
            if let Ok(content) = fs::read_to_string(&bundles_file) {
                return serde_json::from_str(&content).unwrap_or_default();
            }
        }
        Vec::new()
    }

    pub fn save_staged_bundles(workspace: &Path, bundles: &[IntentBundle]) -> EaiResult<()> {
        SandboxManager::ensure_gitignore_purity(workspace);
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let bundles_file = susi_dir.join("staged_bundles.json");
        let json = serde_json::to_string_pretty(bundles)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        fs::write(bundles_file, json).map_err(|e| EaiError::filesystem(e.to_string()))
    }

    pub fn stage_bundle(workspace: &Path, bundle: IntentBundle) -> EaiResult<()> {
        let mut bundles = Self::get_staged_bundles(workspace);
        bundles.retain(|b| b.bundle_id != bundle.bundle_id);
        bundles.push(bundle);
        Self::save_staged_bundles(workspace, &bundles)
    }

    pub fn accept_all(workspace: &Path) -> EaiResult<String> {
        let mut bundles = Self::get_staged_bundles(workspace);
        if bundles.is_empty() {
            return Ok("No staged intent bundles to accept.".to_string());
        }

        let mut accepted_count = 0;
        let mut files_changed = 0;

        for bundle in &mut bundles {
            if !bundle.applied {
                for fix in &bundle.staged_fixes {
                    let target_path = workspace.join(&fix.file_path);
                    if let Some(parent) = target_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    fs::write(&target_path, &fix.staged_content)
                        .map_err(|e| EaiError::filesystem(e.to_string()))?;
                    files_changed += 1;
                }
                bundle.applied = true;
                accepted_count += 1;
            }
        }

        Self::save_staged_bundles(workspace, &bundles)?;
        Ok(format!(
            "SUCCESS: Accepted {} intent bundles across {} files.",
            accepted_count, files_changed
        ))
    }

    pub fn rollback_all(workspace: &Path) -> EaiResult<String> {
        let mut bundles = Self::get_staged_bundles(workspace);
        if bundles.is_empty() {
            return Ok("No staged intent bundles to rollback.".to_string());
        }

        let mut reverted_files = 0;
        for bundle in &mut bundles {
            if bundle.applied {
                for fix in &bundle.staged_fixes {
                    let target_path = workspace.join(&fix.file_path);
                    if !fix.original_content.is_empty() {
                        let _ = fs::write(&target_path, &fix.original_content);
                    } else if target_path.exists() {
                        let _ = fs::remove_file(&target_path);
                    }
                    reverted_files += 1;
                }
                bundle.applied = false;
            }
        }

        let susi_dir = workspace.join(".susi");
        let bundles_file = susi_dir.join("staged_bundles.json");
        let _ = fs::remove_file(bundles_file);

        Ok(format!(
            "SUCCESS: Rolled back staged fixes across {} files.",
            reverted_files
        ))
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

    #[test]
    fn test_intent_bundle_staging_and_rollback_lifecycle() {
        let ws = Path::new("test_bundle_ws");
        let _ = fs::create_dir_all(ws);

        let test_file = ws.join("test_code.txt");
        let _ = fs::write(&test_file, "original code");

        let bundle = IntentBundle {
            bundle_id: "b1".to_string(),
            title: "Test Hygiene Bundle".to_string(),
            risk_tier: RiskTier::Tier0ZeroRisk,
            description: "Test fix".to_string(),
            staged_fixes: vec![StagedFix {
                file_path: "test_code.txt".to_string(),
                original_content: "original code".to_string(),
                staged_content: "refactored code".to_string(),
                description: "Refactor".to_string(),
            }],
            applied: false,
        };

        IntentBundleManager::stage_bundle(ws, bundle).expect("Staging failed");
        let staged = IntentBundleManager::get_staged_bundles(ws);
        assert_eq!(staged.len(), 1);

        IntentBundleManager::accept_all(ws).expect("Accept failed");
        let content = fs::read_to_string(&test_file).unwrap_or_default();
        assert_eq!(content, "refactored code");

        IntentBundleManager::rollback_all(ws).expect("Rollback failed");
        let rolled_back = fs::read_to_string(&test_file).unwrap_or_default();
        assert_eq!(rolled_back, "original code");

        let _ = fs::remove_dir_all(ws);
    }

    #[test]
    fn test_susi_prompts_template_lifecycle() {
        let prompts = SusiPrompts::default();
        let formatted = prompts.format_chat_prompt("Qwen2.5-32B", "System Text", "User Text");
        assert!(formatted.contains("System Text"));
        assert!(formatted.contains("User Text"));
        assert!(formatted.contains("<|im_start|>"));

        let gemma_fmt = prompts.format_chat_prompt("Gemma-2-27B", "Sys", "Usr");
        assert!(gemma_fmt.contains("<start_of_turn>"));
    }

    #[test]
    fn test_dynamic_chat_template_rendering() {
        let mut cfg = ChatTemplateConfig::default();
        cfg.templates.insert("deepseek".to_string(), "### System:\n{system}\n\n### User:\n{prompt}\n\n### Assistant:\n".to_string());

        let mistral_fmt = cfg.render("Mistral-7B-Instruct", "Sys", "Usr");
        assert!(mistral_fmt.contains("[INST]"));

        let phi_fmt = cfg.render("Phi-3-Mini", "Sys", "Usr");
        assert!(phi_fmt.contains("<|user|>"));

        let deepseek_fmt = cfg.render("DeepSeek-R1-Distill", "Sys", "Usr");
        assert!(deepseek_fmt.contains("### Assistant:"));
    }
}
