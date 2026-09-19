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

// === SHARED SELF-HEALING JSON LOAD/SAVE ===
// Every `*.default.json`-backed config type (SusiConfig, SusiPrompts,
// SusiMessages) needs the same two things: an atomic write (so a concurrent
// reader never observes a torn file) and a recursive merge that backfills a
// key/array-element present in the compiled-in default but missing from the
// user's persisted file, without ever touching a value the user already set.
// Factored out once here instead of three separately hand-rolled (and, until
// this was noticed, inconsistently deep) copies.

/// Writes `value` as pretty JSON to `path` via a same-directory temp file +
/// rename, so a concurrent reader — another process's CLI invocation, the
/// daemon's own background cycle — never observes a torn/empty file.
pub fn atomic_write_json_pretty<T: Serialize>(path: &Path, value: &T) -> EaiResult<()> {
    let json = serde_json::to_string_pretty(value).map_err(|e| EaiError::config(e.to_string()))?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = dir.join(format!(
        "{}.tmp.{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("tmp"),
        std::process::id()
    ));
    fs::write(&tmp_path, json).map_err(|e| EaiError::filesystem(e.to_string()))?;
    fs::rename(&tmp_path, path).map_err(|e| EaiError::filesystem(e.to_string()))
}

/// Recursively backfills any key (object) or element (same-length array)
/// present in `default` but absent from `existing`. Returns whether
/// `existing` was modified. A value `existing` already has is never
/// overwritten, at any nesting depth.
pub fn merge_missing_json_defaults(existing: &mut DynamicValue, default: &DynamicValue) -> bool {
    match (existing, default) {
        (DynamicValue::Object(existing_map), DynamicValue::Object(default_map)) => {
            let mut changed = false;
            for (k, def_v) in default_map {
                match existing_map.get_mut(k) {
                    Some(existing_v) => {
                        if merge_missing_json_defaults(existing_v, def_v) {
                            changed = true;
                        }
                    }
                    None => {
                        existing_map.insert(k.clone(), def_v.clone());
                        changed = true;
                    }
                }
            }
            changed
        }
        (DynamicValue::Array(existing_arr), DynamicValue::Array(default_arr))
            if existing_arr.len() == default_arr.len() =>
        {
            let mut changed = false;
            for (e, d) in existing_arr.iter_mut().zip(default_arr.iter()) {
                if merge_missing_json_defaults(e, d) {
                    changed = true;
                }
            }
            changed
        }
        _ => false,
    }
}

/// Backfills into `existing` any top-level key present in `default` but
/// missing, and recursively self-heals nested values (via
/// `merge_missing_json_defaults`) for keys both sides already have. Returns
/// whether `existing` changed.
pub fn merge_missing_registry_defaults(
    existing: &mut DynamicRegistry,
    default: &DynamicRegistry,
) -> bool {
    let mut changed = false;
    for (key, default_val) in default {
        match existing.get_mut(key) {
            Some(existing_val) => {
                if merge_missing_json_defaults(existing_val, default_val) {
                    changed = true;
                }
            }
            None => {
                existing.insert(key.clone(), default_val.clone());
                changed = true;
            }
        }
    }
    changed
}

/// Shared ureq Agent with connect/read/write timeouts. `ureq::get`/`ureq::post`
/// free functions use a default agent with NO timeouts at all — a stalled
/// remote (or one that completes the handshake but then goes silent
/// mid-response, e.g. during SSE body streaming) blocks the calling thread
/// forever. Agent-level timeout_read/timeout_write bound every socket read
/// and write, including streaming body reads after the initial response
/// headers arrive, which a per-request `.timeout()` alone would not cover.
static HTTP_AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();

pub fn http_agent() -> ureq::Agent {
    HTTP_AGENT
        .get_or_init(|| {
            let config = ureq::Agent::config_builder()
                .timeout_connect(Some(std::time::Duration::from_secs(10)))
                .timeout_recv_body(Some(std::time::Duration::from_secs(20)))
                .timeout_send_body(Some(std::time::Duration::from_secs(20)))
                .build();
            ureq::Agent::new_with_config(config)
        })
        .clone()
}
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
        let templates: StringRegistry = serde_json::from_str(include_str!(
            "../../config/chat_templates.default.json"
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

    // Backward compat
    pub fn render_legacy(
        &self,
        model_name: &str,
        system_prompt: &str,
        user_prompt: &str,
    ) -> String {
        let vars = HashMap::from([
            ("system".to_string(), system_prompt.to_string()),
            ("prompt".to_string(), user_prompt.to_string()),
            ("system_prompt".to_string(), system_prompt.to_string()),
            ("user_prompt".to_string(), user_prompt.to_string()),
        ]);
        self.render(model_name, &vars)
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
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let prompts_file = home.join(".susi/prompts.json");

        static STORE: std::sync::OnceLock<crate::sandbox::VersionedJsonStore<SusiPrompts>> = std::sync::OnceLock::new();
        let store = STORE.get_or_init(|| crate::sandbox::VersionedJsonStore::new());

        let mut prompts = store.load_with_healing(
            &prompts_file,
            || Ok(Self::default_dynamic()),
            |cfg| {
                let default_prompts = Self::default_dynamic();
                merge_missing_registry_defaults(&mut cfg.prompts, &default_prompts.prompts)
            },
            false
        ).unwrap_or_else(|_| Self::default_dynamic());

        // User-editable chat-template override, hot-reloaded on every load (Mandate 15:
        // Registry Hot-Reload) independent of prompts.json's persisted snapshot.
        let templates_override = home.join(".susi/chat_templates.json");
        if templates_override.is_file() {
            prompts.chat_templates =
                ChatTemplateConfig::from_file(templates_override.to_str().unwrap_or_default());
        }
        prompts
    }

    fn default_dynamic() -> Self {
        let prompts: DynamicRegistry = serde_json::from_str(include_str!(
            "../../config/prompts.default.json"
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
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        let msgs_file = home.join(".susi/messages.json");

        static STORE: std::sync::OnceLock<crate::sandbox::VersionedJsonStore<SusiMessages>> = std::sync::OnceLock::new();
        let store = STORE.get_or_init(|| crate::sandbox::VersionedJsonStore::new());

        store.load_with_healing(
            &msgs_file,
            || Ok(Self::default_dynamic()),
            |m| {
                let default_msgs = Self::default_dynamic();
                let mut existing_val = serde_json::to_value(&*m).unwrap_or(DynamicValue::Null);
                let default_val = serde_json::to_value(&default_msgs).unwrap_or(DynamicValue::Null);
                
                if merge_missing_json_defaults(&mut existing_val, &default_val) {
                    if let Ok(merged) = serde_json::from_value::<SusiMessages>(existing_val) {
                        *m = merged;
                        return true;
                    }
                }
                false
            },
            false
        ).unwrap_or_else(|_| Self::default_dynamic())
    }

    fn default_dynamic() -> Self {
        let categories: HashMap<String, StringRegistry> = serde_json::from_str(include_str!(
            "../../config/messages.default.json"
        ))
        .expect("Fatal: messages.default.json must be valid JSON. Zero hardcoded config allowed.");
        Self { categories }
    }

    pub fn get(&self, category: &str, key: &str) -> Option<&String> {
        self.categories.get(category)?.get(key)
    }
    pub fn get_category(&self, category: &str) -> Option<&StringRegistry> {
        self.categories.get(category)
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
    pub fn get_endpoint(&self, name: &str) -> Option<&str> {
        self.fields.get(name)?.as_str()
    }
}

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

// === 100% DYNAMIC SUSI CONFIG - THE ROOT ===
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SusiConfig {
    #[serde(flatten)]
    pub settings: DynamicRegistry, // ALL settings are dynamic, loaded from config.default.json
}

impl SusiConfig {
    // Intentionally shadows the derived Default trait impl (which yields an
    // empty settings map): this inherent method is the one that loads the
    // bundled config.default.json, and call sites that need those bundled
    // values reach it via `Self::default()`/`SusiConfig::default()` rather
    // than through `Default::default()` trait dispatch.
    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self {
        serde_json::from_str(include_str!("../../config/config.default.json"))
            .expect("Fatal: config.default.json must be valid JSON. Zero hardcoded config allowed.")
    }

    pub fn get_config_path(global_dir: &Path) -> PathBuf {
        global_dir.join("config.json")
    }

    /// Loads a user's persisted config.json and self-heals schema drift against
    /// the binary's bundled config.default.json: any key (at any nesting depth,
    /// including per-element within same-length arrays like model_ladder) that
    /// exists in the bundled default but is missing from the user's file is
    /// backfilled in memory and the merged result is written back to disk.
    /// Values the user already set are never touched. Without this, a fix that
    /// only lands in config.default.json (e.g. a new field on an existing key)
    /// silently never reaches an install whose config.json predates it.
    pub fn load(global_dir: &Path) -> EaiResult<Self> {
        let path = Self::get_config_path(global_dir);
        static STORE: std::sync::OnceLock<crate::sandbox::VersionedJsonStore<SusiConfig>> = std::sync::OnceLock::new();
        let store = STORE.get_or_init(|| crate::sandbox::VersionedJsonStore::new());

        store.load_with_healing(
            &path,
            || Ok(Self::default()),
            |cfg| {
                let default = Self::default();
                merge_missing_registry_defaults(&mut cfg.settings, &default.settings)
            },
            true
        )
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
        Self::load(&home.join(".susi"))
    }

    /// Writes via a same-directory temp file + rename rather than a direct
    /// fs::write (which truncates before writing), so a concurrent reader —
    /// another process's CLI invocation, the daemon's own background cycle —
    /// never observes a torn/empty file. This matters more now that `load()`
    /// can itself trigger a save on schema-drift backfill, making concurrent
    /// writers to the same config.json from multiple processes routine rather
    /// than rare.
    pub fn save(&self, global_dir: &Path) -> EaiResult<()> {
        atomic_write_json_pretty(&Self::get_config_path(global_dir), self)
    }

    // === TYPED ACCESSORS - No hardcoded fields, dynamic getters with defaults ===
    pub fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        let v = self.settings.get(key)?;
        serde_json::from_value(v.clone()).ok()
    }

    /// Falls back to the bundled config.default.json's value for `key` (not a
    /// zero-value literal duplicated in Rust) when a user's ~/.susi/config.json
    /// predates this key or omits it. This is the *only* fallback path for every
    /// accessor below — config.default.json is the single source of truth for
    /// every default; there is no second, Rust-side copy of any value that
    /// could silently drift out of sync with it (as several of these already
    /// had: gmcp_http_port/gemi_port/udp_discovery_port were scrambled between
    /// their Rust literal and config.default.json, max_stdin_size_bytes was off
    /// by 100x, reflex_training_threshold by 10x, and alpha_weights_url/
    /// mcp_registry_url's Rust fallback was an empty string).
    fn get_or_bundled_default<T: for<'de> Deserialize<'de> + Default>(&self, key: &str) -> T {
        self.get(key)
            .unwrap_or_else(|| Self::default().get(key).unwrap_or_default())
    }

    // Backward compat accessors and helpers
    pub fn gmcp_port(&self) -> u16 {
        self.get_or_bundled_default("gmcp_port")
    }
    pub fn gmcp_http_port(&self) -> u16 {
        self.get_or_bundled_default("gmcp_http_port")
    }
    pub fn gemi_port(&self) -> u16 {
        self.get_or_bundled_default("gemi_port")
    }
    pub fn udp_discovery_port(&self) -> u16 {
        self.get_or_bundled_default("udp_discovery_port")
    }
    pub fn execution_lease_secs(&self) -> u64 {
        self.get_or_bundled_default("execution_lease_secs")
    }
    pub fn max_concurrent_agents(&self) -> usize {
        self.get_or_bundled_default("max_concurrent_agents")
    }
    pub fn trust_level(&self) -> String {
        self.get_or_bundled_default("trust_level")
    }
    pub fn max_stdin_size_bytes(&self) -> usize {
        self.get_or_bundled_default("max_stdin_size_bytes")
    }
    pub fn max_rpc_body_bytes(&self) -> usize {
        self.get_or_bundled_default("max_rpc_body_bytes")
    }
    pub fn allow_origin(&self) -> String {
        self.get_or_bundled_default("allow_origin")
    }
    /// Bearer token required on world-facing HTTP surfaces (GMCP HTTP, GEMI
    /// REST). Empty (the bundled default) means auth is not enforced, so
    /// existing local/desktop installs keep working unmodified; operators
    /// exposing susi beyond localhost should set this.
    pub fn api_auth_token(&self) -> String {
        self.get_or_bundled_default("api_auth_token")
    }
    /// Max requests per IP per 60s window on world-facing HTTP surfaces. 0
    /// disables rate limiting.
    pub fn rate_limit_per_minute(&self) -> u32 {
        self.get_or_bundled_default("rate_limit_per_minute")
    }

    pub fn default_model(&self) -> String {
        self.get_or_bundled_default("default_model")
    }
    pub fn default_engine(&self) -> String {
        self.get_or_bundled_default("default_engine")
    }
    pub fn mcp_registry_url(&self) -> String {
        self.get_or_bundled_default("mcp_registry_url")
    }
    pub fn bootstrap_mcp_servers<T: for<'de> Deserialize<'de> + Default>(&self) -> T {
        self.get("bootstrap_mcp_servers").unwrap_or_default()
    }
    pub fn local_scan_paths(&self) -> Vec<String> {
        self.get("local_scan_paths").unwrap_or_default()
    }
    pub fn discoverable_assets<T: for<'de> Deserialize<'de> + Default>(&self) -> T {
        self.get("discoverable_assets").unwrap_or_default()
    }
    pub fn governance(&self) -> GovernancePatterns {
        self.get_or_bundled_default("governance")
    }
    pub fn admin_pulses(&self) -> AdminPulsesConfig {
        self.get_or_bundled_default("admin_pulses")
    }
    pub fn intent_classify(&self) -> IntentClassifyConfig {
        self.get_or_bundled_default("intent_classify")
    }
    pub fn alpha_weights_url(&self) -> String {
        self.get_or_bundled_default("alpha_weights_url")
    }
    pub fn alpha_weights_filename(&self) -> String {
        self.get_or_bundled_default("alpha_weights_filename")
    }
    pub fn tokenizer_filename(&self) -> String {
        self.get_or_bundled_default("tokenizer_filename")
    }
    pub fn hf_base_url(&self) -> String {
        self.get_or_bundled_default("hf_base_url")
    }
    pub fn qdrant_url(&self) -> String {
        self.get_or_bundled_default("qdrant_url")
    }
    pub fn crates_io_api_url(&self) -> String {
        self.get_or_bundled_default("crates_io_api_url")
    }
    pub fn inference_endpoints(&self) -> InferenceEndpointsConfig {
        self.get_or_bundled_default("inference_endpoints")
    }
    pub fn agent_rank_threshold(&self) -> f32 {
        self.get_or_bundled_default("agent_rank_threshold")
    }
    pub fn cloud_scout_timeout_secs(&self) -> u64 {
        self.get_or_bundled_default("cloud_scout_timeout_secs")
    }
    pub fn model_provisioning_wait_secs(&self) -> u64 {
        self.get_or_bundled_default("model_provisioning_wait_secs")
    }
    pub fn reflex_training_threshold(&self) -> usize {
        self.get_or_bundled_default("reflex_training_threshold")
    }
    pub fn model_ladder(&self) -> Vec<ModelLadderConfigStep> {
        self.get_or_bundled_default("model_ladder")
    }
    pub fn default_fallback_model(&self) -> ModelLadderConfigStep {
        self.get_or_bundled_default("default_fallback_model")
    }
    pub fn admin_command_routing(&self) -> HashMap<String, Vec<Vec<String>>> {
        self.get_or_bundled_default("admin_command_routing")
    }
    pub fn agent_routing(&self) -> HashMap<String, Vec<String>> {
        self.get_or_bundled_default("agent_routing")
    }
    pub fn model_scoring_heuristics(&self) -> ModelScoringHeuristics {
        self.get_or_bundled_default("model_scoring_heuristics")
    }
    pub fn memory_experience_heuristics(&self) -> MemoryExperienceHeuristics {
        self.get_or_bundled_default("memory_experience_heuristics")
    }
    pub fn sandbox_image(&self) -> String {
        self.get_or_bundled_default("sandbox_image")
    }
    /// Special-token IDs that terminate generation (previously hardcoded as
    /// `1 | 2 | 32000 | 151643` directly in the inference loop — vendor/
    /// tokenizer-specific magic numbers with zero comment on which model
    /// family each belonged to).
    pub fn eos_token_ids(&self) -> Vec<u32> {
        self.get_or_bundled_default("eos_token_ids")
    }
    pub fn max_generation_tokens(&self) -> usize {
        self.get_or_bundled_default("max_generation_tokens")
    }
    /// Penalty divisor applied to already-seen tokens' logits before argmax
    /// (llama.cpp convention: >1.0 discourages repetition, 1.0 disables it).
    /// Pure greedy decoding with no penalty readily loops on tiny models -
    /// observed live on qwen2.5-0.5b as a "Name three colors" response
    /// degenerating into an infinitely-nesting repeated JSON structure.
    pub fn repeat_penalty(&self) -> f32 {
        self.get_or_bundled_default("repeat_penalty")
    }
    /// How many of the most recent tokens (prompt + generated) count toward
    /// the repeat penalty above.
    pub fn repeat_last_n(&self) -> usize {
        self.get_or_bundled_default("repeat_last_n")
    }
    /// Whether the generation loop may draft-and-verify multiple tokens per
    /// target-model forward pass (see `gemi::speculative`) instead of one
    /// token at a time. Verification always falls back to the target
    /// model's own greedy choice on the first disagreement, so the emitted
    /// token sequence is provably identical to plain greedy decoding
    /// either way (see `speculative::tests::
    /// test_speculative_output_matches_plain_greedy_decoding`) - this only
    /// gates whether the batched path is attempted, never behavior.
    ///
    /// Defaults to `false`: measured live on this host (RTX 2000 Ada
    /// Laptop, 8GB VRAM), a fully GPU-resident Qwen2.5-7B target with a
    /// 0.5B draft ran at 4.36 tok/s versus 18.47 tok/s for plain greedy
    /// decoding of the same model - a 4x regression, not the hoped-for
    /// speedup. CUDA's quantized matmul genuinely gets cheaper per-token
    /// with a wider batch (confirmed via `cudarc`'s `fast_mmq` threshold),
    /// but that saving is consumed by the extra host/kernel-launch round
    /// trips this adds: the draft model still needs `speculative_draft_tokens
    /// - 1` *sequential* single-token forwards per round (autoregressive,
    ///   can't be batched), plus a resync forward, on top of the target's
    ///   batched verify call - more total round trips than the classic loop
    ///   for the same tokens, and per-call host/launch overhead dominates
    ///   over raw compute at these model sizes on this stack. Left
    ///   configurable (and the implementation fully correctness-tested) in
    ///   case a future candle version, different hardware, or a larger
    ///   draft_chunk changes this trade-off - but never default-on without
    ///   remeasuring.
    pub fn speculative_decoding_enabled(&self) -> bool {
        self.get_or_bundled_default("speculative_decoding_enabled")
    }
    /// How many tokens the draft model proposes ahead of the target model
    /// per verification round. Larger values amortize more work into each
    /// batched target-model forward pass (raising GPU utilization) but waste
    /// more of that pass whenever the draft diverges early.
    pub fn speculative_draft_tokens(&self) -> usize {
        self.get_or_bundled_default("speculative_draft_tokens")
    }
    /// Upper bound (prompt + generated tokens combined) the native Qwen2
    /// engine's KV cache preallocates per layer at model-load time (see
    /// `qwen2_split::ModelWeights::from_gguf_split`). The cache is written
    /// into in place as generation proceeds rather than reallocated every
    /// token, so this must be decided once, before any particular
    /// request's prompt length is known - sized generously above
    /// `max_generation_tokens` to comfortably cover realistic prompts.
    /// Exceeding it mid-generation is a hard error (not silent truncation
    /// or a fallback to reallocation): raise this value if it's ever hit.
    pub fn kv_cache_capacity_tokens(&self) -> usize {
        self.get_or_bundled_default("kv_cache_capacity_tokens")
    }
    /// Risk substrings that block reasoning OUTPUT before it's returned as a
    /// final answer — a distinct security layer from `governance()`'s
    /// `destructive_commands` (which gates COMMANDS before execution); the
    /// two lists overlap in spirit but not in content.
    pub fn axiomatic_risk_patterns(&self) -> Vec<String> {
        self.get_or_bundled_default("axiomatic_risk_patterns")
    }
    pub fn model_scan_exclude_dirs(&self) -> Vec<String> {
        self.get_or_bundled_default("model_scan_exclude_dirs")
    }
    /// Deliberately narrower than `model_scan_exclude_dirs`: this gates
    /// `recursive_scan_model_dir_for_paths`, which `deep_scan_home_and_register`
    /// invokes *with* `.cache`/`.local` etc. as starting directories (they're
    /// dot-prefixed, so a separate first pass has to seed them explicitly) — if
    /// this list excluded them too, the scan would refuse to look inside its
    /// own seeded roots.
    pub fn model_discovery_exclude_dirs(&self) -> Vec<String> {
        self.get_or_bundled_default("model_discovery_exclude_dirs")
    }
    pub fn home_scan_root_exclude_dirs(&self) -> Vec<String> {
        self.get_or_bundled_default("home_scan_root_exclude_dirs")
    }
    pub fn model_file_extensions(&self) -> Vec<String> {
        self.get_or_bundled_default("model_file_extensions")
    }
    pub fn model_file_min_bytes(&self) -> u64 {
        self.get_or_bundled_default("model_file_min_bytes")
    }
}

// Compatibility shim for old code that accessed fields directly
impl std::ops::Deref for SusiConfig {
    type Target = DynamicRegistry;
    fn deref(&self) -> &Self::Target {
        &self.settings
    }
}

// === SANDBOX MANAGER ===
pub struct SandboxManager;

impl SandboxManager {
    pub async fn execute_in_docker(cmd: &str) -> EaiResult<String> {
        use bollard::container::LogOutput;
        use bollard::models::ContainerCreateBody;
        use bollard::query_parameters::{
            CreateContainerOptions, LogsOptions, StartContainerOptions,
        };
        use bollard::Docker;
        use futures::stream::StreamExt;

        let docker = Docker::connect_with_local_defaults()
            .map_err(|e| EaiError::process(format!("Docker connection failed: {}", e)))?;

        let sandbox_image = SusiConfig::load_global()
            .unwrap_or_default()
            .sandbox_image();
        let config = ContainerCreateBody {
            image: Some(sandbox_image),
            cmd: Some(vec!["sh".to_string(), "-c".to_string(), cmd.to_string()]),
            ..Default::default()
        };

        let container = docker
            .create_container(None::<CreateContainerOptions>, config)
            .await
            .map_err(|e| EaiError::process(format!("Container creation failed: {}", e)))?;

        docker
            .start_container(&container.id, None::<StartContainerOptions>)
            .await
            .map_err(|e| EaiError::process(format!("Container start failed: {}", e)))?;

        let mut logs = docker.logs(&container.id, None::<LogsOptions>);
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
            let cfg = if default_cfg_file.is_file() {
                fs::read_to_string(default_cfg_file)
                    .ok()
                    .and_then(|c| serde_json::from_str::<SusiConfig>(&c).ok())
                    .unwrap_or_default()
            } else {
                SusiConfig::default()
            };
            let json = serde_json::to_string_pretty(&cfg).unwrap_or_else(|_| "{}".to_string());
            fs::write(config_path, json).map_err(|e| EaiError::filesystem(e.to_string()))?;
        }
        Ok(())
    }

    pub fn save_mission_checkpoint(workspace: &Path, checkpoint: &NeuralCheckpoint) {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let _ = fs::write(
            susi_dir.join("mission_checkpoint.json"),
            serde_json::to_string_pretty(checkpoint).unwrap_or_default(),
        );
    }

    pub fn check_interrupted_checkpoint(workspace: &Path) -> Option<NeuralCheckpoint> {
        let p = workspace.join(".susi/mission_checkpoint.json");
        if p.is_file() {
            fs::read_to_string(&p)
                .ok()
                .and_then(|c| serde_json::from_str(&c).ok())
        } else {
            None
        }
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

        // Deterministic credential masking (Mandate 10: No Secret Leaks) — every
        // telemetry write funnels through here, so this is the one chokepoint
        // that guarantees secrets never reach the persistent audit trail.
        let details = crate::gawd::security::SecurityDetector::redact(details);
        let details = details.as_str();

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
    fn bundles_path(workspace: &Path) -> PathBuf {
        workspace.join(".susi/staged_bundles.json")
    }

    pub fn get_staged_bundles(workspace: &Path) -> Vec<IntentBundle> {
        let p = Self::bundles_path(workspace);
        if p.is_file() {
            fs::read_to_string(&p)
                .ok()
                .and_then(|c| serde_json::from_str(&c).ok())
                .unwrap_or_default()
        } else {
            vec![]
        }
    }

    fn save_staged_bundles(workspace: &Path, bundles: &[IntentBundle]) -> EaiResult<()> {
        let susi_dir = workspace.join(".susi");
        let _ = fs::create_dir_all(&susi_dir);
        let json = serde_json::to_string_pretty(bundles)
            .map_err(|e| EaiError::filesystem(e.to_string()))?;
        fs::write(Self::bundles_path(workspace), json)
            .map_err(|e| EaiError::filesystem(e.to_string()))
    }

    pub fn stage_bundle(workspace: &Path, bundle: IntentBundle) -> EaiResult<()> {
        let mut bundles = Self::get_staged_bundles(workspace);
        bundles.retain(|b| b.bundle_id() != bundle.bundle_id());
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
            if !bundle.is_applied() {
                for fix in &bundle.staged_fixes {
                    let target_path = workspace.join(fix.file_path());
                    if let Some(parent) = target_path.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let _ = fs::write(&target_path, fix.staged_content());
                    files_changed += 1;
                }
                bundle.set_applied(true);
                accepted_count += 1;
            }
        }
        Self::save_staged_bundles(workspace, &bundles)?;
        Ok(format!(
            "SUCCESS: Accepted {} bundles across {} files.",
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
        Ok(format!(
            "SUCCESS: Rolled back staged fixes across {} files.",
            reverted_files
        ))
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
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&memory_file)
        {
            let _ = writeln!(f, "{}", entry);
        }

        let heuristics = SusiConfig::load_global()
            .unwrap_or_default()
            .memory_experience_heuristics();
        let has_failure_marker = heuristics
            .failure_markers
            .iter()
            .any(|marker| output.contains(marker.as_str()));
        if output.len() > heuristics.min_output_len && !has_failure_marker {
            let exp_file = susi_dir.join("reasoning_experience.jsonl");
            let exp_entry = serde_json::json!({
                "intent": input,
                "blackboard_context": "converged",
                "successful_outcome": output,
                "timestamp": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
                "validation": "STRICT_SEMANTIC_PASS"
            });
            if let Ok(mut f) = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(exp_file)
            {
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

    /// Every scalar accessor's only fallback is config.default.json itself
    /// (via get_or_bundled_default) — there is no second, Rust-literal copy
    /// of any default that could drift out of sync with it. This was not
    /// previously true: gmcp_http_port/gemi_port/udp_discovery_port's Rust
    /// literals were scrambled relative to config.default.json,
    /// max_stdin_size_bytes was off by 100x, reflex_training_threshold by
    /// 10x, and alpha_weights_url/mcp_registry_url's Rust fallback was an
    /// empty string — all invisible in practice because the fallback path
    /// only fires when config.default.json itself is missing a key, which
    /// normal operation never hits. Asserting against config.default.json's
    /// actual values (not against a second hand-copied literal in this test)
    /// is what would have caught that drift.
    #[test]
    fn test_config_accessors_match_bundled_default_single_source_of_truth() {
        let default = SusiConfig::default();
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../../config/config.default.json")).unwrap();

        assert_eq!(
            default.gmcp_port(),
            raw["gmcp_port"].as_u64().unwrap() as u16
        );
        assert_eq!(
            default.gmcp_http_port(),
            raw["gmcp_http_port"].as_u64().unwrap() as u16
        );
        assert_eq!(
            default.gemi_port(),
            raw["gemi_port"].as_u64().unwrap() as u16
        );
        assert_eq!(
            default.udp_discovery_port(),
            raw["udp_discovery_port"].as_u64().unwrap() as u16
        );
        assert_eq!(default.trust_level(), raw["trust_level"].as_str().unwrap());
        assert_eq!(
            default.max_stdin_size_bytes(),
            raw["max_stdin_size_bytes"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.max_rpc_body_bytes(),
            raw["max_rpc_body_bytes"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.allow_origin(),
            raw["allow_origin"].as_str().unwrap()
        );
        assert_eq!(
            default.mcp_registry_url(),
            raw["mcp_registry_url"].as_str().unwrap()
        );
        assert_eq!(
            default.alpha_weights_url(),
            raw["alpha_weights_url"].as_str().unwrap()
        );
        assert_eq!(
            default.agent_rank_threshold(),
            raw["agent_rank_threshold"].as_f64().unwrap() as f32
        );
        assert_eq!(
            default.cloud_scout_timeout_secs(),
            raw["cloud_scout_timeout_secs"].as_u64().unwrap()
        );
        assert_eq!(
            default.model_provisioning_wait_secs(),
            raw["model_provisioning_wait_secs"].as_u64().unwrap()
        );
        assert_eq!(
            default.reflex_training_threshold(),
            raw["reflex_training_threshold"].as_u64().unwrap() as usize
        );
        assert_eq!(default.qdrant_url(), raw["qdrant_url"].as_str().unwrap());
        assert_eq!(
            default.crates_io_api_url(),
            raw["crates_io_api_url"].as_str().unwrap()
        );
        assert_eq!(
            default.sandbox_image(),
            raw["sandbox_image"].as_str().unwrap()
        );

        let heuristics = default.memory_experience_heuristics();
        assert_eq!(
            heuristics.min_output_len,
            raw["memory_experience_heuristics"]["min_output_len"]
                .as_u64()
                .unwrap() as usize
        );
        assert!(!heuristics.failure_markers.is_empty());

        assert_eq!(
            default.max_generation_tokens(),
            raw["max_generation_tokens"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.repeat_penalty(),
            raw["repeat_penalty"].as_f64().unwrap() as f32
        );
        assert_eq!(
            default.repeat_last_n(),
            raw["repeat_last_n"].as_u64().unwrap() as usize
        );
        assert_eq!(
            default.eos_token_ids(),
            raw["eos_token_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u32)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            default.axiomatic_risk_patterns().len(),
            raw["axiomatic_risk_patterns"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_scan_exclude_dirs().len(),
            raw["model_scan_exclude_dirs"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_discovery_exclude_dirs().len(),
            raw["model_discovery_exclude_dirs"]
                .as_array()
                .unwrap()
                .len()
        );
        assert_eq!(
            default.home_scan_root_exclude_dirs().len(),
            raw["home_scan_root_exclude_dirs"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_file_extensions().len(),
            raw["model_file_extensions"].as_array().unwrap().len()
        );
        assert_eq!(
            default.model_file_min_bytes(),
            raw["model_file_min_bytes"].as_u64().unwrap()
        );

        // model_discovery_exclude_dirs (used by deep_scan_home_and_register,
        // which scans *inside* .cache/.local/.android as seeded roots) must
        // never exclude those two dir names, unlike the broader
        // model_scan_exclude_dirs — this is the exact invariant that keeps
        // JetBrains/ProxyAI model discovery under ~/.cache working.
        assert!(!default
            .model_discovery_exclude_dirs()
            .iter()
            .any(|d| d == ".cache" || d == "Library"));
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
        cfg.settings
            .insert("execution_lease_secs".to_string(), serde_json::json!(45));
        cfg.settings
            .insert("max_concurrent_agents".to_string(), serde_json::json!(64));
        cfg.save(dir).expect("Failed to save updated config");

        let reloaded = SusiConfig::reload(dir).expect("Failed to reload config");
        assert_eq!(reloaded.execution_lease_secs(), 45);
        assert_eq!(reloaded.max_concurrent_agents(), 64);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn test_susi_config_backfills_schema_drift_without_clobbering_user_values() {
        let dir = Path::new("test_cfg_migration");
        let _ = fs::create_dir_all(dir);

        // Simulate a pre-existing user config.json that predates a new field
        // (tokenizer_repo) added to one ladder step in config.default.json,
        // and that has customized an unrelated existing value.
        let stale = serde_json::json!({
            "gmcp_port": 12345,
            "model_ladder": [
                {
                    "hf_file": "qwen2.5-1.5b-instruct-q4_k_m.gguf",
                    "hf_repo": "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
                    "label": "1.5B Parameters (Fast Local Edge)",
                    "min_ram_gb": 0,
                    "step": 1
                }
            ]
        });
        fs::write(
            SusiConfig::get_config_path(dir),
            serde_json::to_string_pretty(&stale).unwrap(),
        )
        .unwrap();

        let cfg = SusiConfig::load(dir).expect("Failed to load stale config");

        // User's customized scalar value survives the merge untouched.
        assert_eq!(cfg.gmcp_port(), 12345);

        // The bundled default's full model_ladder (5 steps, all with
        // tokenizer_repo) was backfilled since the user's array only had 1
        // element (length mismatch means no positional merge is attempted and
        // the top-level key is left as the user's own value)... but a key the
        // user never set at all (default_fallback_model) must be pulled in
        // wholesale from the bundled default.
        let fallback = cfg.default_fallback_model();
        assert!(!fallback.tokenizer_repo.is_empty());

        // Same-length-array positional backfill: drop the tokenizer_repo field
        // from one element of a same-length ladder and confirm it gets healed.
        let mut same_len = SusiConfig::default().settings;
        if let Some(serde_json::Value::Array(steps)) = same_len.get_mut("model_ladder") {
            if let Some(serde_json::Value::Object(step0)) = steps.get_mut(0) {
                step0.remove("tokenizer_repo");
            }
        }
        let healed = SusiConfig { settings: same_len };
        healed
            .save(dir)
            .expect("Failed to save same-length stale config");
        let reloaded = SusiConfig::load(dir).expect("Failed to load same-length stale config");
        let ladder = reloaded.model_ladder();
        assert_eq!(ladder.len(), 5);
        assert!(
            !ladder[0].tokenizer_repo.is_empty(),
            "missing tokenizer_repo on an existing array element must be backfilled by position"
        );

        // The merge must have persisted back to disk (self-healing).
        let on_disk = fs::read_to_string(SusiConfig::get_config_path(dir)).unwrap();
        assert!(on_disk.contains("tokenizer_repo"));

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

    /// The experience-promotion threshold (min_output_len, failure_markers)
    /// is config-driven now, not a hardcoded `output.len() > 50` literal —
    /// prove the configured values actually gate behavior, not just that the
    /// accessor returns the right number.
    #[test]
    fn test_susi_memory_experience_promotion_respects_config_heuristics() {
        let ws = Path::new("test_mem_experience");
        let _ = fs::create_dir_all(ws);
        let exp_file = ws.join(".susi/reasoning_experience.jsonl");

        let heuristics = SusiConfig::default().memory_experience_heuristics();
        let short_output = "x".repeat(heuristics.min_output_len); // exactly at threshold: not > min_output_len
        SusiMemory::save_interaction(ws, "goal a", &short_output);
        assert!(
            !exp_file.exists(),
            "output at, not over, the threshold must not be promoted"
        );

        let long_output = "x".repeat(heuristics.min_output_len + 1);
        SusiMemory::save_interaction(ws, "goal b", &long_output);
        assert!(
            exp_file.is_file(),
            "output over the threshold must be promoted"
        );

        let with_marker = format!("{} {}", heuristics.failure_markers[0], long_output);
        let before = fs::read_to_string(&exp_file).unwrap();
        SusiMemory::save_interaction(ws, "goal c", &with_marker);
        let after = fs::read_to_string(&exp_file).unwrap();
        assert_eq!(
            before, after,
            "a configured failure marker must suppress promotion"
        );

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
        fix_fields.insert(
            "original_content".to_string(),
            serde_json::json!("original code"),
        );
        fix_fields.insert(
            "staged_content".to_string(),
            serde_json::json!("refactored code"),
        );

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
    fn test_backfill_missing_prompt_keys_adds_new_key_without_touching_existing() {
        // Regression: a prompts.json written before dynamic_agent_prompt (or
        // any future key) existed would otherwise permanently lack it, since
        // load_global() only writes prompts.json once, on first-ever load.
        let mut existing: DynamicRegistry = HashMap::new();
        existing.insert(
            "consensus_wisdom_prompt".to_string(),
            DynamicValue::String("old customized text".to_string()),
        );

        let mut defaults: DynamicRegistry = HashMap::new();
        defaults.insert(
            "consensus_wisdom_prompt".to_string(),
            DynamicValue::String("new default text".to_string()),
        );
        defaults.insert(
            "dynamic_agent_prompt".to_string(),
            DynamicValue::String("direct-answer prompt".to_string()),
        );

        let changed = merge_missing_registry_defaults(&mut existing, &defaults);

        assert!(changed);
        assert_eq!(
            existing
                .get("consensus_wisdom_prompt")
                .and_then(|v| v.as_str()),
            Some("old customized text"),
            "an existing key must never be overwritten by a newer default"
        );
        assert_eq!(
            existing
                .get("dynamic_agent_prompt")
                .and_then(|v| v.as_str()),
            Some("direct-answer prompt"),
            "a key missing from the user's file must be backfilled from the bundled default"
        );
    }

    #[test]
    fn test_backfill_missing_prompt_keys_reports_no_change_when_nothing_missing() {
        let mut existing: DynamicRegistry = HashMap::new();
        existing.insert(
            "consensus_wisdom_prompt".to_string(),
            DynamicValue::String("text".to_string()),
        );
        let defaults = existing.clone();

        assert!(!merge_missing_registry_defaults(&mut existing, &defaults));
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
        cfg.templates.insert(
            "deepseek".to_string(),
            "### System:\n{system}\n\n### User:\n{prompt}\n\n### Assistant:\n".to_string(),
        );
        cfg.templates.insert(
            "mistral".to_string(),
            "[INST] {system} {prompt} [/INST]".to_string(),
        );
        cfg.templates.insert(
            "phi".to_string(),
            "<|system|>\n{system}<|end|>\n<|user|>\n{prompt}<|end|>\n<|assistant|>".to_string(),
        );

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
