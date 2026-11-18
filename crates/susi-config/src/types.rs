//! Typed config fragments stored inside `SusiConfig` / catalogs.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::json_util::{
    merge_missing_json_defaults, merge_missing_registry_defaults, DynamicRegistry, DynamicValue,
    StringRegistry,
};

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
    #[allow(clippy::expect_used)]
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

        static STORE: std::sync::OnceLock<super::versioned_store::VersionedJsonStore<SusiPrompts>> =
            std::sync::OnceLock::new();
        let store = STORE.get_or_init(super::versioned_store::VersionedJsonStore::new);

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

    #[allow(clippy::expect_used)]
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
            super::versioned_store::VersionedJsonStore<SusiMessages>,
        > = std::sync::OnceLock::new();
        let store = STORE.get_or_init(super::versioned_store::VersionedJsonStore::new);

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

    #[allow(clippy::expect_used)]
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

impl InferenceEndpointItem {
    /// Resolve this endpoint's credential at use time through the scoped
    /// key path — `api_key_env` names a `key://` ref or bare `ENV_NAME`,
    /// never the secret body. `Ok(None)` means no credential configured.
    pub fn resolve_api_key(
        &self,
        global_dir: &Path,
    ) -> crate::susi_error::EaiResult<Option<crate::key_scope::ScopedSecret>> {
        crate::credential::resolve_api_key_env(&self.api_key_env, global_dir)
    }
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

/// Edge privacy controls: local-only inference and egress consent.
/// `mode`: `balanced` | `local_only` | `host_only` (this machine alone) | `open`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PrivacyConfig {
    pub mode: String,
    /// When true (or mode is local_only or host_only), host `exec_command` requires
    /// `process.exec` grant; prefer Docker `sandbox_exec`.
    pub mandatory_sandbox_for_exec: bool,
    /// When true (default), `network.egress` needs an explicit capability grant
    /// or `susi privacy consent --egress`.
    pub require_egress_consent: bool,
}

impl Default for PrivacyConfig {
    fn default() -> Self {
        Self {
            mode: "balanced".into(),
            mandatory_sandbox_for_exec: false,
            require_egress_consent: true,
        }
    }
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

impl ExternalPeerAgentSpec {
    /// Resolve this peer's credential at use time through the scoped key
    /// path — `api_key_env` names a `key://` ref or bare `ENV_NAME`, never
    /// the secret body. `Ok(None)` means no credential configured.
    pub fn resolve_api_key(
        &self,
        global_dir: &Path,
    ) -> crate::susi_error::EaiResult<Option<crate::key_scope::ScopedSecret>> {
        match &self.api_key_env {
            Some(name) => crate::credential::resolve_api_key_env(name, global_dir),
            None => Ok(None),
        }
    }
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

impl ModelCatalogEntry {
    /// Resolve this entry's credential at use time through the scoped key
    /// path — `api_key_env` names a `key://` ref or bare `ENV_NAME`, never
    /// the secret body. `Ok(None)` means no credential configured.
    pub fn resolve_api_key(
        &self,
        global_dir: &Path,
    ) -> crate::susi_error::EaiResult<Option<crate::key_scope::ScopedSecret>> {
        crate::credential::resolve_api_key_env(&self.api_key_env, global_dir)
    }
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
            ask_when_multiple_clouds: false,
        }
    }
}

// === 100% DYNAMIC SUSI CONFIG - THE ROOT ===

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn model(fields: serde_json::Value) -> DynamicModelInfo {
        serde_json::from_value(fields).unwrap()
    }

    fn scratch_file(tag: &str, content: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("susi-types-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("templates.json");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn dynamic_model_info_new_exposes_every_typed_field() {
        let info = DynamicModelInfo::new(
            "Qwen".into(),
            "hf".into(),
            "qwen-7b".into(),
            "a model".into(),
            true,
            "Deep".into(),
            Some(250),
            "local".into(),
            Some("sha256:abc".into()),
            Some(json!({"source": "test"})),
        );
        assert_eq!(info.name(), "Qwen");
        assert_eq!(info.model_id(), "qwen-7b");
        assert_eq!(info.registry(), "hf");
        assert_eq!(info.description(), "a model");
        assert!(info.is_local());
        assert_eq!(info.tier(), "Deep");
        assert_eq!(info.latency_ms(), Some(250));
        assert_eq!(info.provider(), "local");
        assert_eq!(info.checksum(), Some("sha256:abc"));
        assert_eq!(info.provenance(), Some(&json!({"source": "test"})));
    }

    #[test]
    fn dynamic_model_info_omits_absent_optional_fields() {
        let info = DynamicModelInfo::new(
            "n".into(),
            "r".into(),
            "m".into(),
            "d".into(),
            false,
            "t".into(),
            None,
            "p".into(),
            None,
            None,
        );
        assert_eq!(info.latency_ms(), None);
        assert_eq!(info.checksum(), None);
        assert_eq!(info.provenance(), None);
        for key in ["latency_ms", "checksum", "provenance"] {
            assert!(info.get(key).is_none(), "{key} must not be serialized");
        }
        assert!(!info.is_local());
    }

    #[test]
    fn dynamic_model_info_defaults_when_fields_are_missing() {
        let empty = DynamicModelInfo::default();
        assert_eq!(empty.name(), "unknown");
        assert_eq!(empty.model_id(), "unknown");
        assert_eq!(empty.provider(), "unknown");
        assert_eq!(empty.registry(), "SUSI Substrate");
        assert_eq!(empty.description(), "");
        assert_eq!(empty.tier(), "Reflex");
        assert!(!empty.is_local());
    }

    #[test]
    fn model_name_and_id_follow_their_fallback_chains() {
        // name: name -> model_id -> "unknown"
        assert_eq!(model(json!({"name": "n", "model_id": "m"})).name(), "n");
        assert_eq!(model(json!({"model_id": "m"})).name(), "m");
        assert_eq!(model(json!({"id": "i"})).name(), "unknown");
        // model_id: model_id -> id -> name -> "unknown"
        assert_eq!(
            model(json!({"model_id": "m", "id": "i", "name": "n"})).model_id(),
            "m"
        );
        assert_eq!(model(json!({"id": "i", "name": "n"})).model_id(), "i");
        assert_eq!(model(json!({"name": "n"})).model_id(), "n");
        assert_eq!(model(json!({})).model_id(), "unknown");
    }

    #[test]
    fn model_getters_ignore_values_of_the_wrong_type() {
        let info = model(json!({
            "name": 7,
            "is_local": "yes",
            "latency_ms": -5,
            "tier": ["x"]
        }));
        assert_eq!(info.name(), "unknown");
        assert!(!info.is_local());
        assert_eq!(info.latency_ms(), None);
        assert_eq!(info.tier(), "Reflex");
        assert_eq!(model(json!({"latency_ms": 1.5})).latency_ms(), None);
        assert_eq!(model(json!({"latency_ms": 40})).latency_ms(), Some(40));
    }

    #[test]
    fn dynamic_model_info_round_trips_unknown_fields_through_serde() {
        let info = model(json!({"name": "n", "custom_extension": {"k": 1}}));
        assert_eq!(info.get("custom_extension"), Some(&json!({"k": 1})));
        let back = serde_json::to_value(&info).unwrap();
        assert_eq!(back["custom_extension"], json!({"k": 1}));
        assert_eq!(back["name"], json!("n"));
    }

    #[test]
    fn neural_checkpoint_getters_default_and_read_typed_values() {
        let empty = DynamicNeuralCheckpoint::default();
        assert_eq!(empty.intent(), "");
        assert_eq!(empty.timestamp(), 0);
        assert!(empty.completed_tools().is_empty());
        assert_eq!(empty.status(), "IN_PROGRESS");

        let filled: NeuralCheckpoint = serde_json::from_value(json!({
            "intent": "ship",
            "timestamp": 1_700_000_000u64,
            "completed_tools": ["scan", "build"],
            "status": "DONE"
        }))
        .unwrap();
        assert_eq!(filled.intent(), "ship");
        assert_eq!(filled.timestamp(), 1_700_000_000);
        assert_eq!(filled.completed_tools(), ["scan", "build"]);
        assert_eq!(filled.status(), "DONE");

        let malformed: DynamicNeuralCheckpoint = serde_json::from_value(json!({
            "intent": 5,
            "timestamp": "soon",
            "completed_tools": "not-a-list",
            "status": null
        }))
        .unwrap();
        assert_eq!(malformed.intent(), "");
        assert_eq!(malformed.timestamp(), 0);
        assert!(malformed.completed_tools().is_empty());
        assert_eq!(malformed.status(), "IN_PROGRESS");
    }

    #[test]
    fn default_chat_templates_carry_the_system_and_prompt_placeholders() {
        let defaults = ChatTemplateConfig::default();
        assert!(defaults.templates.contains_key("chatml"));
        for (name, template) in &defaults.templates {
            assert!(template.contains("{prompt}"), "{name} lacks {{prompt}}");
        }
    }

    fn templates(pairs: &[(&str, &str)]) -> ChatTemplateConfig {
        ChatTemplateConfig {
            templates: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn chat_template_lookup_prefers_exact_then_normalized_fuzzy_then_fallbacks() {
        let cfg = templates(&[("llama3", "L3"), ("chatml", "CM"), ("phi3", "P3")]);
        assert_eq!(cfg.get("phi3").map(String::as_str), Some("P3"));
        // Case, dashes, underscores and dots are ignored; containment either way.
        assert_eq!(cfg.get("Llama-3.1-8B").map(String::as_str), Some("L3"));
        assert_eq!(cfg.get("PHI_3.mini").map(String::as_str), Some("P3"));
        // Unknown family falls back to chatml, else to any registered template.
        assert_eq!(cfg.get("totally-new").map(String::as_str), Some("CM"));
        let no_chatml = templates(&[("only", "O")]);
        assert_eq!(no_chatml.get("totally-new").map(String::as_str), Some("O"));
        assert!(templates(&[]).get("anything").is_none());
    }

    #[test]
    fn chat_template_render_substitutes_every_supplied_variable() {
        let cfg = templates(&[("chatml", "[{system}] {prompt} / {unset}")]);
        let vars = HashMap::from([
            ("system".to_string(), "S".to_string()),
            ("prompt".to_string(), "P".to_string()),
        ]);
        assert_eq!(cfg.render("chatml", &vars), "[S] P / {unset}");
        assert_eq!(
            cfg.render("chatml", &HashMap::new()),
            "[{system}] {prompt} / {unset}"
        );
    }

    #[test]
    fn chat_template_render_without_any_template_uses_the_plain_layout() {
        let empty = templates(&[]);
        let vars = HashMap::from([
            ("system".to_string(), "S".to_string()),
            ("prompt".to_string(), "P".to_string()),
        ]);
        assert_eq!(empty.render("any", &vars), "S\nP");
    }

    #[test]
    fn chat_template_register_adds_and_overrides() {
        let mut cfg = templates(&[]);
        cfg.register("custom".into(), "v1 {prompt}".into());
        assert_eq!(cfg.get("custom").map(String::as_str), Some("v1 {prompt}"));
        cfg.register("custom".into(), "v2 {prompt}".into());
        assert_eq!(cfg.templates.len(), 1);
        assert_eq!(cfg.get("custom").map(String::as_str), Some("v2 {prompt}"));
    }

    #[test]
    fn chat_template_from_file_reads_maps_and_falls_back_to_defaults() {
        let good = scratch_file("tpl-good", r#"{"mine": "M {prompt}"}"#);
        let cfg = ChatTemplateConfig::from_file(good.to_str().unwrap());
        assert_eq!(cfg.get("mine").map(String::as_str), Some("M {prompt}"));

        let defaults = ChatTemplateConfig::default().templates;
        for (tag, content) in [
            ("tpl-bad-json", "{not json"),
            ("tpl-bad-shape", r#"{"mine": 5}"#),
        ] {
            let path = scratch_file(tag, content);
            let cfg = ChatTemplateConfig::from_file(path.to_str().unwrap());
            assert_eq!(cfg.templates, defaults, "{tag} must fall back");
        }
        let missing = good.with_file_name("absent.json");
        assert_eq!(
            ChatTemplateConfig::from_file(missing.to_str().unwrap()).templates,
            defaults
        );
    }

    #[test]
    fn susi_prompts_default_dynamic_ships_prompts_and_templates() {
        let prompts = SusiPrompts::default_dynamic();
        assert!(prompts.get("system_identity").is_some());
        assert!(prompts.get_str("system_identity").is_some());
        assert!(prompts.get("no-such-prompt").is_none());
        assert!(!prompts.agent_factory_prompt().is_empty());
        assert!(prompts.chat_templates.templates.contains_key("chatml"));
    }

    #[test]
    fn susi_prompt_getters_return_empty_strings_when_unset() {
        let none = SusiPrompts::default();
        assert_eq!(none.agent_factory_prompt(), "");
        assert_eq!(none.consensus_wisdom_prompt(), "");
        assert_eq!(none.intent_planner_prompt(), "");
        assert_eq!(none.mission_partition_prompt(), "");
        assert_eq!(none.dynamic_agent_prompt(), "");
        assert_eq!(none.mission_refine_prompt(), "");
    }

    #[test]
    fn susi_prompts_format_chat_and_dynamic_prompts_through_the_templates() {
        let mut prompts = SusiPrompts::default_dynamic();
        let chat = prompts.format_chat_prompt("chatml", "SYS-TEXT", "USER-TEXT");
        assert!(chat.contains("SYS-TEXT") && chat.contains("USER-TEXT"));
        assert!(!chat.contains("{system}") && !chat.contains("{prompt}"));

        prompts
            .chat_templates
            .register("custom".into(), "Hello {name}, welcome to {place}!".into());
        let vars = HashMap::from([
            ("name".to_string(), "Ada".to_string()),
            ("place".to_string(), "SUSI".to_string()),
        ]);
        assert_eq!(
            prompts.format_dynamic("custom", vars),
            "Hello Ada, welcome to SUSI!"
        );
    }

    #[test]
    fn susi_messages_default_dynamic_ships_categories() {
        let messages = SusiMessages::default_dynamic();
        assert!(!messages.categories.is_empty());
        assert!(messages.get("daemon", "signal_received").is_some());
        assert!(messages.get("daemon", "no-such-key").is_none());
        assert!(messages
            .get("no-such-category", "signal_received")
            .is_none());
    }

    #[test]
    fn susi_messages_register_creates_categories_and_overrides_keys() {
        let mut messages = SusiMessages::default();
        assert!(messages.get("cat", "key").is_none());
        messages.register("cat".into(), "key".into(), "first".into());
        messages.register("cat".into(), "other".into(), "second".into());
        messages.register("cat".into(), "key".into(), "replaced".into());
        assert_eq!(
            messages.get("cat", "key").map(String::as_str),
            Some("replaced")
        );
        assert_eq!(
            messages.get("cat", "other").map(String::as_str),
            Some("second")
        );
        let round: SusiMessages =
            serde_json::from_value(serde_json::to_value(&messages).unwrap()).unwrap();
        assert_eq!(
            round.get("cat", "key").map(String::as_str),
            Some("replaced")
        );
    }

    #[test]
    fn typed_defaults_and_partial_deserialization_keep_documented_values() {
        let privacy = PrivacyConfig::default();
        assert_eq!(privacy.mode, "balanced");
        assert!(!privacy.mandatory_sandbox_for_exec);
        assert!(privacy.require_egress_consent);
        let partial: PrivacyConfig = serde_json::from_value(json!({"mode": "local_only"})).unwrap();
        assert_eq!(partial.mode, "local_only");
        assert!(partial.require_egress_consent, "unset fields keep defaults");

        let routing = InferenceRoutingConfig::default();
        assert_eq!(routing.policy, "auto");
        assert_eq!(routing.local_max_latency_ms, 15_000);
        assert!(routing.prefer_cloud_when_cpu_only && !routing.ask_when_multiple_clouds);
        let tuned: InferenceRoutingConfig =
            serde_json::from_value(json!({"policy": "local_only"})).unwrap();
        assert_eq!(tuned.policy, "local_only");
        assert_eq!(tuned.local_min_tokens_per_sec, 8.0);

        let entry: ModelCatalogEntry = serde_json::from_value(json!({"id": "gpt"})).unwrap();
        assert_eq!(entry.id, "gpt");
        assert_eq!(entry.engine, "");
        let peer = ExternalPeerAgentSpec::default();
        assert_eq!(peer.protocol, "");
        assert!(peer.detect_bins.is_empty());
    }

    #[test]
    fn governance_pattern_accessors_return_independent_copies() {
        let mut patterns = GovernancePatterns {
            secret_tokens: vec!["sk-".into()],
            destructive_commands: vec!["rm -rf".into()],
            critical_system_paths: vec!["/etc".into()],
            exfiltration_vectors: vec!["curl".into()],
            ..GovernancePatterns::default()
        };
        assert_eq!(patterns.secret_tokens(), ["sk-"]);
        assert_eq!(patterns.destructive_commands(), ["rm -rf"]);
        assert_eq!(patterns.critical_system_paths(), ["/etc"]);
        assert_eq!(patterns.exfiltration_vectors(), ["curl"]);
        let snapshot = patterns.secret_tokens();
        patterns.secret_tokens.push("ghp_".into());
        assert_eq!(snapshot, ["sk-"], "a returned Vec is a copy");
    }

    #[test]
    fn external_peer_without_an_api_key_env_resolves_to_no_credential() {
        let peer = ExternalPeerAgentSpec::default();
        let dir = std::env::temp_dir();
        assert!(peer.resolve_api_key(&dir).unwrap().is_none());
    }
}
