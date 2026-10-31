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
    std::env::var("ANTHROPIC_WORKSPACE_ID")
        .ok()
        .map(|value| value.trim().to_string())
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
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    const TEST_REGISTRY: &str = r#"{
      "providers": [
        {
          "id": "alpha",
          "wire": "openai",
          "endpoint": "https://alpha.example/v1/chat/completions",
          "key_env": [],
          "models": [
            {"id": "alpha-one", "backend": "alpha-1"},
            {"id": "alpha-two", "backend": "retired-2"}
          ]
        },
        {
          "id": "beta",
          "wire": "openai",
          "endpoint": "https://beta.example/v1/chat/completions",
          "key_env": [],
          "models": [{"id": "beta-one", "backend": "beta-1"}]
        },
        {
          "id": "closed",
          "wire": "anthropic",
          "endpoint": "https://closed.example/v1/messages",
          "key_env": [],
          "models": [{"id": "claude", "backend": "claude-x"}]
        },
        {
          "id": "keyed",
          "wire": "openai",
          "endpoint": "https://keyed.example/v1/chat/completions",
          "key_env": ["SUSI_HARNESS_VERIFY_KEY_7F3A9C_UNSET"],
          "models": [{"id": "keyed-one", "backend": "keyed-1"}]
        }
      ]
    }"#;

    fn test_registry() -> Registry {
        Registry::parse(TEST_REGISTRY).unwrap()
    }

    #[test]
    fn gemini_and_openrouter_lists_derive_from_their_chat_endpoints() {
        let registry = Registry::builtin();
        let provider = |id: &str| {
            registry
                .providers()
                .iter()
                .find(|candidate| candidate.id == id)
                .unwrap()
        };
        assert_eq!(
            models_endpoint(provider("gemini")).as_deref(),
            Some("https://generativelanguage.googleapis.com/v1beta/openai/models")
        );
        assert_eq!(
            models_endpoint(provider("openrouter")).as_deref(),
            Some("https://openrouter.ai/api/v1/models")
        );
        assert_eq!(
            models_endpoint(provider("anthropic")).as_deref(),
            Some("https://api.anthropic.com/v1/models"),
            "the Messages API publishes its list beside /messages"
        );
    }

    #[test]
    fn parse_upstream_ids_reads_openai_data_and_ignores_nameless_entries() {
        let ids = parse_upstream_ids(
            r#"{"object":"list","data":[{"id":"a"},{"id":" b "},{"object":"model"},{"id":""}]}"#,
        )
        .unwrap();
        assert_eq!(ids.iter().cloned().collect::<Vec<_>>(), vec!["a", "b"]);
    }

    #[test]
    fn a_configured_backend_absent_upstream_is_drift() {
        let fetch = |_: &str, _: &[(&str, &str)]| {
            Ok((
                200,
                r#"{"data":[{"id":"alpha-1"},{"id":"alpha-3"}]}"#.to_string(),
            ))
        };
        let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
        assert!(!report.ok);
        let alpha = &report.providers[0];
        assert_eq!(alpha.missing, vec!["retired-2"]);
        assert!(alpha.reachable);
        assert!(!alpha.ok);
        assert!(alpha.note.contains("1 of 2"), "{}", alpha.note);
    }

    #[test]
    fn every_configured_backend_present_verifies() {
        let fetch = |_: &str, _: &[(&str, &str)]| {
            Ok((
                200,
                r#"{"data":[{"id":"alpha-1"},{"id":"retired-2"}]}"#.to_string(),
            ))
        };
        let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
        assert!(report.ok);
        assert!(report.providers[0].ok);
        assert!(report.providers[0].missing.is_empty());
        assert!(
            report.providers[0].note.contains("all 2"),
            "{}",
            report.providers[0].note
        );
    }

    #[test]
    fn google_namespaced_ids_match_the_bare_backend() {
        // Gemini's endpoint needs a key; the comparison under test does not, so
        // the provider is declared without one rather than reading the host env.
        let registry = Registry::parse(
            r#"{"providers":[{
              "id":"googleish",
              "wire":"openai",
              "endpoint":"https://generativelanguage.googleapis.com/v1beta/openai/chat/completions",
              "key_env":[],
              "models":[{"id":"flash","backend":"gemini-3.8-flash"}]
            }]}"#,
        )
        .unwrap();
        let fetch = |_: &str, _: &[(&str, &str)]| {
            Ok((
                200,
                r#"{"data":[{"id":"models/gemini-3.8-flash"}]}"#.to_string(),
            ))
        };
        let report = verify_registry(&registry, Some("googleish"), &fetch).unwrap();
        assert!(report.ok);
        assert!(report.providers[0].missing.is_empty());
    }

    #[test]
    fn a_non_2xx_response_is_not_a_verification() {
        let fetch = |_: &str, _: &[(&str, &str)]| Ok((404, "not found".to_string()));
        let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
        assert!(!report.ok);
        let alpha = &report.providers[0];
        assert!(!alpha.reachable);
        assert!(!alpha.ok);
        assert!(alpha.note.contains("HTTP 404"), "{}", alpha.note);
    }

    #[test]
    fn an_unreadable_list_is_not_a_verification() {
        let fetch = |_: &str, _: &[(&str, &str)]| Ok((200, "<html>".to_string()));
        let report = verify_registry(&test_registry(), Some("alpha"), &fetch).unwrap();
        let alpha = &report.providers[0];
        assert!(alpha.reachable);
        assert!(!alpha.ok);
        assert!(alpha.note.contains("unreadable"), "{}", alpha.note);
    }

    /// The registry's own `closed` provider names no key, so the Anthropic tests
    /// carry their own: a list request without a key is not an Anthropic
    /// request at all.
    const ANTHROPIC_TEST_KEY: &str = "SUSI_HARNESS_VERIFY_ANTHROPIC_KEY";

    fn anthropic_registry(backend: &str) -> Registry {
        Registry::parse(&format!(
            r#"{{"providers":[{{"id":"closed","wire":"anthropic",
                "endpoint":"https://closed.example/v1/messages",
                "key_env":["{ANTHROPIC_TEST_KEY}"],
                "models":[{{"id":"claude","backend":"{backend}"}}]}}]}}"#
        ))
        .unwrap()
    }

    fn with_anthropic_key<T>(body: impl FnOnce() -> T) -> T {
        std::env::set_var(ANTHROPIC_TEST_KEY, "test-key");
        let out = body();
        std::env::remove_var(ANTHROPIC_TEST_KEY);
        out
    }

    #[test]
    fn anthropic_is_verified_through_its_models_endpoint() {
        let seen = RefCell::new(None::<(String, Vec<(String, String)>)>);
        let fetch = |url: &str, headers: &[(&str, &str)]| {
            seen.borrow_mut().replace((
                url.to_string(),
                headers
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect(),
            ));
            Ok((
                200,
                r#"{"data":[{"id":"claude-x"},{"id":"claude-new"}]}"#.to_string(),
            ))
        };
        let report = with_anthropic_key(|| {
            verify_registry(&anthropic_registry("claude-x"), Some("closed"), &fetch).unwrap()
        });
        assert!(report.ok, "{:?}", report.providers);
        assert!(report.providers[0].reachable);
        assert!(report.providers[0].missing.is_empty());

        let (url, headers) = seen.borrow().clone().expect("the fetch must have run");
        assert_eq!(
            url, "https://closed.example/v1/models",
            "the list sits beside /messages"
        );
        assert!(
            headers.iter().any(|(name, _)| name == "x-api-key"),
            "an Anthropic list needs the key in x-api-key: {headers:?}"
        );
        assert!(
            headers
                .iter()
                .any(|(name, value)| name == "anthropic-version" && value == "2023-06-01"),
            "{headers:?}"
        );
    }

    /// A console user key is refused unless the request names its workspace, so
    /// the list request must carry the header when it is configured.
    #[test]
    fn an_anthropic_workspace_travels_with_the_list_request() {
        let previous = std::env::var("ANTHROPIC_WORKSPACE_ID").ok();
        std::env::set_var("ANTHROPIC_WORKSPACE_ID", " wrkspc_test ");
        let workspace = RefCell::new(None::<String>);
        let fetch = |_: &str, headers: &[(&str, &str)]| {
            if let Some((_, value)) = headers
                .iter()
                .find(|(name, _)| *name == "anthropic-workspace-id")
            {
                workspace.borrow_mut().replace((*value).to_string());
            }
            Ok((200, r#"{"data":[{"id":"claude-x"}]}"#.to_string()))
        };
        with_anthropic_key(|| {
            verify_registry(&anthropic_registry("claude-x"), Some("closed"), &fetch).unwrap()
        });
        assert_eq!(
            workspace.borrow().as_deref(),
            Some("wrkspc_test"),
            "the configured workspace must be trimmed and sent"
        );
        std::env::remove_var("ANTHROPIC_WORKSPACE_ID");
        if let Some(value) = previous {
            std::env::set_var("ANTHROPIC_WORKSPACE_ID", value);
        }
    }

    #[test]
    fn anthropic_without_a_messages_suffix_stays_not_verifiable() {
        let registry = Registry::parse(
            r#"{"providers":[{"id":"odd","wire":"anthropic","endpoint":"https://odd.example/v1/chat",
                "key_env":[],"models":[{"id":"m","backend":"m"}]}]}"#,
        )
        .unwrap();
        let calls = Cell::new(0);
        let fetch = |_: &str, _: &[(&str, &str)]| {
            calls.set(calls.get() + 1);
            Ok((200, "{}".to_string()))
        };
        let report = verify_registry(&registry, Some("odd"), &fetch).unwrap();
        assert_eq!(calls.get(), 0);
        assert!(report.ok);
        assert!(!report.providers[0].reachable);
        assert!(
            report.providers[0].note.contains("not verifiable"),
            "{}",
            report.providers[0].note
        );
    }

    #[test]
    fn a_provider_without_its_key_is_not_verified() {
        let calls = Cell::new(0);
        let fetch = |_: &str, _: &[(&str, &str)]| {
            calls.set(calls.get() + 1);
            Ok((200, "{}".to_string()))
        };
        let report = verify_registry(&test_registry(), Some("keyed"), &fetch).unwrap();
        assert_eq!(calls.get(), 0);
        let keyed = &report.providers[0];
        assert!(!keyed.ok);
        assert!(!keyed.reachable);
        assert!(keyed.note.contains("no key"), "{}", keyed.note);
        assert!(
            keyed.note.contains("SUSI_HARNESS_VERIFY_KEY_7F3A9C_UNSET"),
            "{}",
            keyed.note
        );
    }

    #[test]
    fn selecting_one_provider_fetches_only_its_endpoint() {
        let urls = RefCell::new(Vec::new());
        let fetch = |url: &str, _: &[(&str, &str)]| {
            urls.borrow_mut().push(url.to_string());
            Ok((200, r#"{"data":[{"id":"beta-1"}]}"#.to_string()))
        };
        let report = verify_registry(&test_registry(), Some("beta"), &fetch).unwrap();
        assert!(report.ok);
        assert_eq!(report.providers.len(), 1);
        assert_eq!(report.providers[0].provider, "beta");
        assert_eq!(
            urls.borrow().as_slice(),
            ["https://beta.example/v1/models".to_string()]
        );
    }

    #[test]
    fn selecting_an_unknown_provider_is_refused() {
        let fetch = |_: &str, _: &[(&str, &str)]| -> Result<(u16, String), String> {
            Ok((200, "{}".to_string()))
        };
        let error = verify_registry(&test_registry(), Some("nope"), &fetch)
            .unwrap_err()
            .to_string();
        assert!(error.contains("nope"), "{error}");
        assert!(error.contains("alpha"), "{error}");
    }

    /// DeepInfra's list says what each model is: discovery adds the chat
    /// models the registry lacks, with the published context, cache, vision
    /// and prices, tiers them by output price, and leaves the rest alone.
    #[test]
    fn discovery_adds_tagged_chat_models_with_their_published_prices() {
        let registry = Registry::parse(
            r#"{"providers":[{"id":"di","wire":"openai","endpoint":"https://di.example/v1/openai/chat/completions","key_env":[],
                "models":[{"id":"di-known","backend":"vendor/Known-1","tools":true,"tier":"worker"}]}]}"#,
        )
        .unwrap();
        let urls = RefCell::new(Vec::new());
        let fetch = |url: &str, _: &[(&str, &str)]| {
            urls.borrow_mut().push(url.to_string());
            Ok((200, r#"{"data":[
                {"id":"vendor/Known-1","metadata":{"tags":["chat"],"pricing":{"input_tokens":0.1,"output_tokens":0.2}}},
                {"id":"vendor/Flash-2.1","metadata":{"context_length":1048576,"tags":["chat","vision","prompt_cache"],"pricing":{"input_tokens":0.2,"output_tokens":0.6,"cache_read_tokens":0.006}}},
                {"id":"vendor/Pro","metadata":{"context_length":262144,"tags":["chat"],"pricing":{"input_tokens":1.3,"output_tokens":2.6}}},
                {"id":"vendor/Whisper","metadata":{"tags":["audio"],"pricing":{"input_tokens":0.1,"output_tokens":0.1}}},
                {"id":"vendor/Unpriced","metadata":{"tags":["chat"]}},
                {"id":"","metadata":{"tags":["chat"]}}
            ]}"#.to_string()))
        };
        let discovery = discover(&registry, None, &fetch).unwrap();
        assert_eq!(
            urls.borrow().as_slice(),
            ["https://di.example/v1/openai/models"]
        );
        assert_eq!(discovery.providers.len(), 1);
        let di = &discovery.providers[0];
        assert_eq!((di.id.as_str(), di.wire.as_str()), ("di", "openai"));
        assert_eq!(di.endpoint, "https://di.example/v1/openai/chat/completions");
        let ids = di
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            ["di-vendor-flash-2-1", "di-vendor-pro", "di-vendor-unpriced"]
        );
        let flash = &di.models[0];
        assert_eq!(flash.backend, "vendor/Flash-2.1");
        assert_eq!(flash.context, Some(1_048_576));
        assert!(flash.tools && flash.streaming && flash.vision && flash.cache);
        assert_eq!(flash.tier, "worker");
        let pro = &di.models[1];
        assert_eq!(
            pro.tier, "rescue",
            "a dollar or more per million output tokens"
        );
        assert!(!pro.vision && !pro.cache);
        assert_eq!(di.models[2].tier, "worker");
        let flash_price = discovery.prices.get("di-vendor-flash-2-1").unwrap();
        assert_eq!(flash_price.input_miss.micros(), 200_000);
        assert_eq!(flash_price.input_hit.micros(), 6_000);
        assert_eq!(flash_price.output.micros(), 600_000);
        let pro_price = discovery.prices.get("di-vendor-pro").unwrap();
        assert_eq!(
            pro_price.input_hit, pro_price.input_miss,
            "no cache rate: the input rate"
        );
        assert!(!discovery.prices.contains_key("di-vendor-unpriced"));
        assert_eq!(
            discovery.notes,
            ["di: 1 configured, 3 more chat model(s) listed"]
        );
        // The document is a valid overlay for the registry.
        let overlay = serde_json::to_string(&discovery).unwrap();
        let merged = registry.merged(&overlay).unwrap();
        assert_eq!(
            merged.resolve("di-vendor-pro").unwrap().model_id,
            "vendor/Pro"
        );
    }

    /// SiliconFlow's list is ids alone, so its registry record names the
    /// query that narrows it to chat models; a provider with neither a query
    /// nor tags has its count reported and nothing added.
    #[test]
    fn discovery_narrows_with_the_models_query_or_adds_nothing_without_one() {
        let registry = Registry::parse(
            r#"{"aliases":{"sf-taken-1":"sf-one"},"providers":[
                {"id":"sf","wire":"openai","endpoint":"https://sf.example/v1/chat/completions","key_env":[],"models_query":"type=text&sub_type=chat",
                 "models":[{"id":"sf-one","backend":"Taken/1","tools":true,"tier":"worker"}]},
                {"id":"plain","wire":"openai","endpoint":"https://plain.example/v1/chat/completions","key_env":[],
                 "models":[{"id":"plain-one","backend":"p-1","tools":true,"tier":"worker"}]}]}"#,
        )
        .unwrap();
        let fetch = |url: &str, _: &[(&str, &str)]| {
            if url.starts_with("https://sf.example/") {
                assert_eq!(url, "https://sf.example/v1/models?type=text&sub_type=chat");
                return Ok((
                    200,
                    r#"{"data":[{"id":"Taken/1"},{"id":"vendor/New-Model"},{"id":"taken-1"}]}"#
                        .to_string(),
                ));
            }
            Ok((
                200,
                r#"{"data":[{"id":"p-1"},{"id":"p-2"},{"id":"p-3"}]}"#.to_string(),
            ))
        };
        let discovery = discover(&registry, None, &fetch).unwrap();
        assert_eq!(discovery.providers.len(), 1);
        let sf = &discovery.providers[0];
        assert_eq!(sf.id, "sf");
        let ids = sf
            .models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["sf-vendor-new-model"]);
        assert_eq!(sf.models[0].tier, "worker");
        assert!(
            discovery.prices.is_empty(),
            "nothing published, nothing priced"
        );
        assert_eq!(discovery.notes.len(), 2);
        assert!(
            discovery.notes[0].contains("1 more chat model(s)"),
            "{}",
            discovery.notes[0]
        );
        assert!(
            discovery.notes[0].contains("skipped, their registry id is taken: taken-1"),
            "{}",
            discovery.notes[0]
        );
        assert!(
            discovery.notes[1].contains("plain: lists 3 model(s)"),
            "{}",
            discovery.notes[1]
        );
        assert!(
            discovery.notes[1].contains("nothing added"),
            "{}",
            discovery.notes[1]
        );
    }

    /// An unkeyed provider is skipped unless it is the one named; an unknown
    /// name is refused; a failed fetch is a note, not an error.
    #[test]
    fn discovery_skips_unkeyed_providers_and_reports_failures_as_notes() {
        let registry = test_registry();
        let calls = Cell::new(0);
        let fetch = |_: &str, _: &[(&str, &str)]| {
            calls.set(calls.get() + 1);
            Err("offline".to_string())
        };
        let discovery = discover(&registry, None, &fetch).unwrap();
        assert!(discovery.providers.is_empty());
        assert!(
            discovery
                .notes
                .iter()
                .any(|note| note == "keyed: skipped, no key"),
            "{:?}",
            discovery.notes
        );
        assert!(
            discovery
                .notes
                .iter()
                .any(|note| note.starts_with("alpha: ") && note.contains("offline")),
            "{:?}",
            discovery.notes
        );
        assert!(
            discovery
                .notes
                .iter()
                .any(|note| note.starts_with("closed: ") && note.contains("offline")),
            "{:?}",
            discovery.notes
        );
        assert_eq!(
            calls.get(),
            3,
            "alpha, beta and closed were asked; keyed was not"
        );
        let named = discover(&registry, Some("keyed"), &fetch).unwrap();
        assert!(named.notes[0].contains("no key in"), "{:?}", named.notes);
        assert!(discover(&registry, Some("nobody"), &fetch).is_err());
    }

    #[test]
    fn a_backend_id_becomes_a_registry_id_suffix() {
        assert_eq!(
            slug("deepseek-ai/DeepSeek-V4.1-Flash"),
            "deepseek-ai-deepseek-v4-1-flash"
        );
        assert_eq!(slug("Qwen/Qwen3.8-Flash"), "qwen-qwen3-8-flash");
        assert_eq!(slug("--odd__name//"), "odd-name");
    }
}
