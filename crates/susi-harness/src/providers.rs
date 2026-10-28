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

use crate::catalog::PriceCatalogue;
use crate::policy::UsageProfile;
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
    /// Whether the provider serves a repeated prompt prefix from its cache
    /// (DeepSeek automatic, Gemini implicit, Anthropic with `cache_control`).
    pub cache: bool,
}

impl Capabilities {
    /// What we assume about a model we have no record for: capable, unmeasured.
    /// Prompt caching is not assumed: a model that does not declare it is
    /// ranked as uncached.
    #[must_use]
    pub const fn assumed() -> Self {
        Self {
            context: None,
            tools: true,
            streaming: true,
            vision: true,
            cache: false,
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

/// Where a model sits on the automatic ladder built from the environment.
///
/// A model with no tier is still addressable by name, but it is never chosen
/// automatically: the ladder only spends on models somebody placed on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// A cheap first attempt. Ordered by budget, then capability.
    Worker,
    /// A stronger fallback, tried after every worker has failed the task.
    Rescue,
}

impl Tier {
    fn parse(raw: &str) -> EaiResult<Self> {
        match raw {
            "worker" => Ok(Self::Worker),
            "rescue" => Ok(Self::Rescue),
            other => Err(EaiError::config(format!(
                "provider registry: unknown tier `{other}` (expected worker or rescue)"
            ))),
        }
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
    /// Automatic-ladder placement; `None` keeps the model off the ladder.
    pub tier: Option<Tier>,
}

/// The ladder built from the API keys present in the environment.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoConfig {
    /// The orchestrator: `deepseek-pro` when its key is set, otherwise the
    /// cheapest capable rescue model. `None` when no model is usable.
    pub orchestrator: Option<String>,
    /// Workers, cheapest first.
    pub workers: Vec<String>,
    /// Rescue models, cheapest first. Tried after the workers.
    pub rescue: Vec<String>,
    /// Ladder models the catalogue cannot price. They stay on the ladder, last
    /// within their tier, so a working model is never dropped for lack of a
    /// price; this list says which ones are running on no costing data.
    pub unpriced: Vec<String>,
}

/// A ladder candidate whose position is not yet settled.
struct Ranked {
    id: String,
    cost: crate::money::MicroUsd,
    cache: bool,
    score: u8,
    context: u64,
}

impl AutoConfig {
    /// The escalation order: every worker, then every rescue model.
    #[must_use]
    pub fn ladder(&self) -> Vec<String> {
        self.workers
            .iter()
            .chain(self.rescue.iter())
            .cloned()
            .collect()
    }
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

/// The environment variable naming an overlay registry file: JSON in the
/// built-in registry's shape, whose providers, models and aliases are added.
pub const OVERLAY_ENV: &str = "SUSI_HARNESS_PROVIDERS";

impl Registry {
    /// The registry in force: the one compiled into the binary, with the
    /// overlay file named by [`OVERLAY_ENV`] merged over it when that variable
    /// is set. A new provider or model is added there, with no rebuild.
    ///
    /// A missing or malformed overlay is never used and never ignored
    /// silently: [`Registry::overlay_error`] names it, and the harness CLI
    /// refuses to start on it.
    #[must_use]
    pub fn builtin() -> &'static Self {
        static REGISTRY: OnceLock<Registry> = OnceLock::new();
        REGISTRY.get_or_init(|| {
            let base = Self::parse(BUILTIN).unwrap_or_default();
            let overlay = std::env::var_os(OVERLAY_ENV).map(std::path::PathBuf::from);
            Self::with_overlay(&base, overlay.as_deref()).unwrap_or(base)
        })
    }

    /// Why the overlay named by [`OVERLAY_ENV`] cannot be used, or `None` when
    /// the variable is unset or the overlay is sound.
    #[must_use]
    pub fn overlay_error() -> Option<String> {
        let overlay = std::env::var_os(OVERLAY_ENV).map(std::path::PathBuf::from)?;
        let base = Self::parse(BUILTIN).unwrap_or_default();
        Self::with_overlay(&base, Some(&overlay))
            .err()
            .map(|error| error.to_string())
    }

    /// `base` with the registry document at `path` merged over it.
    fn with_overlay(base: &Self, path: Option<&std::path::Path>) -> EaiResult<Self> {
        let Some(path) = path else {
            return Ok(base.clone());
        };
        let text = std::fs::read_to_string(path).map_err(|error| {
            EaiError::config(format!("{OVERLAY_ENV}: read {}: {error}", path.display()))
        })?;
        base.merged(&text)
            .map_err(|error| EaiError::config(format!("{OVERLAY_ENV} {}: {error}", path.display())))
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

    /// Resolve aliases and provider prefixes to the canonical model identifier.
    #[must_use]
    pub fn canonical_id<'a>(&'a self, model: &'a str) -> &'a str {
        let mut name = model.trim();
        if let Some(stripped) = name.strip_prefix("susi/") {
            name = stripped.trim();
        }
        for _ in 0..4 {
            match self.aliases.get(name) {
                Some(target) if target != name => name = target.as_str(),
                _ => break,
            }
        }
        name
    }

    /// Resolve a caller-facing model name.
    ///
    /// Aliases are followed (bounded), then an exact model id is matched
    /// across providers, then `provider/model` selects a provider and keeps
    /// the rest as the backend id.
    #[must_use]
    pub fn resolve(&self, model: &str) -> Option<ResolvedModel> {
        let mut name = model.trim();
        if let Some(stripped) = name.strip_prefix("susi/") {
            name = stripped.trim();
        }
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

    /// Models with live API keys in the environment that meet `required` capabilities,
    /// optionally sorted by per-task budget cost from cheapest to most expensive under `catalogue`.
    #[must_use]
    pub fn available_capable_models(
        &self,
        required: &[Capability],
        catalogue: Option<&PriceCatalogue>,
        profile: UsageProfile,
    ) -> Vec<String> {
        let mut candidates = Vec::new();
        for provider in &self.providers {
            let key_ok = if provider.key_envs.is_empty() {
                true
            } else {
                provider.key_envs.iter().any(|k| crate::secrets::is_set(k))
            };
            if !key_ok {
                continue;
            }
            for model in &provider.models {
                if model.capabilities.missing(required).is_empty() {
                    candidates.push(model.id.clone());
                }
            }
        }
        if let Some(cat) = catalogue {
            let mut priced: Vec<(String, crate::money::MicroUsd)> = candidates
                .into_iter()
                .filter_map(|m| {
                    cat.price(&m).map(|p| {
                        let cost = p.cost(crate::pricing::TokenUsage::new(
                            profile.input,
                            0,
                            profile.output,
                        ));
                        (m, cost)
                    })
                })
                .collect();
            priced.sort_by_key(|(_, cost)| *cost);
            priced.into_iter().map(|(m, _)| m).collect()
        } else {
            candidates
        }
    }

    /// Build the ladder from the keys `key_present` reports, budget first and
    /// capability second.
    ///
    /// A model is eligible when its provider's key is set and it can call tools
    /// (a worker without tools cannot finish a task). Cached models come first,
    /// then cheaper ones; a model the catalogue does not price comes last. Equal prices go to the model with
    /// more capabilities, then the larger context, then the id, so the order is
    /// the same on every machine.
    #[must_use]
    pub fn auto_config(
        &self,
        key_present: &dyn Fn(&str) -> bool,
        catalogue: &PriceCatalogue,
        profile: UsageProfile,
    ) -> AutoConfig {
        self.auto_config_budgeted(key_present, catalogue, profile, &|_| true)
    }

    /// [`Registry::auto_config`] with a budget gate: a keyed provider whose
    /// `provider_active` answer is `false` contributes no model. The gate is
    /// the one-dollar rule in [`crate::balance`].
    #[must_use]
    pub fn auto_config_budgeted(
        &self,
        key_present: &dyn Fn(&str) -> bool,
        catalogue: &PriceCatalogue,
        profile: UsageProfile,
        provider_active: &dyn Fn(&str) -> bool,
    ) -> AutoConfig {
        let mut workers = Vec::new();
        let mut rescue = Vec::new();
        let mut unpriced = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for provider in &self.providers {
            let keyed = provider.key_envs.is_empty()
                || provider.key_envs.iter().any(|name| key_present(name));
            if !keyed {
                continue;
            }
            // A keyless provider is local and has no balance to gate on.
            if !provider.key_envs.is_empty() && !provider_active(&provider.id) {
                continue;
            }
            for model in &provider.models {
                let Some(tier) = model.tier else { continue };
                if !model.capabilities.missing(&[Capability::Tools]).is_empty() {
                    continue;
                }
                if !seen.insert(model.id.clone()) {
                    continue;
                }
                // A working model with no price still runs. It is ranked after
                // every priced model of the same tier, so a missing price
                // never makes it the cheapest choice, and it is reported.
                let price_key = model.price_ref.as_deref().unwrap_or(&model.id);
                let cost = match catalogue.price(price_key) {
                    Some(price) => price.cost(crate::pricing::TokenUsage::new(
                        profile.input,
                        0,
                        profile.output,
                    )),
                    None => {
                        unpriced.push(model.id.clone());
                        crate::money::MicroUsd::from_micros(u64::MAX)
                    }
                };
                let ranked = Ranked {
                    id: model.id.clone(),
                    cost,
                    cache: model.capabilities.cache,
                    score: u8::from(model.capabilities.streaming)
                        + u8::from(model.capabilities.vision),
                    context: model.capabilities.context.unwrap_or(0),
                };
                match tier {
                    Tier::Worker => workers.push(ranked),
                    Tier::Rescue => rescue.push(ranked),
                }
            }
        }
        // Cache first, always: a cached prefix is the cheapest input a model
        // can take, and the harness re-sends the same prompt on every turn of a
        // task. Budget second. Capability is the third key, and it decides only
        // the rescue tier, where the task is complex and the model must do more.
        let order = |list: &mut Vec<Ranked>, by_capability: bool| {
            list.sort_by(|a, b| {
                b.cache
                    .cmp(&a.cache)
                    .then_with(|| a.cost.cmp(&b.cost))
                    .then_with(|| {
                        if by_capability {
                            b.score
                                .cmp(&a.score)
                                .then_with(|| b.context.cmp(&a.context))
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .then_with(|| a.id.cmp(&b.id))
            });
            list.iter()
                .map(|ranked| ranked.id.clone())
                .collect::<Vec<_>>()
        };
        let workers = order(&mut workers, false);
        let rescue = order(&mut rescue, true);
        // The orchestrator is the primary when it is eligible, else the
        // cheapest rescue model, else the cheapest worker. A fleet of workers
        // alone, or one model of any tier, still has an orchestrator.
        let primary = Self::primary_orchestrator();
        let orchestrator = if workers.iter().chain(rescue.iter()).any(|id| id == primary) {
            Some(primary.to_string())
        } else {
            rescue.first().or_else(|| workers.first()).cloned()
        };
        AutoConfig {
            orchestrator,
            workers,
            rescue,
            unpriced,
        }
    }

    /// The default primary orchestrator model (DeepSeek Pro).
    #[must_use]
    pub const fn primary_orchestrator() -> &'static str {
        "deepseek-pro"
    }

    /// The default worker ladder: workers first (Gemini Flash, DeepSeek Flash,
    /// Kimi Flash), then the backup workers the harness escalates to when a job
    /// fails (DeepSeek Pro, Gemini Pro, Kimi Pro).
    #[must_use]
    pub fn default_worker_ladder() -> [&'static str; 6] {
        [
            "gemini-flash",
            "deepseek-flash",
            "kimi-flash",
            "deepseek-pro",
            "gemini-pro",
            "kimi-pro",
        ]
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
        cache: raw.get("cache").and_then(Value::as_bool).unwrap_or(false),
    };
    let tier = raw
        .get("tier")
        .and_then(Value::as_str)
        .map(Tier::parse)
        .transpose()?;
    Ok(ModelRecord {
        id,
        backend,
        capabilities,
        price_ref: raw
            .get("price_ref")
            .and_then(Value::as_str)
            .map(str::to_string),
        tier,
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
    "primary": "deepseek-pro",
    "orchestrator": "deepseek-pro",
    "worker": "gemini-flash",
    "flash": "deepseek-flash",
    "planner": "deepseek-pro",
    "reasoner": "deepseek-pro",
    "pro": "deepseek-pro",
    "cheap": "gemini-flash",
    "delegate": "gemini-pro",
    "reviewer": "gemini-pro",
    "kimi": "kimi-flash",
    "kimi-pro": "kimi-k3-pro",
    "backup": "deepseek-pro",
    "backup-worker": "deepseek-pro",
    "pro-worker-1": "deepseek-pro",
    "pro-worker-2": "gemini-pro",
    "pro-worker-3": "kimi-k3-pro",
    "pro-worker": "deepseek-pro"
  },
  "providers": [
    {
      "id": "deepseek",
      "wire": "openai",
      "endpoint": "https://api.deepseek.com/chat/completions",
      "key_env": ["DEEPSEEK_API_KEY"],
      "models": [
        {"id": "deepseek-flash", "backend": "deepseek-flash", "context": 128000, "tools": true, "streaming": true, "tier": "worker", "cache": true},
        {"id": "deepseek-pro", "backend": "deepseek-v4-pro", "context": 128000, "tools": true, "streaming": true, "tier": "rescue", "cache": true}
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
        {"id": "gemini-flash", "backend": "gemini-3.8-flash", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "gemini-pro", "backend": "gemini-pro-latest", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true}
      ]
    },
    {
      "id": "moonshot",
      "wire": "openai",
      "endpoint": "https://api.moonshot.ai/v1/chat/completions",
      "key_env": ["MOONSHOT_API_KEY", "KIMI_API_KEY"],
      "models": [
        {"id": "kimi-flash", "backend": "kimi-k2.7-code-highspeed", "context": 262144, "tools": true, "streaming": true, "tier": "worker"},
        {"id": "kimi-k3", "backend": "kimi-k3", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "rescue"},
        {"id": "kimi-k3-pro", "backend": "kimi-k3.1-pro-highspeed", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "rescue"}
      ]
    },
    {
      "id": "anthropic",
      "wire": "anthropic",
      "endpoint": "https://api.anthropic.com/v1/messages",
      "key_env": ["ANTHROPIC_API_KEY"],
      "models": [
        {"id": "claude-haiku", "backend": "claude-haiku-5-5", "context": 200000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true},
        {"id": "claude-sonnet", "backend": "claude-sonnet-5-5", "context": 200000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true},
        {"id": "claude-opus", "backend": "claude-opus-5-5", "context": 200000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true}
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
    fn provider_registry_routes_kimi_as_a_worker() {
        let registry = Registry::builtin();
        let kimi = registry.resolve("kimi-flash").expect("kimi-flash resolves");
        assert_eq!(kimi.provider, "moonshot");
        assert_eq!(kimi.wire, Wire::OpenAi);
        assert_eq!(kimi.endpoint, "https://api.moonshot.ai/v1/chat/completions");
        assert_eq!(kimi.model_id, "kimi-k2.7-code-highspeed");
        assert_eq!(
            kimi.key_envs,
            vec!["MOONSHOT_API_KEY", "KIMI_API_KEY"],
            "both the Moonshot name and the Kimi alias are accepted"
        );
        assert!(kimi.capabilities.tools);
        // Kimi Pro is the backup-worker tier, resolved through its alias.
        assert_eq!(
            registry.resolve("kimi-pro").unwrap().model_id,
            "kimi-k3.1-pro-highspeed"
        );
        assert_eq!(
            registry.resolve("kimi-k3-pro").unwrap().model_id,
            "kimi-k3.1-pro-highspeed"
        );
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
    fn provider_registry_orchestrator_and_worker_ladder() {
        let registry = Registry::builtin();
        assert_eq!(
            registry.resolve("orchestrator").unwrap().model_id,
            "deepseek-v4-pro"
        );
        assert_eq!(
            registry.resolve("primary").unwrap().model_id,
            "deepseek-v4-pro"
        );
        assert_eq!(
            registry.resolve("worker").unwrap().model_id,
            "gemini-3.8-flash"
        );
        assert_eq!(
            registry.resolve("backup").unwrap().model_id,
            "deepseek-v4-pro"
        );
        assert_eq!(
            registry.resolve("pro-worker-1").unwrap().model_id,
            "deepseek-v4-pro"
        );
        assert_eq!(
            registry.resolve("pro-worker-2").unwrap().model_id,
            "gemini-pro-latest"
        );
        assert_eq!(
            registry.resolve("pro-worker-3").unwrap().model_id,
            "kimi-k3.1-pro-highspeed"
        );
        assert_eq!(Registry::primary_orchestrator(), "deepseek-pro");
        assert_eq!(
            Registry::default_worker_ladder(),
            [
                "gemini-flash",
                "deepseek-flash",
                "kimi-flash",
                "deepseek-pro",
                "gemini-pro",
                "kimi-pro"
            ]
        );
    }

    /// One provider, one model, one key: the smallest fleet the harness runs.
    fn one_model_registry(model: &str, tier: &str) -> Registry {
        let json = format!(
            r#"{{"providers":[{{"id":"solo","wire":"openai","endpoint":"https://solo.example/v1/chat/completions","key_env":["SOLO_API_KEY"],"models":[{{"id":"{model}","tools":true,"tier":"{tier}"}}]}}]}}"#
        );
        Registry::parse(&json).unwrap()
    }

    #[test]
    fn one_worker_model_is_a_whole_fleet() {
        let registry = one_model_registry("gemini-flash", "worker");
        let auto = registry.auto_config(
            &keys(&["SOLO_API_KEY"]),
            &builtin_catalogue(),
            UsageProfile::agent(),
        );
        assert_eq!(auto.orchestrator.as_deref(), Some("gemini-flash"));
        assert_eq!(auto.ladder(), vec!["gemini-flash"]);
    }

    #[test]
    fn one_rescue_model_is_a_whole_fleet() {
        let registry = one_model_registry("gemini-pro", "rescue");
        let auto = registry.auto_config(
            &keys(&["SOLO_API_KEY"]),
            &builtin_catalogue(),
            UsageProfile::agent(),
        );
        assert!(auto.workers.is_empty());
        assert_eq!(auto.orchestrator.as_deref(), Some("gemini-pro"));
        assert_eq!(auto.ladder(), vec!["gemini-pro"]);
    }

    #[test]
    fn a_model_without_a_price_stays_on_the_ladder_last() {
        let json = r#"{"providers":[{"id":"solo","wire":"openai","endpoint":"https://solo.example/v1/chat/completions","key_env":["SOLO_API_KEY"],"models":[{"id":"no-such-price","tools":true,"tier":"worker"},{"id":"gemini-flash","tools":true,"tier":"worker"}]}]}"#;
        let registry = Registry::parse(json).unwrap();
        let auto = registry.auto_config(
            &keys(&["SOLO_API_KEY"]),
            &builtin_catalogue(),
            UsageProfile::agent(),
        );
        assert_eq!(auto.unpriced, vec!["no-such-price"]);
        assert_eq!(
            auto.workers.last().map(String::as_str),
            Some("no-such-price")
        );
        assert!(auto.orchestrator.is_some());
    }

    #[test]
    fn an_overlay_file_adds_a_provider_and_a_bad_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.json");
        std::fs::write(
            &good,
            r#"{"providers":[{"id":"local","wire":"openai","endpoint":"http://127.0.0.1:8080/v1/chat/completions","models":[{"id":"local-tiny","tools":true,"tier":"worker"}]}]}"#,
        )
        .unwrap();
        let base = Registry::parse(BUILTIN).unwrap();
        let merged = Registry::with_overlay(&base, Some(&good)).unwrap();
        assert!(merged.resolve("local-tiny").is_some());
        assert!(merged.resolve("deepseek-pro").is_some());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert!(Registry::with_overlay(&base, Some(&bad)).is_err());
        let missing = dir.path().join("missing.json");
        assert!(Registry::with_overlay(&base, Some(&missing)).is_err());
    }

    fn builtin_catalogue() -> PriceCatalogue {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/harness/prices.json"
        );
        PriceCatalogue::load(std::path::Path::new(path)).unwrap()
    }

    fn keys(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |name| names.contains(&name)
    }

    #[test]
    fn provider_registry_auto_config_with_only_deepseek_key() {
        let auto = Registry::builtin().auto_config(
            &keys(&["DEEPSEEK_API_KEY"]),
            &builtin_catalogue(),
            UsageProfile::agent(),
        );
        assert_eq!(auto.workers, vec!["deepseek-flash"]);
        assert_eq!(auto.rescue, vec!["deepseek-pro"]);
        assert_eq!(auto.orchestrator.as_deref(), Some("deepseek-pro"));
        assert!(auto.unpriced.is_empty());
        assert_eq!(auto.ladder(), vec!["deepseek-flash", "deepseek-pro"]);
    }

    #[test]
    fn provider_registry_auto_config_orders_workers_by_budget() {
        let catalogue = builtin_catalogue();
        let profile = UsageProfile::agent();
        let auto = Registry::builtin().auto_config(
            &keys(&["DEEPSEEK_API_KEY", "GEMINI_API_KEY", "MOONSHOT_API_KEY"]),
            &catalogue,
            profile,
        );
        assert_eq!(auto.workers.len(), 3, "{:?}", auto.workers);
        let costs: Vec<_> = auto
            .workers
            .iter()
            .map(|id| {
                catalogue
                    .price(id)
                    .unwrap()
                    .cost(crate::pricing::TokenUsage::new(
                        profile.input,
                        0,
                        profile.output,
                    ))
            })
            .collect();
        assert_eq!(
            auto.workers.last().map(String::as_str),
            Some("kimi-flash"),
            "uncached kimi ranks behind every cached worker: {:?}",
            auto.workers
        );
        assert!(
            costs[..2].windows(2).all(|pair| pair[0] <= pair[1]),
            "{costs:?}"
        );
        assert_eq!(auto.rescue.len(), 4, "{:?}", auto.rescue);
        assert_eq!(auto.orchestrator.as_deref(), Some("deepseek-pro"));
    }

    #[test]
    fn provider_registry_auto_config_ranks_cache_before_budget() {
        // Kimi is far cheaper, but it does not cache. The cached DeepSeek model
        // must still come first: cache is the top priority.
        let catalogue = PriceCatalogue::from_json(
            r#"{"schema":"susi/harness/prices/v1","models":{
                "deepseek-flash":{"input_hit":1,"input_miss":1000000,"output":1000000},
                "kimi-flash":{"input_hit":1,"input_miss":1,"output":1}}}"#,
        )
        .unwrap();
        let auto = Registry::builtin().auto_config(
            &keys(&["DEEPSEEK_API_KEY", "MOONSHOT_API_KEY"]),
            &catalogue,
            UsageProfile::agent(),
        );
        assert_eq!(auto.workers, vec!["deepseek-flash", "kimi-flash"]);
    }

    #[test]
    fn provider_registry_auto_config_without_keys_is_empty() {
        let auto = Registry::builtin().auto_config(
            &keys(&[]),
            &builtin_catalogue(),
            UsageProfile::agent(),
        );
        assert_eq!(auto, AutoConfig::default());
        assert!(auto.ladder().is_empty());
    }

    #[test]
    fn provider_registry_auto_config_falls_back_to_a_rescue_orchestrator() {
        let auto = Registry::builtin().auto_config(
            &keys(&["ANTHROPIC_API_KEY"]),
            &builtin_catalogue(),
            UsageProfile::agent(),
        );
        assert!(auto.workers.is_empty());
        assert_eq!(auto.orchestrator, auto.rescue.first().cloned());
        assert!(
            auto.rescue.iter().all(|id| id.starts_with("claude-")),
            "{:?}",
            auto.rescue
        );
    }

    #[test]
    fn provider_registry_auto_config_keeps_unpriced_models_last_on_the_ladder() {
        let catalogue = PriceCatalogue::from_json(
            r#"{"schema":"susi/harness/prices/v1","models":{"deepseek-flash":{"input_hit":3000,"input_miss":150000,"output":600000}}}"#,
        )
        .unwrap();
        let auto = Registry::builtin().auto_config(
            &keys(&["DEEPSEEK_API_KEY", "GEMINI_API_KEY"]),
            &catalogue,
            UsageProfile::agent(),
        );
        // Priced first, unpriced after: a working model is never dropped.
        assert_eq!(auto.workers, vec!["deepseek-flash", "gemini-flash"]);
        // Unpriced rescue models tie on cost, so capability decides: Gemini Pro
        // has the larger context and vision, and goes first.
        assert_eq!(auto.rescue, vec!["gemini-pro", "deepseek-pro"]);
        assert_eq!(
            auto.unpriced,
            vec!["deepseek-pro", "gemini-flash", "gemini-pro"]
        );
        assert_eq!(auto.orchestrator.as_deref(), Some("deepseek-pro"));
    }

    #[test]
    fn provider_registry_budget_gate_keeps_inactive_providers_off_the_ladder() {
        // Gemini holds a key but is below the budget floor, so none of its
        // models may be on the ladder. DeepSeek is active and stays.
        let gemini_off = |provider: &str| provider != "gemini";
        let auto = Registry::builtin().auto_config_budgeted(
            &keys(&["DEEPSEEK_API_KEY", "GEMINI_API_KEY"]),
            &builtin_catalogue(),
            UsageProfile::agent(),
            &gemini_off,
        );
        assert!(
            auto.workers.iter().all(|id| id != "gemini-flash"),
            "{:?}",
            auto.workers
        );
        assert!(
            auto.rescue.iter().all(|id| id != "gemini-pro"),
            "{:?}",
            auto.rescue
        );
        assert_eq!(auto.workers, vec!["deepseek-flash"]);
        assert_eq!(auto.orchestrator.as_deref(), Some("deepseek-pro"));
    }

    #[test]
    fn provider_registry_rejects_an_unknown_tier() {
        let broken = r#"{"providers":[{"id":"x","wire":"openai","endpoint":"https://x","models":[{"id":"m","tier":"boss"}]}]}"#;
        let error = Registry::parse(broken).unwrap_err().to_string();
        assert!(error.contains("boss"), "{error}");
    }

    #[test]
    fn provider_registry_rejects_an_unknown_wire() {
        let broken = r#"{"providers":[{"id":"x","wire":"grpc","endpoint":"https://x"}]}"#;
        let error = Registry::parse(broken).unwrap_err().to_string();
        assert!(error.contains("grpc"), "{error}");
    }
}
