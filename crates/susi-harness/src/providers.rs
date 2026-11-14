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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// A light model that does the work: every task starts on one.
    Worker,
    /// A strong model that fixes what a worker could not (a failed or
    /// hallucinated attempt escalates straight to the fixers), and the tier
    /// the orchestrator comes from.
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
    /// The orchestrator: the strongest rescue model on expiring credit when
    /// there is one, else `deepseek-pro` when it is eligible, else the
    /// cheapest rescue model, else the cheapest worker. `None` when no model
    /// is usable.
    pub orchestrator: Option<String>,
    /// Models whose provider holds credit that expires, of either tier,
    /// soonest expiry first. Each leads its own tier (workers or rescue), so
    /// the credit is spent before it is lost without a strong model taking
    /// the light models' work.
    pub use_first: Vec<String>,
    /// Workers, cheapest first.
    pub workers: Vec<String>,
    /// Rescue models, cheapest first. Tried after the workers.
    pub rescue: Vec<String>,
    /// Ladder models the catalogue cannot price. They stay on the ladder, last
    /// within their tier, so a working model is never dropped for lack of a
    /// price; this list says which ones are running on no costing data.
    pub unpriced: Vec<String>,
    /// Models left out because their last real call failed: the calls are the
    /// source of truth, whatever the provider's budget says.
    pub failed: Vec<String>,
    /// The preferred providers ([`PREFER_ENV`]) that put a model on the
    /// ladder, first preferred first. A preferred provider with no key, no
    /// budget or no working model is not here: naming it changed nothing.
    pub preferred: Vec<String>,
}

/// What the budget says of each keyed provider, for [`Registry::auto_config_ranked`].
pub struct ProviderGate<'a> {
    /// Whether the provider may run: the one-dollar rule in [`crate::balance`].
    pub active: &'a dyn Fn(&str) -> bool,
    /// The day the provider's credit expires, when that credit should be
    /// spent before anything else: active, dated and not yet expired.
    pub use_first: &'a dyn Fn(&str) -> Option<u64>,
    /// The provider's place among the preferred providers ([`PREFER_ENV`]):
    /// `Some(0)` for the first named, and `None` for a provider that is not
    /// preferred. Preferred providers lead their tier, after expiring
    /// credit, and the first of them with a priced rescue model orchestrates.
    pub prefer: &'a dyn Fn(&str) -> Option<usize>,
    /// A model's measured mean call latency in milliseconds, when the ledger
    /// has any ([`crate::policy::latencies`]).
    pub latency: &'a dyn Fn(&str) -> Option<u64>,
    /// Whether the model's last real call succeeded (or it has made none).
    /// A model that answers no is left out ([`crate::policy::failed_models`]).
    pub live: &'a dyn Fn(&str) -> bool,
}

/// A price's band. Bands are about 10% wide (each edge is 1.1 times the
/// last), so prices that close usually share one and a trivial price
/// difference does not outrank a faster model. Fixed edges keep the order a
/// total order: two near prices on either side of an edge stay ordered by
/// price.
fn cost_band(cost: crate::money::MicroUsd) -> u32 {
    let cost = u128::from(cost.micros());
    let mut band = 0;
    let mut edge: u128 = 1;
    while edge < cost {
        edge = edge * 11 / 10 + 1;
        band += 1;
    }
    band
}

/// Credit that expires first, soonest first; undated credit after it.
fn by_expiry(a: Option<u64>, b: Option<u64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Known latency first, fastest first; unmeasured models after them.
fn by_latency(a: Option<u64>, b: Option<u64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Preferred providers first, in the order the operator named them; the
/// providers not named after them, in their usual order.
fn by_preference(a: Option<usize>, b: Option<usize>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// The setting that names the providers to prefer: a comma-separated list of
/// registry ids (`siliconflow,deepinfra`), first preferred first. Their models
/// lead their tier, after expiring credit and before cache and price, and the
/// first named with a priced rescue model orchestrates. A name the registry
/// does not know is ignored, so a typo costs nothing but the preference.
pub const PREFER_ENV: &str = "SUSI_HARNESS_PREFER";

/// The preferred provider ids, in order, from [`PREFER_ENV`] (environment,
/// then the `.env` files the harness reads). Empty when it is not set.
#[must_use]
pub fn preferred_providers() -> Vec<String> {
    parse_preferred(crate::secrets::setting(PREFER_ENV).as_deref())
}

/// Parse a [`PREFER_ENV`] value: ids split on commas and whitespace, trimmed,
/// lower-cased, each kept once at its first place. `None` or blank is empty.
#[must_use]
pub fn parse_preferred(raw: Option<&str>) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for part in raw.unwrap_or_default().split([',', ' ', '\t', ';']) {
        let id = part.trim().to_ascii_lowercase();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// A ladder candidate whose position is not yet settled.
struct Ranked {
    id: String,
    /// The provider the model runs at.
    provider: String,
    /// The provider's place among the preferred providers, when it is one.
    prefer: Option<usize>,
    /// Whether the catalogue prices the model; an unpriced one ranks after
    /// every priced one of its tier.
    priced: bool,
    cost: crate::money::MicroUsd,
    cache: bool,
    score: u8,
    context: u64,
    /// The day the provider's credit expires, when it is to be spent first.
    expires: Option<u64>,
    tier: Tier,
    /// The price band ([`cost_band`]).
    band: u32,
    /// Measured mean call latency, in milliseconds.
    latency: Option<u64>,
}

impl AutoConfig {
    /// Every model in the fleet: the light workers, then the strong rescue
    /// models that fix what a worker could not. Expiring credit leads within
    /// each tier, never across: a strong model on expiring credit is still a
    /// fixer, not a first-line worker.
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
    /// What every key of this provider starts with, when the provider
    /// publishes a fixed form (`sk-ant-` for Anthropic). A value that does
    /// not is not this provider's key, and [`crate::secrets`] passes over it.
    pub key_prefix: Option<String>,
    /// Where a user gets a key for this provider, for first-run guidance.
    pub key_url: Option<String>,
    /// The query that narrows the provider's models endpoint to the chat
    /// models, when the list mixes in other kinds (`type=text&sub_type=chat`
    /// at SiliconFlow). Discovery ([`crate::verify::discover`]) sends it;
    /// verification reads the whole list.
    pub models_query: Option<String>,
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
    /// The tier a model is placed in, when the registry places it.
    #[must_use]
    pub fn tier_of(&self, model: &str) -> Option<Tier> {
        let id = self.canonical_id(model);
        self.providers
            .iter()
            .flat_map(|provider| provider.models.iter())
            .find(|record| record.id == id)
            .and_then(|record| record.tier)
    }

    /// The key prefix the provider reading the variable `name` requires, when
    /// one does.
    #[must_use]
    pub fn key_prefix_for(&self, name: &str) -> Option<(&str, &str)> {
        self.providers.iter().find_map(|provider| {
            let prefix = provider.key_prefix.as_deref()?;
            provider
                .key_envs
                .iter()
                .any(|env| env == name)
                .then_some((provider.id.as_str(), prefix))
        })
    }

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
        REGISTRY.get_or_init(Self::from_environment)
    }

    /// The compiled-in registry with the overlay in force merged over it, or
    /// without it when the overlay cannot be used.
    fn from_environment() -> Self {
        let base = Self::parse(BUILTIN).unwrap_or_default();
        Self::with_overlay(&base, Self::overlay_path().as_deref()).unwrap_or(base)
    }

    /// The overlay file [`OVERLAY_ENV`] names, read from the user's own
    /// places only ([`crate::secrets::setting_path`]: the process, the
    /// Windows user and machine environment, the data home's `.env`), never
    /// from `./.env` ([`crate::secrets::USER_ONLY`]): an overlay decides
    /// where keys are sent, and a repository's `.env` once sent the user's
    /// DeepSeek key to the repository's own endpoint through one.
    ///
    /// The registry and its fail-closed check both read it here, the place
    /// `susi-harness env` reports it from. They once read the process
    /// environment alone, so an overlay set in a `.env` file was shown as set
    /// and never loaded, and a bad one never stopped the harness.
    fn overlay_path() -> Option<std::path::PathBuf> {
        crate::secrets::setting_path(OVERLAY_ENV)
    }

    /// Why the overlay named by [`OVERLAY_ENV`] cannot be used, or `None` when
    /// the variable is unset or the overlay is sound.
    #[must_use]
    pub fn overlay_error() -> Option<String> {
        let overlay = Self::overlay_path()?;
        let base = Self::parse(BUILTIN).unwrap_or_default();
        Self::with_overlay(&base, Some(&overlay))
            .err()
            .map(|error| error.to_string())
    }

    /// `base` with the registry document at `path` merged over it.
    ///
    /// The user vouches for an overlay named by a full path (or by a path in
    /// the data home's `.env`, taken from the data home): it may send an
    /// existing provider's key to another host, a regional endpoint or the
    /// provider's own proxy. A relative path is read from the directory the
    /// harness runs in, which may be a repository the user just cloned, so
    /// that overlay may only add ([`Registry::merged`]).
    fn with_overlay(base: &Self, path: Option<&std::path::Path>) -> EaiResult<Self> {
        let Some(path) = path else {
            return Ok(base.clone());
        };
        let text = std::fs::read_to_string(path).map_err(|error| {
            EaiError::config(format!("{OVERLAY_ENV}: read {}: {error}", path.display()))
        })?;
        base.merge(&text, path.is_absolute())
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
    /// Providers are matched by id, models by id and aliases by name. An
    /// override adds: new providers, new models (to a new provider or to an
    /// existing one) and new aliases. A provider it names that already exists
    /// must state the same wire and endpoint, and the same key variables, key
    /// form and key page where it states those; a model id that already
    /// exists must be restated unchanged under the same provider, and an
    /// alias that already exists must name the same model. An alias may not
    /// take the name of a model, or of a `provider/model` route. A new
    /// provider may read an existing provider's key variable only to send it
    /// to that provider's own origin (scheme and host). Anything else is
    /// refused, not dropped: an overlay that pointed `deepseek` at a proxy was
    /// once accepted while every call still went to DeepSeek, and one whose
    /// alias `deepseek-flash` named its own model sent every call for the
    /// built-in model, with the user's DeepSeek key, to the overlay's host.
    ///
    /// # Errors
    ///
    /// Returns the parse error of the override document, or a config error
    /// naming the provider, model or alias it would redefine, or the key
    /// variable it would send elsewhere.
    pub fn merged(&self, overrides: &str) -> EaiResult<Self> {
        self.merge(overrides, false)
    }

    /// [`Registry::merged`], where an overlay the user `vouched` for (one
    /// named by a full path in the user's own settings) may also give a new
    /// provider an existing provider's key variable for another origin.
    fn merge(&self, overrides: &str, vouched: bool) -> EaiResult<Self> {
        let extra = Self::parse(overrides)?;
        let stated = stated_key_fields(overrides);
        let mut merged = self.clone();
        let mut added = Vec::new();
        for (index, mut provider) in extra.providers.into_iter().enumerate() {
            let models = std::mem::take(&mut provider.models);
            let at = match merged
                .providers
                .iter()
                .position(|existing| existing.id == provider.id)
            {
                Some(at) => {
                    let stated = stated.get(index).map_or(&[][..], Vec::as_slice);
                    let changed = changed_fields(&merged.providers[at], &provider, stated);
                    if !changed.is_empty() {
                        return Err(EaiError::config(format!(
                            "provider registry: provider `{}` is already defined and an overlay only adds; it cannot change its {} (give the provider a new id)",
                            provider.id,
                            changed.join(", ")
                        )));
                    }
                    at
                }
                None => {
                    if !vouched {
                        merged.refuse_a_key_sent_elsewhere(&provider)?;
                    }
                    merged.providers.push(provider);
                    merged.providers.len() - 1
                }
            };
            for model in models {
                added.push(model.id.clone());
                merged.add_model(at, model)?;
            }
        }
        for (name, target) in extra.aliases {
            merged.add_alias(name, target)?;
        }
        // Resolution follows an alias before it matches a model id, so a
        // model named like an alias could never be called by its name.
        for id in added {
            if let Some(target) = merged.aliases.get(&id).filter(|target| **target != id) {
                return Err(EaiError::config(format!(
                    "provider registry: model `{id}` is also an alias for `{target}`, so it could never be called by its name (give the model a new id)"
                )));
            }
        }
        Ok(merged)
    }

    /// Add `model` to the provider at `at`, unless it restates a model that
    /// provider already has, unchanged.
    fn add_model(&mut self, at: usize, model: ModelRecord) -> EaiResult<()> {
        let known = self
            .providers
            .iter()
            .enumerate()
            .find_map(|(index, provider)| {
                provider
                    .models
                    .iter()
                    .find(|known| known.id == model.id)
                    .map(|known| (index, *known == model))
            });
        match known {
            None => {
                self.providers[at].models.push(model);
                Ok(())
            }
            Some((index, true)) if index == at => Ok(()),
            Some((index, _)) => Err(EaiError::config(format!(
                "provider registry: model `{}` is already defined by provider `{}` and an overlay only adds; it cannot redefine it (give the model a new id)",
                model.id, self.providers[index].id
            ))),
        }
    }

    /// Add the alias `name` for `target`, unless `name` already means
    /// something else: resolution follows an alias before anything, so an
    /// alias named like a model, a `provider/model` route or another alias
    /// would take that name over. Restating an alias unchanged is accepted.
    fn add_alias(&mut self, name: String, target: String) -> EaiResult<()> {
        let refuse = |what: String| {
            EaiError::config(format!(
                "provider registry: alias `{name}` {what} and an overlay only adds; it cannot point it at `{target}` (give the alias a new name)"
            ))
        };
        if name != target {
            let model_owner = self.providers.iter().find(|provider| {
                provider
                    .models
                    .iter()
                    .any(|model| model.id == name.as_str())
            });
            if let Some(owner) = model_owner {
                return Err(refuse(format!(
                    "is the id of a model of provider `{}`",
                    owner.id
                )));
            }
            let route_owner = name.split_once('/').and_then(|(prefix, _)| {
                self.providers
                    .iter()
                    .find(|provider| provider.id == prefix.trim())
            });
            if let Some(owner) = route_owner {
                return Err(refuse(format!(
                    "names a model of provider `{}` by its `{}/` route",
                    owner.id, owner.id
                )));
            }
        }
        if let Some(existing) = self
            .aliases
            .get(&name)
            .filter(|existing| **existing != target)
        {
            return Err(refuse(format!("is already an alias for `{existing}`")));
        }
        self.aliases.insert(name, target);
        Ok(())
    }

    /// Refuse a new `provider` that reads a key variable an existing provider
    /// reads, when it would send that key to another origin. The variable
    /// holds the user's key for that provider, and nothing else should get it.
    fn refuse_a_key_sent_elsewhere(&self, provider: &ProviderRecord) -> EaiResult<()> {
        let to = origin(&provider.endpoint);
        for existing in &self.providers {
            let from = origin(&existing.endpoint);
            if from == to {
                continue;
            }
            if let Some(name) = provider
                .key_envs
                .iter()
                .find(|name| existing.key_envs.contains(name))
            {
                return Err(EaiError::config(format!(
                    "provider registry: provider `{}` reads {name}, the key of provider `{}`, and would send it to {to} instead of {from}; only an overlay named by its full path in your own environment or the data home's .env may do that (a relative path is read from the directory the harness runs in), so give the provider a key variable of its own",
                    provider.id, existing.id
                )));
            }
        }
        Ok(())
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
        let gate = ProviderGate {
            active: provider_active,
            use_first: &|_| None,
            prefer: &|_| None,
            latency: &|_| None,
            live: &|_| true,
        };
        self.auto_config_ranked(key_present, catalogue, profile, &gate)
    }

    /// [`Registry::auto_config_budgeted`] that also spends expiring credit
    /// first, as the [`ProviderGate`] answers per provider.
    ///
    /// Those providers' models lead their tier's priced models, soonest
    /// expiry first, ahead of cache and price: credit that expires unused is
    /// lost, so spending it costs nothing that would otherwise be kept. The
    /// most capable of them that is a rescue model also orchestrates, because
    /// the orchestrator makes the most calls of any model in a run. A model the
    /// catalogue does not price comes after every priced model of its tier,
    /// whatever its credit or cache.
    #[must_use]
    pub fn auto_config_ranked(
        &self,
        key_present: &dyn Fn(&str) -> bool,
        catalogue: &PriceCatalogue,
        profile: UsageProfile,
        gate: &ProviderGate<'_>,
    ) -> AutoConfig {
        let mut workers = Vec::new();
        let mut rescue = Vec::new();
        let mut unpriced = Vec::new();
        let mut failed = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for provider in &self.providers {
            let keyed = provider.key_envs.is_empty()
                || provider.key_envs.iter().any(|name| key_present(name));
            if !keyed {
                continue;
            }
            // A keyless provider is local and has no balance to gate on.
            if !provider.key_envs.is_empty() && !(gate.active)(&provider.id) {
                continue;
            }
            for model in &provider.models {
                let Some(tier) = model.tier else { continue };
                if !(gate.live)(&model.id) {
                    failed.push(model.id.clone());
                    continue;
                }
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
                // A caching model is priced with the profile's cached share
                // at its cache rate: that is what a task's turns pay.
                let price = catalogue.price(price_key);
                let cost = match price {
                    Some(price) => price.cost(profile.usage(model.capabilities.cache)),
                    None => {
                        unpriced.push(model.id.clone());
                        crate::money::MicroUsd::from_micros(u64::MAX)
                    }
                };
                let ranked = Ranked {
                    id: model.id.clone(),
                    provider: provider.id.clone(),
                    prefer: (gate.prefer)(&provider.id),
                    priced: price.is_some(),
                    cost,
                    cache: model.capabilities.cache,
                    score: u8::from(model.capabilities.streaming)
                        + u8::from(model.capabilities.vision),
                    context: model.capabilities.context.unwrap_or(0),
                    expires: (gate.use_first)(&provider.id),
                    tier,
                    band: cost_band(cost),
                    latency: (gate.latency)(&model.id),
                };
                match tier {
                    Tier::Worker => workers.push(ranked),
                    Tier::Rescue => rescue.push(ranked),
                }
            }
        }
        // Expiring credit, soonest expiry first, across both tiers: reported,
        // and it leads each tier below.
        let mut expiring = workers
            .iter()
            .chain(rescue.iter())
            .filter(|ranked| ranked.expires.is_some())
            .collect::<Vec<_>>();
        expiring.sort_by(|a, b| {
            a.expires
                .cmp(&b.expires)
                .then_with(|| a.tier.cmp(&b.tier))
                .then_with(|| b.cache.cmp(&a.cache))
                .then_with(|| a.cost.cmp(&b.cost))
                .then_with(|| a.id.cmp(&b.id))
        });
        let use_first = expiring
            .iter()
            .map(|ranked| ranked.id.clone())
            .collect::<Vec<_>>();
        // The orchestrator on expiring credit is its most capable rescue
        // model. Equal capabilities go to the dearer model: list price is
        // the best proxy for strength there is, and this credit is spent
        // either way.
        let first_orchestrator = rescue
            .iter()
            .filter(|ranked| ranked.expires.is_some())
            .max_by(|a, b| {
                a.score
                    .cmp(&b.score)
                    .then_with(|| a.context.cmp(&b.context))
                    .then_with(|| a.cost.cmp(&b.cost))
                    .then_with(|| b.id.cmp(&a.id))
            })
            .map(|ranked| ranked.id.clone());
        // The preferred providers that are on the ladder, first preferred
        // first.
        let mut preferred = workers
            .iter()
            .chain(rescue.iter())
            .filter_map(|ranked| ranked.prefer.map(|rank| (rank, ranked.provider.clone())))
            .collect::<Vec<_>>();
        preferred.sort();
        preferred.dedup();
        let preferred = preferred
            .into_iter()
            .map(|(_, provider)| provider)
            .collect::<Vec<_>>();
        // Priced first: an unpriced model is ranked at an infinite cost, but
        // the keys before price (credit, cache) once put one that declares a
        // cache ahead of every priced model without one, and tasks started on
        // a model whose spend is recorded at zero and no budget can see.
        // `fleet` moved it back afterwards; this ranking, the library's, did
        // not, against its own documentation. Then expiring credit. Then
        // the preferred providers, in the operator's order: the operator
        // knows what the ledger cannot (a contract, a region, a quota), and
        // the preference costs only the price difference it chooses. Then
        // cache, always: a cached prefix is the cheapest input a model can
        // take, and the harness re-sends the same prompt on every turn of a
        // task. Budget next. Capability decides only the rescue tier, where the
        // task is complex and the model must do more. Within a price band
        // (prices within about 10%), the rescue tier takes the more capable
        // model, then every tier takes the faster one by measured latency,
        // then the cheaper by exact price.
        let order = |list: &mut Vec<Ranked>, by_capability: bool| {
            list.sort_by(|a, b| {
                b.priced
                    .cmp(&a.priced)
                    // Expiring credit first within the tier, soonest expiry first.
                    .then_with(|| by_expiry(a.expires, b.expires))
                    .then_with(|| by_preference(a.prefer, b.prefer))
                    .then_with(|| b.cache.cmp(&a.cache))
                    .then_with(|| a.band.cmp(&b.band))
                    .then_with(|| {
                        if by_capability {
                            b.score
                                .cmp(&a.score)
                                .then_with(|| b.context.cmp(&a.context))
                        } else {
                            std::cmp::Ordering::Equal
                        }
                    })
                    .then_with(|| by_latency(a.latency, b.latency))
                    .then_with(|| a.cost.cmp(&b.cost))
                    .then_with(|| a.id.cmp(&b.id))
            });
            list.iter()
                .map(|ranked| ranked.id.clone())
                .collect::<Vec<_>>()
        };
        let workers = order(&mut workers, false);
        let rescue_ids = order(&mut rescue, true);
        // The orchestrator the preferred providers give: the first rescue
        // model of the sorted tier when it is theirs and priced, which is the
        // first preferred provider's cheapest strong model, the same rule the
        // fallback below applies to the whole fleet. An unpriced one never
        // orchestrates on a preference: its spend is invisible to every cap.
        let preferred_orchestrator = rescue
            .first()
            .filter(|ranked| ranked.prefer.is_some() && ranked.priced)
            .map(|ranked| ranked.id.clone());
        let rescue = rescue_ids;
        // The orchestrator is the strongest rescue model on expiring credit,
        // else the preferred providers' cheapest strong model, else the
        // primary when it is eligible, else the cheapest rescue model, else
        // the cheapest worker. A fleet of workers alone, or one model of any
        // tier, still has an orchestrator.
        let primary = Self::primary_orchestrator();
        let orchestrator = if first_orchestrator.is_some() {
            first_orchestrator
        } else if preferred_orchestrator.is_some() {
            preferred_orchestrator
        } else if workers.iter().chain(rescue.iter()).any(|id| id == primary) {
            Some(primary.to_string())
        } else {
            rescue.first().or_else(|| workers.first()).cloned()
        };
        AutoConfig {
            orchestrator,
            use_first,
            workers,
            rescue,
            unpriced,
            failed,
            preferred,
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

/// The optional key fields each provider of a registry document states, in
/// document order. A parsed record cannot tell an absent `key_env` from an
/// empty one, and an overlay that restates a provider without them is not
/// changing them.
fn stated_key_fields(json: &str) -> Vec<Vec<&'static str>> {
    let Ok(root) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    root.get("providers")
        .and_then(Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .map(|raw| {
                    ["key_env", "key_prefix", "key_url"]
                        .into_iter()
                        .filter(|field| raw.get(*field).is_some_and(|value| !value.is_null()))
                        .collect()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Where a request to `endpoint` goes: its scheme and authority, lower-cased.
/// The authority is everything before the path, any `user@` part included,
/// so a URL that only starts like a provider's
/// (`https://api.deepseek.com@elsewhere.example/`) is not taken for it.
fn origin(endpoint: &str) -> String {
    let (scheme, rest) = endpoint.split_once("://").unwrap_or(("", endpoint));
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    format!("{scheme}://{authority}").to_ascii_lowercase()
}

/// The fields of `existing` that the overlay's `restated` record changes;
/// the key fields count only where the overlay states them (`stated`).
fn changed_fields(
    existing: &ProviderRecord,
    restated: &ProviderRecord,
    stated: &[&str],
) -> Vec<&'static str> {
    let mut changed = Vec::new();
    if existing.wire != restated.wire {
        changed.push("wire");
    }
    if existing.endpoint != restated.endpoint {
        changed.push("endpoint");
    }
    if stated.contains(&"key_env") && existing.key_envs != restated.key_envs {
        changed.push("key_env");
    }
    if stated.contains(&"key_prefix") && existing.key_prefix != restated.key_prefix {
        changed.push("key_prefix");
    }
    if stated.contains(&"key_url") && existing.key_url != restated.key_url {
        changed.push("key_url");
    }
    if restated.models_query.is_some() && existing.models_query != restated.models_query {
        changed.push("models_query");
    }
    changed
}

/// The variables that may hold a provider's key: `key_env` as one name or a
/// list of names; absent or `null` for a local provider with no key.
///
/// Anything else is refused. A string was once read as no list at all, so a
/// keyed provider ran as local: no budget gate, and every call sent without
/// its key got 401. The refusal never repeats the entry, which may be a key
/// pasted where its variable's name belongs.
fn parse_key_envs(id: &str, raw: Option<&Value>) -> EaiResult<Vec<String>> {
    let names = match raw {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(name)) => vec![name.trim().to_string()],
        Some(Value::Array(names)) => names
            .iter()
            .map(|name| name.as_str().map(|name| name.trim().to_string()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                EaiError::config(format!(
                    "provider registry: provider `{id}` key_env must be a variable name or a list of variable names"
                ))
            })?,
        Some(_) => {
            return Err(EaiError::config(format!(
                "provider registry: provider `{id}` key_env must be a variable name or a list of variable names"
            )))
        }
    };
    if names.iter().any(|name| !crate::secrets::valid_name(name)) {
        return Err(EaiError::config(format!(
            "provider registry: provider `{id}` key_env holds an entry that is not an environment variable name (upper-case letters, digits and _); it names the variable that holds the key, not the key"
        )));
    }
    Ok(names)
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
    let key_envs = parse_key_envs(&id, raw.get("key_env"))?;
    if !key_envs.is_empty() && !susi_http_transport::client::permits_credentials(&endpoint) {
        return Err(EaiError::config(format!(
            "provider registry: keyed provider `{id}` requires HTTPS or a loopback endpoint"
        )));
    }
    let mut models = Vec::new();
    if let Some(records) = raw.get("models").and_then(Value::as_array) {
        for record in records {
            models.push(parse_model(record)?);
        }
    }
    let key_prefix = raw
        .get("key_prefix")
        .and_then(Value::as_str)
        .filter(|prefix| !prefix.is_empty())
        .map(str::to_string);
    let key_url = raw
        .get("key_url")
        .and_then(Value::as_str)
        .filter(|url| !url.is_empty())
        .map(str::to_string);
    let models_query = raw
        .get("models_query")
        .and_then(Value::as_str)
        .map(|query| query.trim().trim_start_matches('?').to_string())
        .filter(|query| !query.is_empty());
    Ok(ProviderRecord {
        id,
        wire,
        endpoint,
        key_envs,
        key_prefix,
        key_url,
        models_query,
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
      "key_url": "https://platform.deepseek.com/api_keys",
      "models": [
        {"id": "deepseek-flash", "backend": "deepseek-flash", "context": 128000, "tools": true, "streaming": true, "tier": "worker", "cache": true},
        {"id": "deepseek-pro", "backend": "deepseek-v4-pro", "context": 128000, "tools": true, "streaming": true, "tier": "rescue", "cache": true}
      ]
    },
    {
      "id": "siliconflow",
      "wire": "openai",
      "endpoint": "https://api.siliconflow.com/v1/chat/completions",
      "key_env": ["SILICONFLOW_API_KEY"],
      "key_url": "https://cloud.siliconflow.com/account/ak",
      "models_query": "type=text&sub_type=chat",
      "models": [
        {"id": "siliconflow-deepseek-flash", "backend": "deepseek-ai/DeepSeek-V4.1-Flash", "context": 1048576, "tools": true, "streaming": true, "tier": "worker", "cache": true},
        {"id": "siliconflow-glm-flash", "backend": "zai-org/GLM-5.3-Flash", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "siliconflow-qwen", "backend": "Qwen/Qwen3.8-27B", "context": 262144, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "siliconflow-qwen-coder", "backend": "Qwen/Qwen3-Coder-30B-A3B-Instruct", "context": 262144, "tools": true, "streaming": true, "tier": "worker"},
        {"id": "siliconflow-deepseek", "backend": "deepseek-ai/DeepSeek-V3.2", "context": 163840, "tools": true, "streaming": true, "tier": "rescue", "cache": true},
        {"id": "siliconflow-deepseek-pro", "backend": "deepseek-ai/DeepSeek-V4-Pro", "context": 1048576, "tools": true, "streaming": true, "tier": "rescue", "cache": true},
        {"id": "siliconflow-glm", "backend": "zai-org/GLM-5.3", "context": 1048576, "tools": true, "streaming": true, "tier": "rescue", "cache": true},
        {"id": "siliconflow-kimi-k3", "backend": "moonshotai/Kimi-K3", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true}
      ]
    },
    {
      "id": "deepinfra",
      "wire": "openai",
      "endpoint": "https://api.deepinfra.com/v1/openai/chat/completions",
      "key_env": ["DEEPINFRA_API_KEY"],
      "key_url": "https://deepinfra.com/dash/api_keys",
      "models": [
        {"id": "deepinfra-deepseek-flash", "backend": "deepseek-ai/DeepSeek-V4.1-Flash", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "deepinfra-qwen-flash", "backend": "Qwen/Qwen3.8-Flash", "context": 1000000, "tools": true, "streaming": true, "tier": "worker", "cache": true},
        {"id": "deepinfra-glm-flash", "backend": "zai-org/GLM-5.3-Flash", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "deepinfra-llama", "backend": "meta-llama/Llama-3.3-70B-Instruct-Turbo", "context": 131072, "tools": true, "streaming": true, "tier": "worker"},
        {"id": "deepinfra-qwen-coder", "backend": "Qwen/Qwen3-Coder-480B-A35B-Instruct-Turbo", "context": 262144, "tools": true, "streaming": true, "tier": "worker", "cache": true},
        {"id": "deepinfra-deepseek", "backend": "deepseek-ai/DeepSeek-V3.2", "context": 163840, "tools": true, "streaming": true, "tier": "rescue", "cache": true},
        {"id": "deepinfra-deepseek-pro", "backend": "deepseek-ai/DeepSeek-V4-Pro", "context": 1048576, "tools": true, "streaming": true, "tier": "rescue", "cache": true},
        {"id": "deepinfra-kimi-k3", "backend": "moonshotai/Kimi-K3", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true}
      ]
    },
    {
      "id": "dashscope",
      "wire": "openai",
      "endpoint": "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions",
      "key_env": ["DASHSCOPE_API_KEY"],
      "key_url": "https://dashscope.console.aliyun.com/apiKey",
      "models": [
        {"id": "dashscope-qwen-plus", "backend": "qwen-plus", "context": 131072, "tools": true, "streaming": true, "tier": "worker"},
        {"id": "dashscope-qwen-max", "backend": "qwen-max", "context": 32768, "tools": true, "streaming": true, "tier": "rescue"},
        {"id": "dashscope-qwen-coder", "backend": "qwen2.5-coder-32b-instruct", "context": 32768, "tools": true, "streaming": true, "tier": "worker"}
      ]
    },
    {
      "id": "gemini",
      "wire": "openai",
      "endpoint": "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
      "key_env": ["GEMINI_API_KEY", "GOOGLE_API_KEY"],
      "key_url": "https://aistudio.google.com/apikey",
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
      "key_url": "https://platform.moonshot.ai/console/api-keys",
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
      "key_url": "https://console.anthropic.com/settings/keys",
      "key_prefix": "sk-ant-",
      "models": [
        {"id": "claude-haiku", "backend": "claude-haiku-5-5", "context": 1000000, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "claude-sonnet", "backend": "claude-sonnet-5-5", "context": 1000000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true},
        {"id": "claude-opus", "backend": "claude-opus-5-5", "context": 1000000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true}
      ]
    },
    {
      "id": "openrouter",
      "wire": "openai",
      "endpoint": "https://openrouter.ai/api/v1/chat/completions",
      "key_env": ["OPENROUTER_API_KEY"],
      "key_url": "https://openrouter.ai/settings/keys",
      "models": [
        {"id": "or-deepseek-flash", "backend": "deepseek/deepseek-v4.1-flash", "context": 1048576, "tools": true, "streaming": true, "tier": "worker", "cache": true},
        {"id": "or-qwen-flash", "backend": "qwen/qwen3.8-flash", "context": 1000000, "tools": true, "streaming": true, "tier": "worker", "cache": true},
        {"id": "or-gemini-flash", "backend": "google/gemini-3.8-flash", "context": 1048576, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "or-claude-sonnet", "backend": "anthropic/claude-sonnet-5.5", "context": 1000000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true},
        {"id": "or-gpt-sol", "backend": "openai/gpt-6.1-sol", "context": 1050000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true},
        {"id": "or-claude-opus", "backend": "anthropic/claude-opus-5.5", "context": 1000000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true}
      ]
    },
    {
      "id": "openai",
      "wire": "openai",
      "endpoint": "https://api.openai.com/v1/chat/completions",
      "key_env": ["OPENAI_API_KEY"],
      "key_url": "https://platform.openai.com/api-keys",
      "key_prefix": "sk-",
      "models": [
        {"id": "gpt-luna", "backend": "gpt-6-luna", "context": 1050000, "tools": true, "streaming": true, "vision": true, "tier": "worker", "cache": true},
        {"id": "gpt-sol", "backend": "gpt-6.1-sol", "context": 1050000, "tools": true, "streaming": true, "vision": true, "tier": "rescue", "cache": true}
      ]
    }
  ]
}
"#;

#[cfg(test)]
#[path = "tests/providers.rs"]
mod tests;
