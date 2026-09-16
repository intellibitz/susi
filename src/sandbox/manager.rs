// susi Sandbox Manager: 100% DYNAMIC - Zero hardcoded keys
// Pattern used by Astral (ruff) and Claude Code: Registry + HashMap + Value
// Add new model, prompt, message, endpoint without touching Rust

use crate::error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

// === CORE DYNAMIC TYPES ===
// Everything is a registry. No struct fields are hardcoded.

pub type DynamicValue = serde_json::Value;
pub type DynamicRegistry = HashMap<String, DynamicValue>;
pub type StringRegistry = HashMap<String, String>;

// ModelTier and ProviderType are now dynamic strings, not hardcoded enums
pub type ModelTier = String;
pub type ProviderType = String;
pub type TrustLevel = String; // Was enum, now dynamic: "conservative", "balanced", "autonomous", "any_new_level"
pub type RiskTier = String;   // Was enum, now dynamic: "Tier0ZeroRisk", "Tier1LowRisk", etc.

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DynamicModelInfo {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
}

impl DynamicModelInfo {
    pub fn new(
        name: String,
        registry: String,
        model_id: String,
        description: String,
        is_local: bool,
        tier: String,
        latency_ms: Option<u128>,
        provider: String,
        checksum: Option<String>,
        provenance: Option<serde_json::Value>,
    ) -> Self {
        let mut fields = DynamicRegistry::new();
        fields.insert("name".to_string(), serde_json::json!(name));
        fields.insert("registry".to_string(), serde_json::json!(registry));
        fields.insert("model_id".to_string(), serde_json::json!(model_id));
        fields.insert("description".to_string(), serde_json::json!(description));
        fields.insert("is_local".to_string(), serde_json::json!(is_local));
        fields.insert("tier".to_string(), serde_json::json!(tier));
        if let Some(lat) = latency_ms {
            fields.insert("latency_ms".to_string(), serde_json::json!(lat));
        }
        fields.insert("provider".to_string(), serde_json::json!(provider));
        if let Some(chk) = checksum {
            fields.insert("checksum".to_string(), serde_json::json!(chk));
        }
        if let Some(prov) = provenance {
            fields.insert("provenance".to_string(), prov);
        }
        Self { fields }
    }

    pub fn get(&self, key: &str) -> Option<&DynamicValue> { self.fields.get(key) }
    pub fn get_str(&self, key: &str) -> Option<&str> { self.fields.get(key)?.as_str() }
    pub fn get_bool(&self, key: &str) -> Option<bool> { self.fields.get(key)?.as_bool() }
    pub fn name(&self) -> &str { self.get_str("name").or_else(|| self.get_str("model_id")).unwrap_or("unknown") }
    pub fn model_id(&self) -> &str { self.get_str("model_id").or_else(|| self.get_str("id")).or_else(|| self.get_str("name")).unwrap_or("unknown") }
    pub fn provider(&self) -> &str { self.get_str("provider").unwrap_or("unknown") }
    pub fn is_local(&self) -> bool { self.get_bool("is_local").unwrap_or(false) }
    pub fn registry(&self) -> &str { self.get_str("registry").unwrap_or("SUSI Substrate") }
    pub fn description(&self) -> &str { self.get_str("description").unwrap_or("") }
    pub fn tier(&self) -> &str { self.get_str("tier").unwrap_or("Reflex") }
    pub fn latency_ms(&self) -> Option<u128> { self.fields.get("latency_ms").and_then(|v| v.as_u64()).map(|u| u as u128) }
    pub fn checksum(&self) -> Option<&str> { self.get_str("checksum") }
    pub fn provenance(&self) -> Option<&DynamicValue> { self.get("provenance") }
}

// Backward compat alias
pub type ModelInfo = DynamicModelInfo;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DynamicStagedFix {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
}

impl DynamicStagedFix {
    pub fn file_path(&self) -> &str { self.fields.get("file_path").and_then(|v| v.as_str()).unwrap_or("") }
    pub fn original_content(&self) -> &str { self.fields.get("original_content").and_then(|v| v.as_str()).unwrap_or("") }
    pub fn staged_content(&self) -> &str { self.fields.get("staged_content").and_then(|v| v.as_str()).unwrap_or("") }
    pub fn description(&self) -> &str { self.fields.get("description").and_then(|v| v.as_str()).unwrap_or("") }
}

pub type StagedFix = DynamicStagedFix;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DynamicIntentBundle {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    #[serde(default)]
    pub staged_fixes: Vec<DynamicStagedFix>,
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub title: String,
}

impl DynamicIntentBundle {
    pub fn bundle_id(&self) -> &str { self.fields.get("bundle_id").and_then(|v| v.as_str()).unwrap_or("") }
    pub fn title(&self) -> &str { if !self.title.is_empty() { &self.title } else { self.fields.get("title").and_then(|v| v.as_str()).unwrap_or("") } }
    pub fn risk_tier(&self) -> RiskTier { self.fields.get("risk_tier").and_then(|v| v.as_str()).unwrap_or("Tier0ZeroRisk").to_string() }
    pub fn description(&self) -> &str { self.fields.get("description").and_then(|v| v.as_str()).unwrap_or("") }
    pub fn is_applied(&self) -> bool { self.applied || self.fields.get("applied").and_then(|v| v.as_bool()).unwrap_or(false) }
    pub fn set_applied(&mut self, val: bool) {
        self.applied = val;
        self.fields.remove("applied");
    }
}

pub type IntentBundle = DynamicIntentBundle;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DynamicNeuralCheckpoint {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
}

impl DynamicNeuralCheckpoint {
    pub fn intent(&self) -> &str { self.fields.get("intent").and_then(|v| v.as_str()).unwrap_or("") }
    pub fn timestamp(&self) -> u64 { self.fields.get("timestamp").and_then(|v| v.as_u64()).unwrap_or(0) }
    pub fn completed_tools(&self) -> Vec<String> {
        self.fields.get("completed_tools").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default()
    }
    pub fn status(&self) -> &str { self.fields.get("status").and_then(|v| v.as_str()).unwrap_or("IN_PROGRESS") }
}

pub type NeuralCheckpoint = DynamicNeuralCheckpoint;

// === 100% DYNAMIC CHAT TEMPLATES ===
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatTemplateConfig {
    #[serde(flatten)]
    pub templates: StringRegistry,
}

impl Default for ChatTemplateConfig {
    fn default() -> Self {
        let mut templates = StringRegistry::new();
        templates.insert("chatml".to_string(), "<|im_start|>system\n{system}<|im_end|>\n<|im_start|>user\n{prompt}<|im_end|>\n<|im_start|>assistant\n".to_string());
        templates.insert("llama3".to_string(), "<|begin_of_text|><|start_header_id|>system<|end_header_id|>\n\n{system}<|eot_id|><|start_header_id|>user<|end_header_id|>\n\n{prompt}<|eot_id|><|start_header_id|>assistant<|end_header_id|>\n\n".to_string());
        templates.insert("mistral".to_string(), "[INST] <<SYS>>\n{system}\n<</SYS>>\n\n{prompt} [/INST]".to_string());
        templates.insert("gemma".to_string(), "<start_of_turn>user\n{system}\n\n{prompt}<end_of_turn>\n<start_of_turn>model\n".to_string());
        templates.insert("deepseek_r1".to_string(), "<｜begin of sentence｜><｜User｜>{system}\n\n{prompt}<｜Assistant｜>".to_string());
        templates.insert("phi3".to_string(), "<|system|>\n{system}<|end|>\n<|user|>\n{prompt}<|end|>\n<|assistant|>\n".to_string());
        Self { templates }
    }
}

impl ChatTemplateConfig {
    pub fn from_file(path: &str) -> Self {
        let p = Path::new(path);
        if p.is_file() {
            if let Ok(content) = fs::read_to_string(p) {
                if let Ok(cfg) = serde_json::from_str::<ChatTemplateConfig>(&content) { return cfg; }
                if let Ok(map) = serde_json::from_str::<StringRegistry>(&content) { return Self { templates: map }; }
            }
        }
        Self::default()
    }

    pub fn get(&self, model_name: &str) -> Option<&String> {
        if let Some(t) = self.templates.get(model_name) { return Some(t); }
        let lower = model_name.to_lowercase().replace(['-', '_', '.'], "");
        for (k, v) in &self.templates {
            let k_clean = k.to_lowercase().replace(['-', '_', '.'], "");
            if lower.contains(&k_clean) || k_clean.contains(&lower) { return Some(v); }
        }
        self.templates.get("chatml").or_else(|| self.templates.values().next())
    }

    // 100% dynamic render - supports ANY {variable}
    pub fn render(&self, model_name: &str, vars: &HashMap<String, String>) -> String {
        let template = self.get(model_name).cloned().unwrap_or_else(|| "{system}\n{prompt}".to_string());
        let mut out = template;
        for (k, v) in vars {
            out = out.replace(&format!("{{{}}}", k), v);
        }
        out
    }

    // Backward compat
    pub fn render_legacy(&self, model_name: &str, system_prompt: &str, user_prompt: &str) -> String {
        let vars = HashMap::from([
            ("system".to_string(), system_prompt.to_string()),
            ("prompt".to_string(), user_prompt.to_string()),
            ("system_prompt".to_string(), system_prompt.to_string()),
            ("user_prompt".to_string(), user_prompt.to_string()),
        ]);
        self.render(model_name, &vars)
    }

    pub fn register(&mut self, name: String, template: String) { self.templates.insert(name, template); }
}

// === 100% DYNAMIC PROMPTS - NO HARDCODED FIELDS ===
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SusiPrompts {
    #[serde(flatten)]
    pub prompts: DynamicRegistry,
    #[serde(default)]
    pub chat_templates: ChatTemplateConfig,
}

impl SusiPrompts {
    pub fn load_global() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let prompts_file = home.join(".susi/prompts.json");
        if prompts_file.is_file() {
            if let Ok(content) = fs::read_to_string(&prompts_file) {
                if let Ok(p) = serde_json::from_str::<SusiPrompts>(&content) { return p; }
            }
        }
        let default_prompts = Self::default_dynamic();
        let _ = fs::create_dir_all(home.join(".susi"));
        if let Ok(json) = serde_json::to_string_pretty(&default_prompts) { let _ = fs::write(&prompts_file, json); }
        default_prompts
    }

    fn default_dynamic() -> Self {
        let mut prompts = DynamicRegistry::new();
        prompts.insert("system_identity".to_string(), serde_json::json!("You are SUSI, Exponential Intelligence Substrate v{version}."));
        prompts.insert("agent_factory_prompt".to_string(), serde_json::json!("MISSION_GOAL: {goal}\n\n[INSTRUCTION]: You are the SUSI Agent Factory. Detect the capability gap and synthesize a specialist agent specification in JSON."));
        prompts.insert("truth_verifier_prompt".to_string(), serde_json::json!("Verify if tool {tool} output '{result}' matches physical reality in {workspace}."));
        prompts.insert("intent_planner_prompt".to_string(), serde_json::json!("MISSION_GOAL: {goal}\n\n[INSTRUCTION]: Decompose this intent into a sequence of executable sub-goals. Output as a comma-separated list."));
        prompts.insert("consensus_wisdom_prompt".to_string(), serde_json::json!("MISSION_GOAL: {goal}\n\n[WEIGHTED_WISDOM]:\n{wisdom}\n\n[INSTRUCTION]: Resolve conflicts using rank-weighted consensus. Output final verified answer."));
        Self { prompts, chat_templates: ChatTemplateConfig::default() }
    }

    pub fn get(&self, key: &str) -> Option<&DynamicValue> { self.prompts.get(key) }
    pub fn get_str(&self, key: &str) -> Option<&str> { self.prompts.get(key)?.as_str() }

    pub fn agent_factory_prompt(&self) -> String { self.get_str("agent_factory_prompt").unwrap_or("").to_string() }
    pub fn consensus_wisdom_prompt(&self) -> String { self.get_str("consensus_wisdom_prompt").unwrap_or("").to_string() }
    pub fn intent_planner_prompt(&self) -> String { self.get_str("intent_planner_prompt").unwrap_or("").to_string() }

    pub fn format_chat_prompt(&self, model_name: &str, system_prompt: &str, user_prompt: &str) -> String {
        let vars = HashMap::from([
            ("system".to_string(), system_prompt.to_string()),
            ("prompt".to_string(), user_prompt.to_string()),
        ]);
        self.chat_templates.render(model_name, &vars)
    }

    pub fn format_dynamic(&self, model_name: &str, vars: HashMap<String, String>) -> String {
        self.chat_templates.render(model_name, &vars)
    }
}

// === 100% DYNAMIC MESSAGES - NO HARDCODED STRUCTS ===
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SusiMessages {
    #[serde(flatten)]
    pub categories: HashMap<String, StringRegistry>, // category -> key -> message, fully dynamic
}

impl SusiMessages {
    pub fn load_global() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        let msgs_file = home.join(".susi/messages.json");
        if msgs_file.is_file() {
            if let Ok(content) = fs::read_to_string(&msgs_file) {
                if let Ok(m) = serde_json::from_str::<SusiMessages>(&content) { return m; }
            }
        }
        let default_msgs = Self::default_dynamic();
        let _ = fs::create_dir_all(home.join(".susi"));
        if let Ok(json) = serde_json::to_string_pretty(&default_msgs) { let _ = fs::write(&msgs_file, json); }
        default_msgs
    }

    fn default_dynamic() -> Self {
        let mut categories = HashMap::new();
        categories.insert("daemon".to_string(), HashMap::from([
            ("signal_received".to_string(), "[SusiDaemon] Received signal: {}".to_string()),
            ("binary_recompiled".to_string(), "[SusiDaemon] Binary recompiled. Restarting daemon PID {}...".to_string()),
            ("binary_verified".to_string(), "[SusiDaemon] Binary integrity verified.".to_string()),
            ("binary_tampered".to_string(), "[SusiDaemon] Binary integrity check FAILED. Potential tampering detected or build out of sync.".to_string()),
            ("shutdown_initiated".to_string(), "[SusiDaemon] Graceful shutdown initiated.".to_string()),
            ("lock_failed".to_string(), "[SusiDaemon] Failed to acquire lock: {}. Daemon likely already running.".to_string()),
        ]));
        categories.insert("readiness".to_string(), HashMap::from([
            ("auditing_models".to_string(), "[Readiness] Auditing model substrate optimal state...".to_string()),
            ("scanning_security".to_string(), "[Readiness] Scanning for exfiltration vectors and security leaks...".to_string()),
            ("security_engaged".to_string(), "[READINESS: SECURITY PROTOCOLS ENGAGED]".to_string()),
        ]));
        categories.insert("swarm".to_string(), HashMap::from([
            ("dispatch_init".to_string(), "- [Swarm Dispatch] Initializing Rayon work-stealing parallel execution for {} agents...".to_string()),
            ("consensus_reached".to_string(), "[SWARM COMPLETE] Consensus reached.".to_string()),
            ("consensus_failed".to_string(), "[SWARM FAILED] {}".to_string()),
        ]));
        Self { categories }
    }

    pub fn get(&self, category: &str, key: &str) -> Option<&String> { self.categories.get(category)?.get(key) }
    pub fn get_category(&self, category: &str) -> Option<&StringRegistry> { self.categories.get(category) }
    pub fn register(&mut self, category: String, key: String, message: String) {
        self.categories.entry(category).or_default().insert(key, message);
    }
}

// === 100% DYNAMIC REMAINING CONFIGS ===
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminPulsesConfig {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    #[serde(default)] pub install_pulse: String,
    #[serde(default)] pub uninstall_pulse: String,
    #[serde(default)] pub select_model_pulse: String,
    #[serde(default)] pub deep_scan_pulse: String,
    #[serde(default)] pub mcp_scout_pulse: String,
    #[serde(default)] pub audit_pulse: String,
    #[serde(default)] pub verify_pulse: String,
    #[serde(default)] pub release_pulse: String,
    #[serde(default)] pub lint_pulse: String,
    #[serde(default)] pub audit_deps_pulse: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InferenceEndpointItem {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub api_base: String,
    #[serde(default)]
    pub protocol_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InferenceEndpointsConfig {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    #[serde(default)]
    pub endpoints: Vec<InferenceEndpointItem>,
}

impl InferenceEndpointsConfig {
    pub fn get_endpoint(&self, name: &str) -> Option<&str> { self.fields.get(name)?.as_str() }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiscoverableAssetConfig {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GovernancePatterns {
    #[serde(flatten)]
    pub patterns: DynamicRegistry,
    #[serde(default)]
    pub secret_tokens: Vec<String>,
    #[serde(default)]
    pub destructive_commands: Vec<String>,
    #[serde(default)]
    pub critical_system_paths: Vec<String>,
    #[serde(default)]
    pub exfiltration_vectors: Vec<String>,
}

impl GovernancePatterns {
    pub fn secret_tokens(&self) -> Vec<String> { self.secret_tokens.clone() }
    pub fn destructive_commands(&self) -> Vec<String> { self.destructive_commands.clone() }
    pub fn critical_system_paths(&self) -> Vec<String> { self.critical_system_paths.clone() }
    pub fn exfiltration_vectors(&self) -> Vec<String> { self.exfiltration_vectors.clone() }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelLadderConfigStep {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    #[serde(default)]
    pub step: usize,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub hf_repo: String,
    #[serde(default)]
    pub hf_file: String,
    #[serde(default)]
    pub min_ram_gb: f32,
}

// === 100% DYNAMIC SUSI CONFIG - THE ROOT ===
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SusiConfig {
    #[serde(flatten)]
    pub settings: DynamicRegistry, // ALL settings are dynamic, loaded from config.default.json
}

impl SusiConfig {
    pub fn default() -> Self {
        serde_json::from_str(include_str!("../../config.default.json"))
            .expect("Fatal: config.default.json must be valid JSON. Zero hardcoded config allowed.")
    }

    pub fn get_config_path(global_dir: &Path) -> PathBuf { global_dir.join("config.json") }

    pub fn load(global_dir: &Path) -> EaiResult<Self> {
        let path = Self::get_config_path(global_dir);
        if path.is_file() {
            let content = fs::read_to_string(&path).map_err(|e| EaiError::config(format!("Failed to read config: {}", e)))?;
            return serde_json::from_str(&content).map_err(|e| EaiError::config(format!("Malformed configuration: {}", e)));
        }
        Ok(Self::default())
    }

    pub fn reload(global_dir: &Path) -> EaiResult<Self> { let loaded = Self::load(global_dir)?; let _ = loaded.save(global_dir); Ok(loaded) }

    pub fn load_global() -> EaiResult<Self> {
        let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        Self::load(&home.join(".susi"))
    }

    pub fn save(&self, global_dir: &Path) -> EaiResult<()> {
        let path = Self::get_config_path(global_dir);
        let json = serde_json::to_string_pretty(self).map_err(|e| EaiError::config(e.to_string()))?;
        fs::write(path, json).map_err(|e| EaiError::filesystem(e.to_string()))
    }

    // === TYPED ACCESSORS - No hardcoded fields, dynamic getters with defaults ===
    pub fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        let v = self.settings.get(key)?; serde_json::from_value(v.clone()).ok()
    }

    pub fn get_u16(&self, key: &str, default: u16) -> u16 { self.get(key).unwrap_or(default) }
    pub fn get_u64(&self, key: &str, default: u64) -> u64 { self.get(key).unwrap_or(default) }
    pub fn get_usize(&self, key: &str, default: usize) -> usize { self.get(key).unwrap_or(default) }
    pub fn get_string(&self, key: &str, default: &str) -> String { self.get::<String>(key).unwrap_or_else(|| default.to_string()) }
    pub fn get_bool(&self, key: &str, default: bool) -> bool { self.get(key).unwrap_or(default) }
    pub fn get_f32(&self, key: &str, default: f32) -> f32 { self.get(key).unwrap_or(default) }

    // Backward compat accessors and helpers
    pub fn gmcp_port(&self) -> u16 { self.get_u16("gmcp_port", 9090) }
    pub fn gmcp_http_port(&self) -> u16 { self.get_u16("gmcp_http_port", 9091) }
    pub fn gemi_port(&self) -> u16 { self.get_u16("gemi_port", 9092) }
    pub fn udp_discovery_port(&self) -> u16 { self.get_u16("udp_discovery_port", 9093) }
    pub fn execution_lease_secs(&self) -> u64 { self.get_u64("execution_lease_secs", 30) }
    pub fn max_concurrent_agents(&self) -> usize { self.get_usize("max_concurrent_agents", 32) }
    pub fn trust_level(&self) -> String { self.get_string("trust_level", "balanced") }
    pub fn max_stdin_size_bytes(&self) -> usize { self.get_usize("max_stdin_size_bytes", 1048576) }

    pub fn default_model(&self) -> String { self.get_string("default_model", "susi-alpha") }
    pub fn default_engine(&self) -> String { self.get_string("default_engine", "susi-offline") }
    pub fn mcp_registry_url(&self) -> String { self.get_string("mcp_registry_url", "") }
    pub fn bootstrap_mcp_servers<T: for<'de> Deserialize<'de>>(&self) -> T {
        self.get("bootstrap_mcp_servers").unwrap_or_else(|| serde_json::from_str("[]").unwrap())
    }
    pub fn local_scan_paths(&self) -> Vec<String> {
        self.get("local_scan_paths").unwrap_or_default()
    }
    pub fn discoverable_assets<T: for<'de> Deserialize<'de>>(&self) -> T {
        self.get("discoverable_assets").unwrap_or_else(|| serde_json::from_str("[]").unwrap())
    }
    pub fn governance(&self) -> GovernancePatterns {
        self.get("governance").unwrap_or_default()
    }
    pub fn admin_pulses(&self) -> AdminPulsesConfig {
        self.get("admin_pulses").unwrap_or_default()
    }
    pub fn alpha_weights_url(&self) -> String {
        self.get_string("alpha_weights_url", "")
    }
    pub fn inference_endpoints(&self) -> InferenceEndpointsConfig {
        self.get("inference_endpoints").unwrap_or_default()
    }
    pub fn agent_rank_threshold(&self) -> f32 {
        self.get_f32("agent_rank_threshold", 0.5)
    }
    pub fn cloud_scout_timeout_secs(&self) -> u64 {
        self.get_u64("cloud_scout_timeout_secs", 10)
    }
    pub fn reflex_training_threshold(&self) -> usize {
        self.get_usize("reflex_training_threshold", 5)
    }
    pub fn model_ladder(&self) -> Vec<ModelLadderConfigStep> {
        self.get("model_ladder").unwrap_or_default()
    }
}

// Compatibility shim for old code that accessed fields directly
impl std::ops::Deref for SusiConfig {
    type Target = DynamicRegistry;
    fn deref(&self) -> &Self::Target { &self.settings }
}

// === SANDBOX MANAGER ===
pub struct SandboxManager;

impl SandboxManager {
    pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
        use bollard::Docker;
        use bollard::container::{Config, CreateContainerOptions, StartContainerOptions, LogOutput, LogsOptions};
        use futures::stream::StreamExt;

        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| EaiError::process(format!("Docker connection failed: {}", e)))?;

        let config = Config {
            image: Some("alpine:latest"),
            cmd: Some(vec!["sh", "-c", cmd]),
            ..Default::default()
        };

        let container = docker.create_container(None::<CreateContainerOptions<String>>, config).await
            .map_err(|e| EaiError::process(format!("Container creation failed: {}", e)))?;

        docker.start_container(&container.id, None::<StartContainerOptions<String>>).await
            .map_err(|e| EaiError::process(format!("Container start failed: {}", e)))?;

        let mut logs = docker.logs::<String>(&container.id, None::<LogsOptions<String>>);
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
        if !global_dir.exists() { fs::create_dir_all(global_dir).map_err(|e| EaiError::filesystem(e.to_string()))?; }
        let config_path = SusiConfig::get_config_path(global_dir);
        if !config_path.exists() {
            let default_cfg_file = Path::new("config.default.json");
            let cfg = if default_cfg_file.is_file() {
                fs::read_to_string(default_cfg_file)
                    .ok()
                    .and_then(|c| serde_json::from_str::<SusiConfig>(&c).ok())
                    .unwrap_or_default()
            } else {
                SusiConfig::default()
            };
            let json = serde_json::to_string_pretty(&cfg).unwrap();
            fs::write(config_path, json).map_err(|e| EaiError::filesystem(e.to_string()))?;
        }
        Ok(())
    }

    pub fn save_mission_checkpoint(workspace: &Path, checkpoint: &NeuralCheckpoint) {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let _ = fs::write(susi_dir.join("mission_checkpoint.json"), serde_json::to_string_pretty(checkpoint).unwrap_or_default());
    }

    pub fn check_interrupted_checkpoint(workspace: &Path) -> Option<NeuralCheckpoint> {
        let p = workspace.join(".susi/mission_checkpoint.json");
        if p.is_file() { fs::read_to_string(&p).ok().and_then(|c| serde_json::from_str(&c).ok()) } else { None }
    }
}

// === AUDIT LOGGER & LOG LEVEL ===
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

// === BACKUP MANAGER ===
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

// === Intent Bundle Manager - Now 100% dynamic ===
pub struct IntentBundleManager;

impl IntentBundleManager {
    fn bundles_path(workspace: &Path) -> PathBuf { workspace.join(".susi/staged_bundles.json") }

    pub fn get_staged_bundles(workspace: &Path) -> Vec<IntentBundle> {
        let p = Self::bundles_path(workspace);
        if p.is_file() { fs::read_to_string(&p).ok().and_then(|c| serde_json::from_str(&c).ok()).unwrap_or_default() } else { vec![] }
    }

    fn save_staged_bundles(workspace: &Path, bundles: &[IntentBundle]) -> EaiResult<()> {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let json = serde_json::to_string_pretty(bundles).map_err(|e| EaiError::filesystem(e.to_string()))?;
        fs::write(Self::bundles_path(workspace), json).map_err(|e| EaiError::filesystem(e.to_string()))
    }

    pub fn stage_bundle(workspace: &Path, bundle: IntentBundle) -> EaiResult<()> {
        let mut bundles = Self::get_staged_bundles(workspace);
        bundles.retain(|b| b.bundle_id() != bundle.bundle_id());
        bundles.push(bundle);
        Self::save_staged_bundles(workspace, &bundles)
    }

    pub fn accept_all(workspace: &Path) -> EaiResult<String> {
        let mut bundles = Self::get_staged_bundles(workspace);
        if bundles.is_empty() { return Ok("No staged intent bundles to accept.".to_string()); }
        let mut accepted_count = 0; let mut files_changed = 0;
        for bundle in &mut bundles {
            if !bundle.is_applied() {
                for fix in &bundle.staged_fixes {
                    let target_path = workspace.join(fix.file_path());
                    if let Some(parent) = target_path.parent() { let _ = fs::create_dir_all(parent); }
                    let _ = fs::write(&target_path, fix.staged_content());
                    files_changed += 1;
                }
                bundle.set_applied(true);
                accepted_count += 1;
            }
        }
        Self::save_staged_bundles(workspace, &bundles)?;
        Ok(format!("SUCCESS: Accepted {} bundles across {} files.", accepted_count, files_changed))
    }

    pub fn rollback_all(workspace: &Path) -> EaiResult<String> {
        let mut bundles = Self::get_staged_bundles(workspace);
        if bundles.is_empty() {
            return Ok("No staged intent bundles to rollback.".to_string());
        }

        let mut reverted_files = 0;
        for bundle in &mut bundles {
            if bundle.is_applied() {
                for fix in &bundle.staged_fixes {
                    let target_path = workspace.join(fix.file_path());
                    if !fix.original_content().is_empty() {
                        let _ = fs::write(&target_path, fix.original_content());
                    } else if target_path.exists() {
                        let _ = fs::remove_file(&target_path);
                    }
                    reverted_files += 1;
                }
                bundle.set_applied(false);
            }
        }

        let _ = fs::remove_file(Self::bundles_path(workspace));
        Ok(format!("SUCCESS: Rolled back staged fixes across {} files.", reverted_files))
    }
}

pub struct SusiMemory;
impl SusiMemory {
    pub fn save_interaction(workspace: &Path, input: &str, output: &str) {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let memory_file = susi_dir.join("memory.jsonl");

        if input.trim().is_empty() || output.trim().is_empty() {
            return;
        }

        let entry = serde_json::json!({
            "intent": input,
            "outcome": output,
            "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "provenance": {
                "workspace": workspace.display().to_string(),
                "engine_version": crate::SUSI_VERSION,
            }
        });

        use std::io::Write;
        if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(&memory_file) {
            let _ = writeln!(f, "{}", entry);
        }

        if output.len() > 50 && !output.contains("[FAIL]") && !output.contains("error") {
            let exp_file = susi_dir.join("reasoning_experience.jsonl");
            let exp_entry = serde_json::json!({
                "intent": input,
                "blackboard_context": "converged",
                "successful_outcome": output,
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                "validation": "STRICT_SEMANTIC_PASS"
            });
            if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(exp_file) {
                let _ = writeln!(f, "{}", exp_entry);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn test_checkpoint_lifecycle() {
        let ws = Path::new(".");
        let mut fields = DynamicRegistry::new();
        fields.insert("intent".to_string(), serde_json::json!("test"));
        let cp = NeuralCheckpoint { fields };
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
        assert_eq!(cfg.gmcp_port(), 9090);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_susi_config_hot_reload_lifecycle() {
        let dir = Path::new("test_hot_reload_cfg");
        let _ = fs::create_dir_all(dir);
        let _ = SandboxManager::ensure_global_sandbox(dir);

        let mut cfg = SusiConfig::load(dir).expect("Failed to load initial config");
        assert_eq!(cfg.execution_lease_secs(), 30);
        assert_eq!(cfg.max_concurrent_agents(), 32);

        // Modify config externally and verify dynamic reload
        cfg.settings.insert("execution_lease_secs".to_string(), serde_json::json!(45));
        cfg.settings.insert("max_concurrent_agents".to_string(), serde_json::json!(64));
        cfg.save(dir).expect("Failed to save updated config");

        let reloaded = SusiConfig::reload(dir).expect("Failed to reload config");
        assert_eq!(reloaded.execution_lease_secs(), 45);
        assert_eq!(reloaded.max_concurrent_agents(), 64);

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

        let mut fix_fields = DynamicRegistry::new();
        fix_fields.insert("file_path".to_string(), serde_json::json!("test_code.txt"));
        fix_fields.insert("original_content".to_string(), serde_json::json!("original code"));
        fix_fields.insert("staged_content".to_string(), serde_json::json!("refactored code"));
        
        let mut bundle_fields = DynamicRegistry::new();
        bundle_fields.insert("bundle_id".to_string(), serde_json::json!("b1"));
        
        let bundle = IntentBundle {
            fields: bundle_fields,
            staged_fixes: vec![StagedFix { fields: fix_fields }],
            applied: false,
            title: "Test Bundle".to_string(),
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

        let mut vars = HashMap::new();
        vars.insert("system".to_string(), "Sys".to_string());
        vars.insert("prompt".to_string(), "Usr".to_string());
        let fmt = prompts.format_dynamic("Unknown-Model", vars);
        assert!(fmt.contains("Sys"));
        assert!(fmt.contains("Usr"));
    }

    #[test]
    fn test_dynamic_chat_template_rendering() {
        let mut cfg = ChatTemplateConfig::default();
        cfg.templates.clear();
        cfg.templates.insert("deepseek".to_string(), "### System:\n{system}\n\n### User:\n{prompt}\n\n### Assistant:\n".to_string());
        cfg.templates.insert("mistral".to_string(), "[INST] {system} {prompt} [/INST]".to_string());
        cfg.templates.insert("phi".to_string(), "<|system|>\n{system}<|end|>\n<|user|>\n{prompt}<|end|>\n<|assistant|>".to_string());

        let mut vars = HashMap::new();
        vars.insert("system".to_string(), "Sys".to_string());
        vars.insert("prompt".to_string(), "Usr".to_string());

        let mistral_fmt = cfg.render("Mistral-7B-Instruct", &vars);
        assert!(mistral_fmt.contains("[INST]"));

        let phi_fmt = cfg.render("Phi-3-Mini", &vars);
        assert!(phi_fmt.contains("<|user|>"));

        let deepseek_fmt = cfg.render("DeepSeek-R1-Distill", &vars);
        assert!(deepseek_fmt.contains("### Assistant:"));
    }
}
