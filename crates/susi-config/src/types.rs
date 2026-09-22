//! Typed config fragments stored inside `SusiConfig` / catalogs.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use crate::json_util::{
    merge_missing_json_defaults, merge_missing_registry_defaults, DynamicRegistry, DynamicValue,
    StringRegistry,
};

pub type TrustLevel = String; // Was enum, now dynamic: "conservative", "balanced", "autonomous", "any_new_level"
pub type RiskTier = String; // Was enum, now dynamic: "Tier0ZeroRisk", "Tier1LowRisk", etc.

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DynamicModelInfo {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
}

impl DynamicModelInfo {
    // Each parameter maps 1:1 to a distinct serialized field (name/registry/
    // model_id/...); a builder would just move the same arity into chained
    // calls at every construction site for no behavioral benefit.
    #[allow(clippy::too_many_arguments)]
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

    pub fn get(&self, key: &str) -> Option<&DynamicValue> {
        self.fields.get(key)
    }
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.fields.get(key)?.as_str()
    }
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.fields.get(key)?.as_bool()
    }
    pub fn name(&self) -> &str {
        self.get_str("name")
            .or_else(|| self.get_str("model_id"))
            .unwrap_or("unknown")
    }
    pub fn model_id(&self) -> &str {
        self.get_str("model_id")
            .or_else(|| self.get_str("id"))
            .or_else(|| self.get_str("name"))
            .unwrap_or("unknown")
    }
    pub fn provider(&self) -> &str {
        self.get_str("provider").unwrap_or("unknown")
    }
    pub fn is_local(&self) -> bool {
        self.get_bool("is_local").unwrap_or(false)
    }
    pub fn registry(&self) -> &str {
        self.get_str("registry").unwrap_or("SUSI Substrate")
    }
    pub fn description(&self) -> &str {
        self.get_str("description").unwrap_or("")
    }
    pub fn tier(&self) -> &str {
        self.get_str("tier").unwrap_or("Reflex")
    }
    pub fn latency_ms(&self) -> Option<u128> {
        self.fields
            .get("latency_ms")
            .and_then(|v| v.as_u64())
            .map(|u| u as u128)
    }
    pub fn checksum(&self) -> Option<&str> {
        self.get_str("checksum")
    }
    pub fn provenance(&self) -> Option<&DynamicValue> {
        self.get("provenance")
    }
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
    pub fn file_path(&self) -> &str {
        self.fields
            .get("file_path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
    pub fn original_content(&self) -> &str {
        self.fields
            .get("original_content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
    pub fn staged_content(&self) -> &str {
        self.fields
            .get("staged_content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
    pub fn description(&self) -> &str {
        self.fields
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
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
    pub fn bundle_id(&self) -> &str {
        self.fields
            .get("bundle_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
    pub fn title(&self) -> &str {
        if !self.title.is_empty() {
            &self.title
        } else {
            self.fields
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        }
    }
    pub fn risk_tier(&self) -> RiskTier {
        self.fields
            .get("risk_tier")
            .and_then(|v| v.as_str())
            .unwrap_or("Tier0ZeroRisk")
            .to_string()
    }
    pub fn description(&self) -> &str {
        self.fields
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
    pub fn is_applied(&self) -> bool {
        self.applied
            || self
                .fields
                .get("applied")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
    }
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
    pub fn intent(&self) -> &str {
        self.fields
            .get("intent")
            .and_then(|v| v.as_str())
            .unwrap_or("")
    }
    pub fn timestamp(&self) -> u64 {
        self.fields
            .get("timestamp")
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    }
    pub fn completed_tools(&self) -> Vec<String> {
        self.fields
            .get("completed_tools")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }
    pub fn status(&self) -> &str {
        self.fields
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("IN_PROGRESS")
    }
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
        // Deserialize directly into the flattened map type, not `Self` — the
        // container's #[serde(default)] makes Self's Deserialize impl call
        // Self::default() to backfill missing fields, which would recurse
        // infinitely (stack overflow) if this constructed a Self via serde_json.
        // Mandate 42: safe - this and the other bundled-default `.expect()`
        // calls in this file (default_dynamic x2, SusiConfig::default) all
        // deserialize a file compiled into the binary via include_str!, not
        // a user-editable runtime file. Content is fixed for a given
        // binary, so parsing either always succeeds or always fails for
        // that binary - a failure is a build/packaging bug caught by any
        // test run, never a runtime condition that varies between calls.
        let templates: StringRegistry = serde_json::from_str(include_str!(
            "../../../config/chat_templates.default.json"
        ))
        .expect(
            "Fatal: chat_templates.default.json must be valid JSON. Zero hardcoded config allowed.",
        );
        Self { templates }
    }
}

impl ChatTemplateConfig {
    pub fn from_file(path: &str) -> Self {
        let p = Path::new(path);
        if p.is_file() {
            if let Ok(content) = fs::read_to_string(p) {
                if let Ok(cfg) = serde_json::from_str::<ChatTemplateConfig>(&content) {
                    return cfg;
                }
                if let Ok(map) = serde_json::from_str::<StringRegistry>(&content) {
                    return Self { templates: map };
                }
            }
        }
        Self::default()
    }

    pub fn get(&self, model_name: &str) -> Option<&String> {
        if let Some(t) = self.templates.get(model_name) {
            return Some(t);
        }
        let lower = model_name.to_lowercase().replace(['-', '_', '.'], "");
        for (k, v) in &self.templates {
            let k_clean = k.to_lowercase().replace(['-', '_', '.'], "");
            if lower.contains(&k_clean) || k_clean.contains(&lower) {
                return Some(v);
            }
        }
        self.templates
            .get("chatml")
            .or_else(|| self.templates.values().next())
    }

    // 100% dynamic render - supports ANY {variable}
    pub fn render(&self, model_name: &str, vars: &HashMap<String, String>) -> String {
        let template = self
            .get(model_name)
            .cloned()
            .unwrap_or_else(|| "{system}\n{prompt}".to_string());
        let mut out = template;
        for (k, v) in vars {
            out = out.replace(&format!("{{{}}}", k), v);
        }
        out
    }

    pub fn register(&mut self, name: String, template: String) {
        self.templates.insert(name, template);
    }
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
        let prompts_file = susi_paths::SusiDirs::config_dir().join("prompts.json");

        static STORE: std::sync::OnceLock<crate::versioned_store::VersionedJsonStore<SusiPrompts>> =
            std::sync::OnceLock::new();
        let store = STORE.get_or_init(crate::versioned_store::VersionedJsonStore::new);

        let mut prompts = store
            .load_with_healing(
                &prompts_file,
                || Ok(Self::default_dynamic()),
                |cfg| {
                    let default_prompts = Self::default_dynamic();
                    merge_missing_registry_defaults(&mut cfg.prompts, &default_prompts.prompts)
                },
                false,
            )
            .unwrap_or_else(|_| Self::default_dynamic());

        // User-editable chat-template override, hot-reloaded on every load (Mandate 15:
        // Registry Hot-Reload) independent of prompts.json's persisted snapshot.
        let templates_override = susi_paths::SusiDirs::config_dir().join("chat_templates.json");
        if templates_override.is_file() {
            prompts.chat_templates =
                ChatTemplateConfig::from_file(templates_override.to_str().unwrap_or_default());
        }
        prompts
    }

    fn default_dynamic() -> Self {
        // Mandate 42: safe - see ChatTemplateConfig::default's comment
        // above; same compile-time include_str! pattern.
        let prompts: DynamicRegistry = serde_json::from_str(include_str!(
            "../../../config/prompts.default.json"
        ))
        .expect("Fatal: prompts.default.json must be valid JSON. Zero hardcoded config allowed.");
        Self {
            prompts,
            chat_templates: ChatTemplateConfig::default(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&DynamicValue> {
        self.prompts.get(key)
    }
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.prompts.get(key)?.as_str()
    }

    pub fn agent_factory_prompt(&self) -> String {
        self.get_str("agent_factory_prompt")
            .unwrap_or("")
            .to_string()
    }
    pub fn consensus_wisdom_prompt(&self) -> String {
        self.get_str("consensus_wisdom_prompt")
            .unwrap_or("")
            .to_string()
    }
    pub fn intent_planner_prompt(&self) -> String {
        self.get_str("intent_planner_prompt")
            .unwrap_or("")
            .to_string()
    }
    pub fn mission_partition_prompt(&self) -> String {
        self.get_str("mission_partition_prompt")
            .unwrap_or("")
            .to_string()
    }
    /// Used by `DynamicAgent` (every synthesized specialist, plus the
    /// UniversalReasoner fallback). Deliberately asks for a direct answer,
    /// not a tool-call schema: nothing in the swarm ever parses an
    /// "action"/"action_input" field from an agent's raw output to execute a
    /// real tool and fill in "observation," so asking a small model to
    /// produce that schema just adds an unnecessary indirection it often
    /// can't complete - verified live, this produced JSON stubs describing
    /// an intended action with an empty "observation" instead of an answer.
    pub fn dynamic_agent_prompt(&self) -> String {
        self.get_str("dynamic_agent_prompt")
            .unwrap_or("")
            .to_string()
    }
    pub fn mission_refine_prompt(&self) -> String {
        self.get_str("mission_refine_prompt")
            .unwrap_or("")
            .to_string()
    }

    pub fn format_chat_prompt(
        &self,
        model_name: &str,
        system_prompt: &str,
        user_prompt: &str,
    ) -> String {
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
        let msgs_file = susi_paths::SusiDirs::config_dir().join("messages.json");

        static STORE: std::sync::OnceLock<
            crate::versioned_store::VersionedJsonStore<SusiMessages>,
        > = std::sync::OnceLock::new();
        let store = STORE.get_or_init(crate::versioned_store::VersionedJsonStore::new);

        store
            .load_with_healing(
                &msgs_file,
                || Ok(Self::default_dynamic()),
                |m| {
                    let default_msgs = Self::default_dynamic();
                    let mut existing_val = serde_json::to_value(&*m).unwrap_or(DynamicValue::Null);
                    let default_val =
                        serde_json::to_value(&default_msgs).unwrap_or(DynamicValue::Null);

                    if merge_missing_json_defaults(&mut existing_val, &default_val) {
                        if let Ok(merged) = serde_json::from_value::<SusiMessages>(existing_val) {
                            *m = merged;
                            return true;
                        }
                    }
                    false
                },
                false,
            )
            .unwrap_or_else(|_| Self::default_dynamic())
    }

    fn default_dynamic() -> Self {
        // Mandate 42: safe - see ChatTemplateConfig::default's comment
        // above; same compile-time include_str! pattern.
        let categories: HashMap<String, StringRegistry> = serde_json::from_str(include_str!(
            "../../../config/messages.default.json"
        ))
        .expect("Fatal: messages.default.json must be valid JSON. Zero hardcoded config allowed.");
        Self { categories }
    }

    pub fn get(&self, category: &str, key: &str) -> Option<&String> {
        self.categories.get(category)?.get(key)
    }
    pub fn register(&mut self, category: String, key: String, message: String) {
        self.categories
            .entry(category)
            .or_default()
            .insert(key, message);
    }
}

// === 100% DYNAMIC REMAINING CONFIGS ===
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct AdminPulsesConfig {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    #[serde(default)]
    pub install_pulse: String,
    #[serde(default)]
    pub uninstall_pulse: String,
    #[serde(default)]
    pub select_model_pulse: String,
    #[serde(default)]
    pub deep_scan_pulse: String,
    #[serde(default)]
    pub mcp_scout_pulse: String,
    #[serde(default)]
    pub audit_pulse: String,
    #[serde(default)]
    pub verify_pulse: String,
    #[serde(default)]
    pub lint_pulse: String,
    #[serde(default)]
    pub audit_deps_pulse: String,
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
    /// Default model id for this endpoint (e.g. `gpt-4o-mini`, `claude-3-5-haiku-…`).
    #[serde(default)]
    pub model: String,
    /// Env var holding the API key (e.g. `OPENAI_API_KEY`). Empty = no auth.
    #[serde(default)]
    pub api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct InferenceEndpointsConfig {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    #[serde(default)]
    pub endpoints: Vec<InferenceEndpointItem>,
}

impl InferenceEndpointsConfig {}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiscoverableAssetConfig {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelScoringHeuristics {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    pub size_gb_multipliers: HashMap<String, f32>,
    pub native_candle_bonus: f32,
    pub system_ram_buffer_gb: f32,
}

/// Tunable thresholds for when `SusiMemory::save_interaction` promotes an
/// interaction from plain memory into the reasoning-experience distillation
/// log (previously a hardcoded `output.len() > 50` + two magic substrings).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MemoryExperienceHeuristics {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    pub min_output_len: usize,
    pub failure_markers: Vec<String>,
}

/// Config-driven natural-intent classification for `SusiAdmin::classify_natural_intent`
/// (Mandate 35: no hardcoded keyword lists in Rust).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct IntentClassifyConfig {
    #[serde(flatten)]
    pub fields: DynamicRegistry,
    pub query_exact: Vec<String>,
    pub query_prefixes: Vec<String>,
    pub query_contains: Vec<String>,
    pub motion_contains: Vec<String>,
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
    pub fn secret_tokens(&self) -> Vec<String> {
        self.secret_tokens.clone()
    }
    pub fn destructive_commands(&self) -> Vec<String> {
        self.destructive_commands.clone()
    }
    pub fn critical_system_paths(&self) -> Vec<String> {
        self.critical_system_paths.clone()
    }
    pub fn exfiltration_vectors(&self) -> Vec<String> {
        self.exfiltration_vectors.clone()
    }
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
    pub tokenizer_repo: String,
    #[serde(default)]
    pub min_ram_gb: f32,
    #[serde(default)]
    pub min_bytes: u64,
    #[serde(default)]
    pub expected_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelLifecycleConfig {
    pub download_attempts: u32,
    pub download_timeout_secs: u64,
    pub max_parallel_downloads: usize,
    pub discovery_refresh_secs: u64,
    pub discovery_retry_secs: u64,
    pub discovery_limit: usize,
    pub max_ladder_tiers: usize,
    pub ladder_size_ratio: f32,
    pub memory_overhead_ratio: f32,
    pub prefetch_tiers: usize,
    pub failure_cooldown_secs: u64,
}

/// When local inference is too slow, escalate to cloud providers.
/// `policy`: `auto` | `local_only` | `cloud_first` | `ask`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InferenceRoutingConfig {
    pub policy: String,
    pub local_min_tokens_per_sec: f32,
    pub local_max_latency_ms: u64,
    pub prefer_cloud_when_cpu_only: bool,
    pub ask_when_multiple_clouds: bool,
}

/// Config-driven external coding-agent peer (Claude Code, Cursor, Codex, …).
///
/// Open admission: any name + protocol in `external_peer_agents` mounts as a
/// GawdAgent. Protocols: `managed`, `cli` (default), `openai_chat`, `http`, `a2a`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ExternalPeerAgentSpec {
    pub name: String,
    pub description: String,
    /// Wire protocol: `managed` | `cli` | `openai_chat` | `http` | `a2a` (default `cli`).
    pub protocol: String,
    /// Base URL for `openai_chat` / `http` / `a2a` peers (OpenAI-compat or A2A).
    pub api_base: String,
    /// Optional model id for `openai_chat` peers.
    pub model: String,
    /// Binaries probed in order (`which`-style) to select the live driver.
    pub detect_bins: Vec<String>,
    /// Explicit command; when empty, the first detected bin is used.
    pub command: String,
    /// Argv templates; `{goal}` and `{workspace}` are substituted.
    pub args: Vec<String>,
    pub timeout_secs: u64,
    /// When set, the named env var must be present (e.g. Devin API key).
    pub api_key_env: Option<String>,
}

/// One entry in the leading-models catalog (`config/models.catalog.default.json`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelCatalogEntry {
    pub id: String,
    /// Inference endpoint name from `inference_endpoints` (e.g. OpenRouter).
    pub engine: String,
    #[serde(default)]
    pub protocol_type: String,
    #[serde(default)]
    pub api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ModelCatalogConfig {
    pub models: Vec<ModelCatalogEntry>,
}

impl Default for InferenceRoutingConfig {
    fn default() -> Self {
        Self {
            policy: "auto".to_string(),
            local_min_tokens_per_sec: 8.0,
            local_max_latency_ms: 15_000,
            prefer_cloud_when_cpu_only: true,
            ask_when_multiple_clouds: true,
        }
    }
}

// === 100% DYNAMIC SUSI CONFIG - THE ROOT ===
