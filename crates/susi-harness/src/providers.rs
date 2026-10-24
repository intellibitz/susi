//! The provider registry.
//!
//! Providers, models and their capabilities are data: adding a provider is a
//! record, not a match arm, and a model that cannot do what the caller needs is
//! refused before the call is priced or dispatched.
//!
//! The registry compiled into the binary is the floor, not the ceiling: a user
//! file (or a test) can [`Registry::merged`] extra providers, extra models and
//! aliases on top of it. Resolution order is alias, exact model id, then
//! `provider/model` prefix; an unknown model is left to the caller's fallback
//! provider rather than guessed at.

use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use susi_error::{EaiError, EaiResult};

/// What a provider speaks on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// OpenAI chat-completions shape.
    OpenAi,
    /// Anthropic Messages shape.
    Anthropic,
}

impl Wire {
    fn parse(value: &str) -> EaiResult<Self> {
        match value {
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            other => Err(EaiError::config(format!(
                "provider registry: unknown wire `{other}`"
            ))),
        }
    }
}

/// A capability a caller can require before dispatching a turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Function/tool calling.
    Tools,
    /// Server-sent-event streaming.
    Streaming,
    /// Image input.
    Vision,
}

impl Capability {
    const fn label(self) -> &'static str {
        match self {
            Self::Tools => "tool calling",
            Self::Streaming => "streaming",
            Self::Vision => "vision",
        }
    }
}

/// What a model can do. `context` is `None` when the provider does not
/// document it (most per-request aliases do not).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Capabilities {
    /// Documented context window in tokens.
    pub context: Option<u64>,
    /// Whether the model accepts tools.
    pub tools: bool,
    /// Whether the model can stream.
    pub streaming: bool,
    /// Whether the model accepts images.
    pub vision: bool,
}

impl Capabilities {
    /// What we assume about a model we have no record for: capable, unmeasured.
    #[must_use]
    pub const fn assumed() -> Self {
        Self {
            context: None,
            tools: true,
            streaming: true,
            vision: true,
        }
    }

    /// The required capabilities this record does not have.
    #[must_use]
    pub fn missing(&self, required: &[Capability]) -> Vec<&'static str> {
        required
            .iter()
            .filter(|capability| match capability {
                Capability::Tools => !self.tools,
                Capability::Streaming => !self.streaming,
                Capability::Vision => !self.vision,
            })
            .map(|capability| capability.label())
            .collect()
    }
}

/// One model a provider serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRecord {
    /// Id callers ask for.
    pub id: String,
    /// Id the provider expects on the wire.
    pub backend: String,
    /// What the model can do.
    pub capabilities: Capabilities,
    /// Catalogue model key, when it differs from [`Self::id`].
    pub price_ref: Option<String>,
}

/// One provider endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRecord {
    /// Stable id (`deepseek`, `anthropic`, …).
    pub id: String,
    /// Wire family.
    pub wire: Wire,
    /// Full chat-completions URL.
    pub endpoint: String,
    /// Environment variables that may hold the key, in priority order.
    pub key_envs: Vec<String>,
    /// Models known by name.
    pub models: Vec<ModelRecord>,
}

/// A model resolved to an endpoint and a wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    /// Provider id.
    pub provider: String,
    /// Wire family.
    pub wire: Wire,
    /// Full chat-completions URL.
    pub endpoint: String,
    /// Id to put in the outgoing request.
    pub model_id: String,
    /// Environment variables that may hold the key.
    pub key_envs: Vec<String>,
    /// What the model can do.
    pub capabilities: Capabilities,
    /// Whether this came from an explicit record (as opposed to a prefix alias).
    pub known: bool,
}

/// The data every dispatch resolves through.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    providers: Vec<ProviderRecord>,
    aliases: BTreeMap<String, String>,
}

impl Registry {
    /// The registry compiled into the binary.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        REGISTRY.get_or_init(|| Self::parse(BUILTIN).unwrap_or_default())
    }

    /// Parse a registry document.
    ///
    /// # Errors
    ///
    /// Returns a config error for malformed JSON, a missing provider id, an
    /// unknown wire family or an empty endpoint.
    pub fn parse(json: &str) -> EaiResult<Self> {
        let root: Value = serde_json::from_str(json)
            .map_err(|error| EaiError::config(format!("provider registry is not JSON: {error}")))?;
        let mut registry = Self::default();
        if let Some(aliases) = root.get("aliases").and_then(Value::as_object) {
            for (name, target) in aliases {
                if let Some(target) = target.as_str() {
                    registry.aliases.insert(name.clone(), target.to_string());
                }
            }
        }
        let providers = root
            .get("providers")
            .and_then(Value::as_array)
            .ok_or_else(|| EaiError::config("provider registry has no providers array"))?;
        for raw in providers {
            registry.providers.push(parse_provider(raw)?);
        }
        Ok(registry)
    }

    /// This registry with another document merged on top.
    ///
    /// Providers are matched by id, models by id and aliases by name, so an
    /// override can add a model to an existing provider without restating it.
    ///
    /// # Errors
    ///
    /// Returns the parse error of the override document.
    pub fn merged(&self, overrides: &str) -> EaiResult<Self> {
        let extra = Self::parse(overrides)?;
        let mut merged = self.clone();
        for provider in extra.providers {
            match merged
                .providers
                .iter_mut()
                .find(|existing| existing.id == provider.id)
            {
                Some(existing) => {
                    for model in provider.models {
                        if !existing.models.iter().any(|known| known.id == model.id) {
                            existing.models.push(model);
                        }
                    }
                }
                None => merged.providers.push(provider),
            }
        }
        merged.aliases.extend(extra.aliases);
        Ok(merged)
    }

    /// Providers in the registry.
    #[must_use]
    pub fn providers(&self) -> &[ProviderRecord] {
        &self.providers
    }

    /// Aliases in the registry.
    #[must_use]
    pub fn aliases(&self) -> &BTreeMap<String, String> {
        &self.aliases
    }

    /// Resolve a caller-facing model name.
    ///
    /// Aliases are followed (bounded), then an exact model id is matched
    /// across providers, then `provider/model` selects a provider and keeps
    /// the rest as the backend id.
    #[must_use]
    pub fn resolve(&self, model: &str) -> Option<ResolvedModel> {
        let mut name = model.trim();
        if name.is_empty() {
            return None;
        }
        for _ in 0..4 {
            match self.aliases.get(name) {
                Some(target) if target != name => name = target.as_str(),
                _ => break,
            }
        }
        for provider in &self.providers {
            if let Some(record) = provider.models.iter().find(|record| record.id == name) {
                return Some(provider.resolved(record.backend.clone(), record.capabilities, true));
            }
        }
        if let Some((provider_id, rest)) = name.split_once('/') {
            let rest = rest.trim();
            if !rest.is_empty() {
                if let Some(provider) = self
                    .providers
                    .iter()
                    .find(|provider| provider.id == provider_id)
                {
                    return Some(provider.resolved(
                        rest.to_string(),
                        Capabilities::assumed(),
                        false,
                    ));
                }
            }
        }
        None
    }

    /// Refuse a turn when a known model lacks a required capability.
    ///
    /// An unknown model is not refused: resolution falls back to the caller's
    /// upstream, and pretending to know its capabilities would be worse than
    /// letting the provider answer.
    ///
    /// # Errors
    ///
    /// Returns a config error naming the missing capabilities.
    pub fn require(&self, model: &str, required: &[Capability]) -> EaiResult<()> {
        let Some(resolved) = self.resolve(model) else {
            return Ok(());
        };
        let missing = resolved.capabilities.missing(required);
        if missing.is_empty() {
            return Ok(());
        }
        Err(EaiError::config(format!(
            "model `{model}` cannot run this turn: missing {}",
            missing.join(", ")
        )))
    }
}

impl ProviderRecord {
    fn resolved(&self, model_id: String, capabilities: Capabilities, known: bool) -> ResolvedModel {
        ResolvedModel {
            provider: self.id.clone(),
            wire: self.wire,
            endpoint: self.endpoint.clone(),
            model_id,
            key_envs: self.key_envs.clone(),
            capabilities,
            known,
        }
    }
}

fn parse_provider(raw: &Value) -> EaiResult<ProviderRecord> {
    let id = string_field(raw, "id")?;
    let wire = Wire::parse(&string_field(raw, "wire")?)?;
    let endpoint = string_field(raw, "endpoint")?;
    if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
        return Err(EaiError::config(format!(
            "provider registry: provider `{id}` endpoint `{endpoint}` is not a URL"
        )));
    }
    let key_envs = raw
        .get("key_env")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut models = Vec::new();
    if let Some(records) = raw.get("models").and_then(Value::as_array) {
        for record in records {
            models.push(parse_model(record)?);
        }
    }
    Ok(ProviderRecord {
        id,
        wire,
        endpoint,
        key_envs,
        models,
    })
}

fn parse_model(raw: &Value) -> EaiResult<ModelRecord> {
    let id = string_field(raw, "id")?;
    let backend = raw
        .get("backend")
        .and_then(Value::as_str)
        .unwrap_or(&id)
        .to_string();
    let capabilities = Capabilities {
        context: raw.get("context").and_then(Value::as_u64),
        tools: raw.get("tools").and_then(Value::as_bool).unwrap_or(false),
        streaming: raw
            .get("streaming")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        vision: raw.get("vision").and_then(Value::as_bool).unwrap_or(false),
    };
    Ok(ModelRecord {
        id,
        backend,
        capabilities,
        price_ref: raw
            .get("price_ref")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn string_field(raw: &Value, field: &str) -> EaiResult<String> {
    raw.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| EaiError::config(format!("provider registry: `{field}` is required")))
}

/// The registry compiled into the binary.
///
/// `price_ref` names the catalogue entry when a provider's own id differs from
/// the price file. Capabilities that are not documented are `false`, so a
/// refusal is a fact rather than a guess.
const BUILTIN: &str = r#"
{
  "aliases": {
    "coder": "deepseek-flash",
    "worker": "deepseek-flash",
    "flash": "deepseek-flash",
    "planner": "deepseek-pro",
    "reasoner": "deepseek-pro",
    "pro": "deepseek-pro",
    "cheap": "gemini-flash",
    "delegate": "gemini-pro",
    "reviewer": "gemini-pro"
  },
  "providers": [
    {
      "id": "deepseek",
      "wire": "openai",
      "endpoint": "https://api.deepseek.com/chat/completions",
      "key_env": ["DEEPSEEK_API_KEY"],
      "models": [
        {"id": "deepseek-flash", "backend": "deepseek-flash", "context": 128000, "tools": true, "streaming": true},
        {"id": "deepseek-pro", "backend": "deepseek-v4-pro", "context": 128000, "tools": true, "streaming": true}
      ]
    },
    {
      "id": "siliconflow",
      "wire": "openai",
      "endpoint": "https://api.siliconflow.cn/v1/chat/completions",
      "key_env": ["SILICONFLOW_API_KEY"],
      "models": [
        {"id": "siliconflow-qwen", "backend": "Qwen/Qwen3-235B-A22B-Instruct-2507", "context": 128000, "tools": true, "streaming": true},
        {"id": "siliconflow-deepseek", "backend": "deepseek-ai/DeepSeek-V3.2", "context": 128000, "tools": true, "streaming": true},
        {"id": "siliconflow-qwen-coder", "backend": "Qwen/Qwen2.5-Coder-32B-Instruct", "context": 32768, "tools": true, "streaming": true}
      ]
    },
    {
      "id": "dashscope",
      "wire": "openai",
      "endpoint": "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
      "key_env": ["DASHSCOPE_API_KEY"],
      "models": [
        {"id": "dashscope-qwen-plus", "backend": "qwen-plus", "context": 131072, "tools": true, "streaming": true},
        {"id": "dashscope-qwen-max", "backend": "qwen-max", "context": 32768, "tools": true, "streaming": true},
        {"id": "dashscope-qwen-coder", "backend": "qwen2.5-coder-32b-instruct", "context": 32768, "tools": true, "streaming": true}
      ]
    },
    {
      "id": "gemini",
      "wire": "openai",
      "endpoint": "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
      "key_env": ["GEMINI_API_KEY", "GOOGLE_API_KEY"],
      "models": [
        {"id": "gemini-flash", "backend": "gemini-3.8-flash", "context": 1048576, "tools": true, "streaming": true, "vision": true},
        {"id": "gemini-pro", "backend": "gemini-pro-latest", "context": 1048576, "tools": true, "streaming": true, "vision": true}
      ]
    },
    {
      "id": "anthropic",
      "wire": "anthropic",
      "endpoint": "https://api.anthropic.com/v1/messages",
      "key_env": ["ANTHROPIC_API_KEY"],
      "models": [
        {"id": "claude-haiku", "backend": "claude-haiku-5-5", "context": 200000, "tools": true, "streaming": true, "vision": true},
        {"id": "claude-sonnet", "backend": "claude-sonnet-5-5", "context": 200000, "tools": true, "streaming": true, "vision": true},
        {"id": "claude-opus", "backend": "claude-opus-5-5", "context": 200000, "tools": true, "streaming": true, "vision": true}
      ]
    },
    {
      "id": "openrouter",
      "wire": "openai",
      "endpoint": "https://openrouter.ai/api/v1/chat/completions",
      "key_env": ["OPENROUTER_API_KEY"],
      "models": []
    }
  ]
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    const OVERRIDE: &str = r#"
    {
      "aliases": {"scratch": "local-small"},
      "providers": [
        {
          "id": "local",
          "wire": "openai",
          "endpoint": "http://127.0.0.1:11434/v1/chat/completions",
          "key_env": [],
          "models": [
            {"id": "local-small", "backend": "qwen2.5:3b", "context": 32768, "tools": true, "streaming": true},
            {"id": "local-prompt", "backend": "gemma:2b", "streaming": true}
          ]
        }
      ]
    }
    "#;

    #[test]
    fn provider_registry_builtin_parses() {
        let registry = Registry::builtin();
        assert!(registry.providers().len() >= 6);
        assert_eq!(
            registry.aliases().get("coder").map(String::as_str),
            Some("deepseek-flash")
        );
    }

    #[test]
    fn provider_registry_resolves_a_known_model() {
        let resolved = Registry::builtin().resolve("deepseek-flash").unwrap();
        assert_eq!(resolved.provider, "deepseek");
        assert_eq!(resolved.wire, Wire::OpenAi);
        assert_eq!(
            resolved.endpoint,
            "https://api.deepseek.com/chat/completions"
        );
        assert_eq!(resolved.model_id, "deepseek-flash");
        assert_eq!(resolved.key_envs, vec!["DEEPSEEK_API_KEY"]);
        assert!(resolved.capabilities.tools);
        assert!(resolved.known);
    }

    /// The tiers the fleet runs on: DeepSeek drives, Gemini delegates.
    #[test]
    fn provider_registry_carries_the_deepseek_and_gemini_tiers() {
        let registry = Registry::builtin();
        assert_eq!(
            registry.resolve("deepseek-pro").unwrap().model_id,
            "deepseek-v4-pro"
        );
        assert_eq!(
            registry.resolve("gemini-pro").unwrap().model_id,
            "gemini-pro-latest"
        );
        // The provider ids DeepSeek actually advertises; the previous
        // chat/reasoner ids are retired upstream and would 404.
        assert!(registry.resolve("deepseek-chat").is_none());
        assert!(registry.resolve("deepseek-reasoner").is_none());
    }

    #[test]
    fn provider_registry_maps_friendly_ids_to_backend_ids() {
        let registry = Registry::builtin();
        assert_eq!(
            registry.resolve("claude-sonnet").unwrap().model_id,
            "claude-sonnet-5-5"
        );
        assert_eq!(
            registry.resolve("gemini-flash").unwrap().model_id,
            "gemini-3.8-flash"
        );
        assert_eq!(
            registry.resolve("siliconflow-qwen-coder").unwrap().model_id,
            "Qwen/Qwen2.5-Coder-32B-Instruct"
        );
        assert_eq!(
            registry.resolve("claude-sonnet").unwrap().wire,
            Wire::Anthropic
        );
    }

    #[test]
    fn provider_registry_follows_aliases() {
        let registry = Registry::builtin();
        let coder = registry.resolve("coder").unwrap();
        assert_eq!(coder.model_id, "deepseek-flash");
        let planner = registry.resolve("planner").unwrap();
        assert_eq!(planner.model_id, "deepseek-v4-pro");
        let cheap = registry.resolve("cheap").unwrap();
        assert_eq!(cheap.model_id, "gemini-3.8-flash");
        let reviewer = registry.resolve("reviewer").unwrap();
        assert_eq!(reviewer.model_id, "gemini-pro-latest");
    }

    #[test]
    fn provider_registry_prefix_selects_a_provider() {
        let resolved = Registry::builtin()
            .resolve("openrouter/qwen/qwen3")
            .unwrap();
        assert_eq!(resolved.provider, "openrouter");
        assert_eq!(resolved.model_id, "qwen/qwen3");
        assert!(!resolved.known);
        assert_eq!(
            resolved.endpoint,
            "https://openrouter.ai/api/v1/chat/completions"
        );
        // A prefix alias is assumed capable, never silently refused.
        assert!(resolved.capabilities.tools);
    }

    #[test]
    fn provider_registry_override_adds_a_provider_and_alias() {
        let registry = Registry::builtin().merged(OVERRIDE).unwrap();
        let resolved = registry.resolve("scratch").unwrap();
        assert_eq!(resolved.provider, "local");
        assert_eq!(
            resolved.endpoint,
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(resolved.model_id, "qwen2.5:3b");
        // The builtin providers survive the merge.
        assert!(registry.resolve("deepseek-flash").is_some());
    }

    #[test]
    fn provider_registry_refuses_a_missing_capability() {
        let registry = Registry::builtin().merged(OVERRIDE).unwrap();
        let error = registry
            .require("local-prompt", &[Capability::Tools])
            .unwrap_err()
            .to_string();
        assert!(error.contains("tool calling"), "{error}");
        assert!(registry
            .require("local-prompt", &[Capability::Streaming])
            .is_ok());
    }

    #[test]
    fn provider_registry_unknown_model_is_left_to_the_fallback() {
        let registry = Registry::builtin();
        assert!(registry.resolve("mystery-model").is_none());
        assert!(registry
            .require("mystery-model", &[Capability::Tools, Capability::Vision])
            .is_ok());
    }

    #[test]
    fn provider_registry_rejects_a_non_url_endpoint() {
        let broken = r#"{"providers":[{"id":"x","wire":"openai","endpoint":"nope"}]}"#;
        assert!(Registry::parse(broken).is_err());
    }

    #[test]
    fn provider_registry_rejects_an_unknown_wire() {
        let broken = r#"{"providers":[{"id":"x","wire":"grpc","endpoint":"https://x"}]}"#;
        let error = Registry::parse(broken).unwrap_err().to_string();
        assert!(error.contains("grpc"), "{error}");
    }
}
