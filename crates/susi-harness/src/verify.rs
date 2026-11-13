//! Verify the provider registry against the model lists upstream serves.
//!
//! Model ids drift: a backend that was valid when the registry was compiled can
//! be retired upstream, and a chat call is then the first thing that finds out.
//! This module asks each selected provider's models endpoint what it serves and
//! compares the registry's `backend` ids with the answer.
//!
//! Parsing and comparison are pure; the HTTP GET is injected as a closure, so
//! every branch — present, missing, non-2xx, unreadable body — is testable
//! without a server. Only the command path supplies [`fetch_http`].
//!
//! One naming quirk is normalized: Google's list spells ids `models/<id>` while
//! its OpenAI-compatible chat route takes the bare id, so the comparison accepts
//! either spelling of the same name as present.

use crate::providers::{ProviderRecord, Registry, Wire};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeSet;
use susi_error::{EaiError, EaiResult};

/// Bound on one models-endpoint GET, seconds.
pub const TIMEOUT_SECS: u64 = 10;

/// Largest models response read, bytes. An OpenRouter list is the biggest the
/// builtin registry reaches and fits well inside this.
pub const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The injected HTTP fetch: `(url, headers)` → `(status, body)`.
///
/// The full header set travels with the request, because the wires do not agree
/// on how a key travels: OpenAI-compatible hosts take `authorization: Bearer`,
/// while the Anthropic Messages API takes `x-api-key` plus `anthropic-version`
/// — and a key that is not workspace-scoped also needs
/// `anthropic-workspace-id`. One injection point covers both.
///
/// An `Err` means the request never produced a response; an HTTP error status
/// is a successful fetch of `(status, body)` so the caller can report it.
pub type Fetch<'a> = dyn Fn(&str, &[(&str, &str)]) -> Result<(u16, String), String> + 'a;

/// One provider's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderReport {
    /// Registry id the entry is about.
    pub provider: String,
    /// True when the provider was verified, or deliberately skipped.
    pub ok: bool,
    /// Configured backends the upstream list does not advertise.
    pub missing: Vec<String>,
    /// True when the models endpoint answered 2xx.
    pub reachable: bool,
    /// Human-readable explanation of the outcome.
    pub note: String,
}

/// The report `susi harness models --verify` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyReport {
    /// True when every selected provider verified or was skipped.
    pub ok: bool,
    /// One entry per selected provider, in registry order.
    pub providers: Vec<ProviderReport>,
}

/// The models URL for a provider, when its wire has one.
///
/// OpenAI-compatible endpoints put the model list beside the chat route, so
/// `{base}/chat/completions` becomes `{base}/models` — which also yields the
/// documented Gemini OpenAI-compat and OpenRouter lists from the registry's own
/// endpoints. The Anthropic Messages API publishes its list beside the messages
/// route the same way: `{base}/messages` becomes `{base}/models`.
#[must_use]
pub fn models_endpoint(provider: &ProviderRecord) -> Option<String> {
    let suffix = match provider.wire {
        Wire::OpenAi => "/chat/completions",
        Wire::Anthropic => "/messages",
    };
    provider
        .endpoint
        .strip_suffix(suffix)
        .filter(|base| !base.is_empty())
        .map(|base| format!("{base}/models"))
}

/// The set of backend ids the registry expects upstream to serve.
#[must_use]
pub fn configured_backends(provider: &ProviderRecord) -> BTreeSet<String> {
    provider
        .models
        .iter()
        .map(|model| model.backend.clone())
        .collect()
}

/// Read an OpenAI-shaped models response into the set of advertised ids.
///
/// # Errors
///
/// Returns a protocol error when the body is not JSON or has no `data` array.
pub fn parse_upstream_ids(body: &str) -> EaiResult<BTreeSet<String>> {
    let root: Value = serde_json::from_str(body)
        .map_err(|error| EaiError::protocol(format!("models response is not JSON: {error}")))?;
    let data = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| EaiError::protocol("models response has no data array"))?;
    let mut ids = BTreeSet::new();
    for entry in data {
        if let Some(id) = entry.get("id").and_then(Value::as_str) {
            let id = id.trim();
            if !id.is_empty() {
                ids.insert(id.to_string());
            }
        }
    }
    Ok(ids)
}

/// Check one provider against the live list `fetch` returns.
///
/// A provider with no derivable models endpoint — Anthropic, or a custom
/// OpenAI-compatible endpoint that does not end in `/chat/completions` — is
/// skipped with `ok: true` and a `not verifiable` note, because nothing can be
/// compared. An unset key, an unreachable endpoint, a non-2xx status and an
/// unreadable body are all `ok: false`: the registry was not verified.
#[must_use]
pub fn check_provider(provider: &ProviderRecord, fetch: &Fetch<'_>) -> ProviderReport {
    let provider_id = provider.id.clone();
    let Some(url) = models_endpoint(provider) else {
        return ProviderReport {
            provider: provider_id,
            ok: true,
            missing: Vec::new(),
            reachable: false,
            note: not_verifiable(provider),
        };
    };
    let key = match key_for(provider) {
        Ok(key) => key,
        Err(note) => {
            return ProviderReport {
                provider: provider_id,
                ok: false,
                missing: Vec::new(),
                reachable: false,
                note,
            };
        }
    };
    let headers = models_headers(provider, key.as_deref());
    let pairs = headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let (status, body) = match fetch(&url, &pairs) {
        Ok(response) => response,
        Err(error) => {
            return ProviderReport {
                provider: provider_id,
                ok: false,
                missing: Vec::new(),
                reachable: false,
                note: format!("{url} could not be reached: {error}"),
            };
        }
    };
    if !(200..300).contains(&status) {
        return ProviderReport {
            provider: provider_id,
            ok: false,
            missing: Vec::new(),
            reachable: false,
            note: format!("{url} returned HTTP {status}"),
        };
    }
    let upstream = match parse_upstream_ids(&body) {
        Ok(ids) => ids,
        Err(error) => {
            return ProviderReport {
                provider: provider_id,
                ok: false,
                missing: Vec::new(),
                reachable: true,
                note: format!("{url} returned an unreadable model list: {error}"),
            };
        }
    };
    let configured = configured_backends(provider);
    let missing = configured
        .iter()
        .filter(|backend| !advertised(&upstream, backend))
        .cloned()
        .collect::<Vec<_>>();
    let ok = missing.is_empty();
    let note = if ok {
        format!(
            "all {} configured model(s) present upstream ({} advertised)",
            configured.len(),
            upstream.len()
        )
    } else {
        format!(
            "{} of {} configured model(s) absent upstream ({} advertised)",
            missing.len(),
            configured.len(),
            upstream.len()
        )
    };
    ProviderReport {
        provider: provider_id,
        ok,
        missing,
        reachable: true,
        note,
    }
}

/// Verify every selected provider (or just `provider`) against the live lists.
///
/// # Errors
///
/// Returns a config error when `provider` names an id the registry does not
/// have.
pub fn verify_registry(
    registry: &Registry,
    provider: Option<&str>,
    fetch: &Fetch<'_>,
) -> EaiResult<VerifyReport> {
    let selected: Vec<&ProviderRecord> = match provider {
        Some(id) => {
            let found = registry
                .providers()
                .iter()
                .filter(|candidate| candidate.id == id)
                .collect::<Vec<_>>();
            if found.is_empty() {
                let known = registry
                    .providers()
                    .iter()
                    .map(|candidate| candidate.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(EaiError::config(format!(
                    "provider `{id}` is not in the registry (known: {known})"
                )));
            }
            found
        }
        None => registry.providers().iter().collect(),
    };
    let providers = selected
        .into_iter()
        .map(|record| check_provider(record, fetch))
        .collect::<Vec<_>>();
    let ok = providers.iter().all(|entry| entry.ok);
    Ok(VerifyReport { ok, providers })
}

/// One model a provider serves that the registry does not know, as the
/// overlay record that would add it ([`discover`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredModel {
    /// The registry id the overlay gives it: the provider id, a dash, and the
    /// backend's own name in lower case with runs of punctuation as one dash.
    pub id: String,
    /// The id the provider takes on the wire.
    pub backend: String,
    /// Context window, when the provider publishes it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<u64>,
    /// Tool calling. Taken as given for a chat model: every chat model the
    /// two providers that publish capability data serve takes tools, and the
    /// first real call that is refused takes the model off the ladder on its
    /// own ([`crate::policy::failed_models`]).
    pub tools: bool,
    /// Streaming, taken as given for an OpenAI-compatible chat model.
    pub streaming: bool,
    /// Image input, when the provider says so (DeepInfra's `vision` tag).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub vision: bool,
    /// A prompt cache, when the provider says so (DeepInfra's `prompt_cache`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub cache: bool,
    /// `rescue` when the output costs a dollar or more per million tokens,
    /// else `worker`; a model without a price is a worker, priced last.
    pub tier: String,
}

/// What one provider's live list adds to the registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredProvider {
    /// Registry id.
    pub id: String,
    /// Restated unchanged, so the document is a valid overlay.
    pub wire: String,
    /// Restated unchanged.
    pub endpoint: String,
    /// Restated unchanged.
    pub key_env: Vec<String>,
    /// The models the provider lists and the registry does not.
    pub models: Vec<DiscoveredModel>,
}

/// The document `models --discover` prints: an overlay that adds every chat
/// model the selected keyed providers serve beyond the registry, the prices
/// the providers publish for them, and one note per provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Discovery {
    /// One entry per provider that had something to add.
    pub providers: Vec<DiscoveredProvider>,
    /// Catalogue entries, in the price file's shape, for the discovered
    /// models whose provider publishes prices (micro-USD per million tokens).
    pub prices: std::collections::BTreeMap<String, crate::pricing::ModelPrice>,
    /// What happened at each selected provider, keyed or not.
    pub notes: Vec<String>,
}

/// Ask each selected keyed provider what it serves beyond the registry.
///
/// The list is read from the provider's models endpoint, narrowed to the chat
/// models by the provider's `models_query` where the registry has one
/// (SiliconFlow) or by a `chat` tag in each entry's metadata where the
/// provider publishes one (DeepInfra); a provider that does neither is
/// reported as a count, with nothing added, because an image or a speech
/// model on a worker ladder helps nobody. Each new model gets a registry id
/// of its own, a tier from its output price, and, where the provider
/// publishes prices and a context window, those too. A model whose id the
/// registry already uses, as a model or an alias, is skipped and named.
///
/// The document is an overlay: save it and name it in
/// [`crate::providers::OVERLAY_ENV`]; copy its `prices` into the catalogue
/// to price the models it adds. It is a proposal to review, not a change the
/// harness makes on its own: the live list is the provider's word about what
/// it serves, not about what is worth running.
///
/// # Errors
///
/// Returns a config error when `provider` names an id the registry does not
/// have.
pub fn discover(
    registry: &Registry,
    provider: Option<&str>,
    fetch: &Fetch<'_>,
) -> EaiResult<Discovery> {
    let selected: Vec<&ProviderRecord> = match provider {
        Some(id) => {
            let found = registry
                .providers()
                .iter()
                .filter(|candidate| candidate.id == id)
                .collect::<Vec<_>>();
            if found.is_empty() {
                let known = registry
                    .providers()
                    .iter()
                    .map(|candidate| candidate.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(EaiError::config(format!(
                    "provider `{id}` is not in the registry (known: {known})"
                )));
            }
            found
        }
        None => registry.providers().iter().collect(),
    };
    let mut taken: BTreeSet<String> = registry.aliases().keys().cloned().collect();
    for record in registry.providers() {
        for model in &record.models {
            taken.insert(model.id.clone());
        }
    }
    let mut discovery = Discovery {
        providers: Vec::new(),
        prices: std::collections::BTreeMap::new(),
        notes: Vec::new(),
    };
    for record in selected {
        let keyed = record.key_envs.is_empty()
            || record
                .key_envs
                .iter()
                .any(|name| crate::secrets::is_set(name));
        if !keyed && provider.is_none() {
            discovery
                .notes
                .push(format!("{}: skipped, no key", record.id));
            continue;
        }
        let Some(mut url) = models_endpoint(record) else {
            discovery
                .notes
                .push(format!("{}: {}", record.id, not_verifiable(record)));
            continue;
        };
        if let Some(query) = &record.models_query {
            url.push('?');
            url.push_str(query);
        }
        let key = match key_for(record) {
            Ok(key) => key,
            Err(note) => {
                discovery.notes.push(format!("{}: {note}", record.id));
                continue;
            }
        };
        let headers = models_headers(record, key.as_deref());
        let pairs = headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        let body = match fetch(&url, &pairs) {
            Ok((status, body)) if (200..300).contains(&status) => body,
            Ok((status, _)) => {
                discovery
                    .notes
                    .push(format!("{}: {url} returned HTTP {status}", record.id));
                continue;
            }
            Err(error) => {
                discovery.notes.push(format!(
                    "{}: {url} could not be reached: {error}",
                    record.id
                ));
                continue;
            }
        };
        let entries = match parse_upstream_entries(&body) {
            Ok(entries) => entries,
            Err(error) => {
                discovery.notes.push(format!(
                    "{}: {url} returned an unreadable model list: {error}",
                    record.id
                ));
                continue;
            }
        };
        let configured = configured_backends(record);
        let tagged = entries.iter().any(|entry| entry.tags.is_some());
        if record.models_query.is_none() && !tagged {
            discovery.notes.push(format!(
                "{}: lists {} model(s) but publishes neither a chat filter nor capability data; nothing added (route one as {}/<id>)",
                record.id,
                entries.len(),
                record.id
            ));
            continue;
        }
        let mut models = Vec::new();
        let mut skipped = Vec::new();
        for entry in entries {
            if configured.contains(&entry.id) || advertised(&configured, &entry.id) {
                continue;
            }
            if let Some(tags) = &entry.tags {
                if !tags.iter().any(|tag| tag == "chat") {
                    continue;
                }
            }
            let id = format!("{}-{}", record.id, slug(&entry.id));
            if !taken.insert(id.clone()) {
                skipped.push(entry.id.clone());
                continue;
            }
            let tier = match entry.price {
                Some(price) if price.output.micros() >= 1_000_000 => "rescue",
                _ => "worker",
            };
            if let Some(price) = entry.price {
                discovery.prices.insert(id.clone(), price);
            }
            let tags = entry.tags.unwrap_or_default();
            models.push(DiscoveredModel {
                id,
                backend: entry.id,
                context: entry.context,
                tools: true,
                streaming: true,
                vision: tags.iter().any(|tag| tag == "vision"),
                cache: tags.iter().any(|tag| tag == "prompt_cache"),
                tier: tier.to_string(),
            });
        }
        let mut note = format!(
            "{}: {} configured, {} more chat model(s) listed",
            record.id,
            configured.len(),
            models.len()
        );
        if !skipped.is_empty() {
            note.push_str(&format!(
                "; {} skipped, their registry id is taken: {}",
                skipped.len(),
                skipped.join(", ")
            ));
        }
        discovery.notes.push(note);
        if !models.is_empty() {
            discovery.providers.push(DiscoveredProvider {
                id: record.id.clone(),
                wire: match record.wire {
                    Wire::OpenAi => "openai".to_string(),
                    Wire::Anthropic => "anthropic".to_string(),
                },
                endpoint: record.endpoint.clone(),
                key_env: record.key_envs.clone(),
                models,
            });
        }
    }
    Ok(discovery)
}

/// One entry of a provider's model list, with what it publishes about itself.
#[derive(Debug, Clone, PartialEq)]
struct UpstreamEntry {
    id: String,
    /// `metadata.tags`, where the provider publishes them (DeepInfra).
    tags: Option<Vec<String>>,
    /// `metadata.context_length`, where published.
    context: Option<u64>,
    /// `metadata.pricing`, where published, in micro-USD per million tokens.
    price: Option<crate::pricing::ModelPrice>,
}

/// Read an OpenAI-shaped models response with whatever each entry says about
/// itself: DeepInfra's `metadata` carries tags, a context length and prices
/// in dollars per million tokens; a plain list carries ids alone.
fn parse_upstream_entries(body: &str) -> EaiResult<Vec<UpstreamEntry>> {
    let root: Value = serde_json::from_str(body)
        .map_err(|error| EaiError::protocol(format!("models response is not JSON: {error}")))?;
    let data = root
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| EaiError::protocol("models response has no data array"))?;
    let mut entries = Vec::new();
    for entry in data {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() {
            continue;
        }
        let metadata = entry.get("metadata");
        let tags = metadata
            .and_then(|metadata| metadata.get("tags"))
            .and_then(Value::as_array)
            .map(|tags| {
                tags.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            });
        let context = metadata
            .and_then(|metadata| metadata.get("context_length"))
            .and_then(Value::as_u64)
            .filter(|context| *context > 0);
        let price = metadata
            .and_then(|metadata| metadata.get("pricing"))
            .and_then(published_price);
        entries.push(UpstreamEntry {
            id: id.to_string(),
            tags,
            context,
            price,
        });
    }
    Ok(entries)
}

/// A published `pricing` object (`input_tokens`, `output_tokens`, an optional
/// `cache_read_tokens`, dollars per million) as a catalogue price. Rounded up
/// to the micro-dollar; a cache rate that is absent is the input rate.
fn published_price(pricing: &Value) -> Option<crate::pricing::ModelPrice> {
    let per_million = |field: &str| -> Option<crate::money::MicroUsd> {
        let usd = pricing.get(field)?.as_f64()?;
        if !usd.is_finite() || usd < 0.0 || usd > 1.0e6 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let micros = (usd * 1_000_000.0).ceil() as u64;
        Some(crate::money::MicroUsd::from_micros(micros))
    };
    let input_miss = per_million("input_tokens")?;
    let output = per_million("output_tokens")?;
    let input_hit = per_million("cache_read_tokens").unwrap_or(input_miss);
    Some(crate::pricing::ModelPrice {
        input_miss,
        input_hit,
        output,
    })
}

/// A backend id as a registry id suffix: lower case, every run of characters
/// other than letters and digits as one dash, none at either end
/// (`deepseek-ai/DeepSeek-V4.1-Flash` → `deepseek-ai-deepseek-v4-1-flash`).
fn slug(backend: &str) -> String {
    let mut slug = String::with_capacity(backend.len());
    for character in backend.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

/// The headers a models request needs for this provider's wire.
///
/// Anthropic needs three: the key, the API version, and — when the key is a
/// console user key rather than a workspace-scoped one — the workspace it must
/// act in. `ANTHROPIC_WORKSPACE_ID` supplies the last; unset sends nothing,
/// which is what a workspace-scoped key expects.
fn models_headers(provider: &ProviderRecord, key: Option<&str>) -> Vec<(String, String)> {
    let mut headers = vec![("accept".to_string(), "application/json".to_string())];
    let Some(key) = key else {
        return headers;
    };
    match provider.wire {
        Wire::Anthropic => {
            headers.push(("x-api-key".to_string(), key.to_string()));
            headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
            if let Some(workspace) = anthropic_workspace() {
                headers.push(("anthropic-workspace-id".to_string(), workspace));
            }
        }
        Wire::OpenAi => headers.push(("authorization".to_string(), format!("Bearer {key}"))),
    }
    headers
}

/// `ANTHROPIC_WORKSPACE_ID`, trimmed, when it names a workspace.
fn anthropic_workspace() -> Option<String> {
    crate::secrets::lookup("ANTHROPIC_WORKSPACE_ID")
        .map(|value| value.expose().trim().to_string())
        .filter(|value| !value.is_empty())
}

/// GET a models endpoint through the shared HTTP transport.
///
/// Non-2xx statuses are returned, not raised: the caller reports the status it
/// got instead of pretending the request failed.
///
/// # Errors
///
/// Returns the transport error string when no response is produced, or when the
/// body exceeds [`MAX_BYTES`].
pub fn fetch_http(url: &str, headers: &[(&str, &str)]) -> Result<(u16, String), String> {
    let call = susi_http_transport::http_call("GET", url, headers, TIMEOUT_SECS, 0)?;
    let status = call.status;
    let body = call.into_utf8(MAX_BYTES)?;
    Ok((status, body))
}

/// Whether the upstream list advertises `backend`.
///
/// Google's model list names every entry `models/<id>` while its
/// OpenAI-compatible chat route takes the bare id, so either spelling of the
/// same name matches; any other spelling is drift.
fn advertised(upstream: &BTreeSet<String>, backend: &str) -> bool {
    upstream.contains(backend) || upstream.contains(&format!("models/{backend}"))
}

fn not_verifiable(provider: &ProviderRecord) -> String {
    match provider.wire {
        Wire::Anthropic => format!(
            "not verifiable: no models URL is derivable from endpoint {} (expected .../messages)",
            provider.endpoint
        ),
        Wire::OpenAi => format!(
            "not verifiable: no models URL is derivable from endpoint {} (expected .../chat/completions)",
            provider.endpoint
        ),
    }
}

fn key_for(provider: &ProviderRecord) -> Result<Option<String>, String> {
    if provider.key_envs.is_empty() {
        return Ok(None);
    }
    for name in &provider.key_envs {
        if let Some(key) = crate::secrets::lookup(name) {
            return Ok(Some(key.expose().to_string()));
        }
    }
    Err(format!(
        "no key in {}; set one to verify this provider",
        provider.key_envs.join(" / ")
    ))
}

#[cfg(test)]
#[path = "tests/verify.rs"]
mod tests;
