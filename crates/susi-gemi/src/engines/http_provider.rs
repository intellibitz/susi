use std::any::Any;

use crate::susi_core::inference_wire as wire;
use crate::susi_core::provider::{BoxFuture, Provider};

pub use crate::susi_core::inference_wire::InferenceProtocol;

pub struct HttpProvider {
    pub name: String,
    pub api_base: String,
    pub model: String,
    pub protocol: InferenceProtocol,
    /// Resolved API key (never logged). Empty = unauthenticated local.
    pub api_key: String,
}

impl HttpProvider {
    pub fn openai_local(
        name: impl Into<String>,
        api_base: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            api_base: api_base.into(),
            model: model.into(),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: String::new(),
        }
    }

    pub fn resolve_api_key(api_key_env: &str, endpoint_name: &str) -> String {
        susi_gemi_models::cloud::resolve_api_key(api_key_env, endpoint_name)
    }

    pub fn resolve_api_base(api_base_default: &str, endpoint_name: &str) -> String {
        susi_gemi_models::cloud::resolve_api_base(api_base_default, endpoint_name)
    }

    /// True for HTTPS remote bases (not localhost). Used to treat any
    /// OpenAI-compatible cloud vendor as a cloud provider for routing.
    pub fn is_remote_cloud(api_base: &str) -> bool {
        susi_gemi_models::cloud::is_remote_cloud(api_base)
    }
}

fn header_refs(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect()
}

fn openai_headers(api_key: &str, api_base: &str) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    if !api_key.is_empty() {
        headers.push(("Authorization".into(), format!("Bearer {api_key}")));
    }
    if susi_gemi_models::openrouter::is_openrouter_base(api_base) {
        let (referer, title) = susi_gemi_models::openrouter::attribution_headers();
        headers.push(("HTTP-Referer".into(), referer));
        headers.push(("X-Title".into(), title));
    }
    headers
}

fn http_ok(status: u16) -> bool {
    (200..300).contains(&status)
}

/// Wire errors arrive as `HTTP {status}: {body}` — recover the status for
/// eligibility classification.
fn parse_http_status(err: &str) -> Option<u16> {
    err.strip_prefix("HTTP ")?
        .split(':')
        .next()?
        .trim()
        .parse()
        .ok()
}

/// Providers that embed a retry hint in the error body (e.g. Gemini
/// `retryDelay`, "try again in N s"); headers do not reach the error string.
fn parse_retry_after(err: &str) -> Option<u64> {
    let lower = err.to_ascii_lowercase();
    let idx = lower.find("retry")?;
    let digits: String = lower[idx..]
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

fn healthy_blocking(
    api_base: &str,
    api_key: &str,
    protocol: InferenceProtocol,
    model: &str,
) -> bool {
    match protocol {
        InferenceProtocol::Anthropic => !api_key.is_empty(),
        InferenceProtocol::Gemini => {
            if api_key.is_empty() {
                return false;
            }
            let url = format!(
                "{}/models/{}",
                api_base.trim_end_matches('/'),
                wire::gemini_model_path(model)
            );
            susi_http_transport::http_call("GET", &url, &[("x-goog-api-key", api_key)], 5, 0)
                .ok()
                .is_some_and(|c| http_ok(c.status))
        }
        InferenceProtocol::Triton => susi_http_transport::http_call("GET", api_base, &[], 5, 0)
            .ok()
            .is_some_and(|c| http_ok(c.status) || c.status == 405),
        InferenceProtocol::OpenAiChat | InferenceProtocol::OpenAiCompletions => {
            let url = format!("{}/models", api_base.trim_end_matches('/'));
            let owned = openai_headers(api_key, api_base);
            let refs = header_refs(&owned);
            susi_http_transport::http_call("GET", &url, &refs, 5, 0)
                .ok()
                .is_some_and(|c| http_ok(c.status))
        }
    }
}

fn generate_blocking(
    api_base: &str,
    model: &str,
    api_key: &str,
    protocol: InferenceProtocol,
    prompt: &str,
) -> Result<String, String> {
    match protocol {
        InferenceProtocol::OpenAiChat => {
            let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
            let body = wire::openai_chat_body(model, prompt, 2048);
            let owned = openai_headers(api_key, api_base);
            let refs = header_refs(&owned);
            wire::openai_chat_text(&wire::post_json_timeout(&url, &refs, &body, 120)?)
        }
        InferenceProtocol::OpenAiCompletions => {
            let url = format!("{}/completions", api_base.trim_end_matches('/'));
            let body = wire::openai_completions_body(model, prompt, 2048);
            let owned = openai_headers(api_key, api_base);
            let refs = header_refs(&owned);
            wire::openai_completions_text(&wire::post_json_timeout(&url, &refs, &body, 120)?)
        }
        InferenceProtocol::Anthropic => {
            if api_key.is_empty() {
                return Err("ANTHROPIC_API_KEY (or api_key_env) not set".into());
            }
            let url = format!("{}/messages", api_base);
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 2048,
                "messages": [{"role": "user", "content": prompt}]
            });
            let json = wire::post_json_timeout(
                &url,
                &[
                    ("x-api-key", api_key),
                    ("anthropic-version", "2023-06-01"),
                    ("content-type", "application/json"),
                ],
                &body,
                120,
            )?;
            wire::anthropic_text(&json)
        }
        InferenceProtocol::Gemini => {
            if api_key.is_empty() {
                return Err("GEMINI_API_KEY / GOOGLE_API_KEY (or api_key_env) not set".into());
            }
            let url = format!(
                "{}/models/{}:generateContent",
                api_base,
                wire::gemini_model_path(model)
            );
            let body = serde_json::json!({
                "contents": [{
                    "parts": [{"text": prompt}]
                }]
            });
            let json = wire::post_json_timeout(&url, &[("x-goog-api-key", api_key)], &body, 120)?;
            wire::gemini_text(&json)
        }
        InferenceProtocol::Triton => {
            let body = wire::triton_body(prompt, 512);
            wire::triton_text(&wire::post_json_timeout(api_base, &[], &body, 120)?)
        }
    }
}

impl Provider for HttpProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_healthy(&self) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<bool>> {
        let api_base = self.api_base.clone();
        let api_key = self.api_key.clone();
        let protocol = self.protocol;
        let model = self.model.clone();
        Box::pin(async move {
            // A health probe that can't reach the endpoint is `false`, not a
            // logged error — absent local engines (Ollama, etc.) are expected.
            Ok(tokio::task::spawn_blocking(move || {
                healthy_blocking(&api_base, &api_key, protocol, &model)
            })
            .await
            .unwrap_or(false))
        })
    }

    fn generate(
        &self,
        prompt: &str,
    ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<String>> {
        let prompt = prompt.to_string();
        let api_base = self.api_base.trim_end_matches('/').to_string();
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let protocol = self.protocol;
        let name = self.name.clone();

        Box::pin(async move {
            refuse_off_host_under_local_only(&api_base)?;
            let provider = name.clone();
            let outcome_base = api_base.clone();
            let outcome_model = model.clone();
            let outcome_key = api_key.clone();
            let result = tokio::task::spawn_blocking(move || {
                generate_blocking(&api_base, &model, &api_key, protocol, &prompt)
            })
            .await
            .map_err(|e| {
                crate::susi_core::susi_error::EaiError::process(format!("Provider '{name}': {e}"))
            })?;
            // Feed the eligibility ledger with the real inference outcome so
            // eligibility reflects proven success/failure, not guesswork.
            let tracked = match &result {
                Ok(_) => susi_gemi_models::cloud_eligibility::InferenceResult::Success,
                Err(e) => susi_gemi_models::cloud_eligibility::InferenceResult::Failed {
                    status: parse_http_status(e),
                    body_snippet: e.clone(),
                    retry_after_secs: parse_retry_after(e),
                },
            };
            if susi_gemi_models::cloud::is_remote_cloud(&outcome_base) {
                susi_gemi_models::cloud_eligibility::record_inference_outcome(
                    susi_gemi_models::cloud_eligibility::Subject {
                        provider: &provider,
                        api_key: &outcome_key,
                        account: None,
                        region: None,
                        model: &outcome_model,
                    },
                    &outcome_base,
                    &tracked,
                );
            }
            result.map_err(|e| {
                crate::susi_core::susi_error::EaiError::process(format!(
                    "Provider '{provider}': {e}"
                ))
            })
        })
    }

    fn embed(
        &self,
        text: &str,
    ) -> BoxFuture<'_, crate::susi_core::susi_error::EaiResult<Vec<f32>>> {
        let text = text.to_string();
        let api_base = self.api_base.trim_end_matches('/').to_string();
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let protocol = self.protocol;

        Box::pin(async move {
            refuse_off_host_under_local_only(&api_base)?;
            if protocol != InferenceProtocol::OpenAiChat
                && protocol != InferenceProtocol::OpenAiCompletions
            {
                return Err(crate::susi_core::susi_error::EaiError::inference(
                    "Embeddings only supported on OpenAI-compatible providers",
                ));
            }
            tokio::task::spawn_blocking(move || {
                let url = format!("{}/embeddings", api_base);
                let body = serde_json::json!({
                    "model": model,
                    "input": text,
                    "encoding_format": "float"
                });
                let owned = openai_headers(&api_key, &api_base);
                let refs = header_refs(&owned);
                let json = wire::post_json_timeout(&url, &refs, &body, 60)
                    .map_err(crate::susi_core::susi_error::EaiError::network)?;
                let embedding: Vec<f32> = json["data"][0]["embedding"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|v| v.as_f64().map(|f| f as f32))
                            .collect()
                    })
                    .unwrap_or_default();
                if embedding.is_empty() {
                    return Err(crate::susi_core::susi_error::EaiError::process(
                        "embeddings response carried no data[0].embedding vector",
                    ));
                }
                Ok(embedding)
            })
            .await
            .map_err(|e| crate::susi_core::susi_error::EaiError::process(e.to_string()))?
        })
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

// Cloud env / endpoint metadata lives in susi-gemi-models (models must not
// depend on engines). Re-export for existing `susi_gemi::engines::http_provider::…` callers.
pub use susi_gemi_models::cloud::{
    cloud_env_path, effective_inference_endpoints, known_cloud_vendors, list_api_key_status,
    parse_env_file, remove_api_key, resolve_vendor_env_name,
};

/// Upsert `KEY=value` in `~/.susi/cloud.env`, apply into the process, then
/// register matching cloud providers (engines-only side effect).
pub fn register_api_key(vendor: &str, api_key: &str) -> Result<String, String> {
    let (env_name, path) = susi_gemi_models::cloud::register_api_key(vendor, api_key)?;
    register_configured_cloud_endpoints(crate::susi_core::registry::CapabilityRegistry::global());
    // Zero-config sticky pick: first registered vendor becomes preferred unless
    // the user already chose one (so a single `keys set` / env key is enough).
    let pref = crate::engines::routing::InferenceRouter::load_preference();
    if pref.preferred_cloud.is_none() {
        let _ = crate::engines::routing::InferenceRouter::set_preferred_cloud(vendor);
    }
    let registered: Vec<String> = crate::susi_core::registry::CapabilityRegistry::global()
        .list_providers()
        .into_iter()
        .filter(|n| {
            let vendor_l = vendor.to_ascii_lowercase();
            let n_l = n.to_ascii_lowercase();
            n_l.contains(&vendor_l)
                || n_l.contains(&env_name.to_ascii_lowercase().replace("_api_key", ""))
        })
        .collect();
    let provider_note = if registered.is_empty() {
        "Key saved. Provider will activate on next inference if a matching endpoint preset exists."
            .to_string()
    } else {
        format!("Registered provider(s): {}", registered.join(", "))
    };
    let prefer_note =
        match crate::engines::routing::InferenceRouter::load_preference().preferred_cloud {
            Some(p) => format!("\nPreferred cloud: {p}"),
            None => String::new(),
        };
    Ok(format!(
        "Saved {} to {} (mode 600).\n{}{}",
        env_name,
        path.display(),
        provider_note,
        prefer_note
    ))
}

/// The provider itself refuses to send a prompt or embedding text off the
/// host/LAN while the privacy posture is `local_only` without a cloud grant.
/// Callers filter providers by name; this holds even for a provider
/// registered before the switch, or named outside the vendor heuristics.
fn refuse_off_host_under_local_only(api_base: &str) -> crate::susi_core::susi_error::EaiResult<()> {
    if !crate::susi_core::mac_policy::url_stays_local(api_base)
        && crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference()
    {
        return Err(crate::susi_core::susi_error::EaiError::governance(format!(
            "[PRIVACY] local_only: refusing to send inference payload to {api_base}"
        )));
    }
    Ok(())
}

/// Register every configured `inference_endpoints` entry that can be admitted:
/// cloud vendors when their API key is present, and any local/non-cloud
/// OpenAI-compat (or Anthropic/Gemini/Triton) base with an explicit model id.
/// Bundled OpenAI-compatible presets register themselves when the matching key
/// is present — no config.json edits required.
///
/// Zero-config contract: user only sets vendor API keys (shell env,
/// `~/.susi/cloud.env`, or `susi keys set <vendor>`).
pub fn register_configured_cloud_endpoints(
    registry: &crate::susi_core::registry::CapabilityRegistry,
) {
    if crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference() {
        if std::env::var("SUSI_VERBOSE").is_ok() {
            eprintln!("[PRIVACY] local_only mode — skipping cloud inference endpoint registration");
        }
        return;
    }
    for endpoint in effective_inference_endpoints() {
        let api_base = endpoint.api_base.trim().to_string();
        if api_base.is_empty() {
            continue;
        }
        let protocol = InferenceProtocol::from_config(&endpoint.protocol_type);
        let api_key = HttpProvider::resolve_api_key(&endpoint.api_key_env, &endpoint.name);
        let is_cloud = HttpProvider::is_remote_cloud(&api_base);

        // Cloud vendors require a key; skip silently when unset so offline installs stay clean.
        if (is_cloud && api_key.is_empty())
            || key_required_but_missing(&endpoint.api_key_env, &api_key)
        {
            if std::env::var("SUSI_VERBOSE").is_ok() {
                eprintln!(
                    "[AUTODISCOVER] Skipping cloud endpoint '{}' — set {} to enable",
                    endpoint.name,
                    if endpoint.api_key_env.is_empty() {
                        "the vendor API key env"
                    } else {
                        endpoint.api_key_env.as_str()
                    }
                );
            }
            continue;
        }

        if is_custom_vendor_endpoint(&endpoint) {
            // A proxy's aliases are not the vendor's bundled model IDs. Admit
            // one advertised model, not an entire roster of failover attempts.
            if let Some(model) = custom_endpoint_model(&endpoint, &api_key) {
                let name = format!(
                    "{}-{}",
                    endpoint.name.to_ascii_lowercase().replace(' ', "-"),
                    model
                );
                if registry.get_provider(&name).is_none() {
                    registry.register_provider(HttpProvider {
                        name,
                        api_base,
                        model,
                        protocol,
                        api_key,
                    });
                }
            }
            continue;
        }

        // Local OpenAI-compat engines without an explicit model are discovered
        // live via `/models` in auto_discover_local_engines.
        if !is_cloud && endpoint.model.is_empty() {
            continue;
        }

        let model = if endpoint.name.eq_ignore_ascii_case("OpenRouter") {
            // Host pin from `susi openrouter prefer <model>` wins over bundled default.
            susi_gemi_models::openrouter::OpenRouterManager::new()
                .ok()
                .map(|m| m.effective_model())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| {
                    if endpoint.model.is_empty() {
                        susi_gemi_models::openrouter::DEFAULT_MODEL.to_string()
                    } else {
                        endpoint.model.clone()
                    }
                })
        } else if endpoint.model.is_empty() {
            match protocol {
                InferenceProtocol::Anthropic => "claude-haiku-4-5-20251001".to_string(),
                InferenceProtocol::Gemini => "gemini-3.6-flash".to_string(),
                InferenceProtocol::OpenAiChat
                | InferenceProtocol::OpenAiCompletions
                | InferenceProtocol::Triton => "gpt-4o-mini".to_string(),
            }
        } else {
            endpoint.model.clone()
        };

        let name = format!(
            "{}-{}",
            endpoint.name.to_ascii_lowercase().replace(' ', "-"),
            model
        );
        if registry.get_provider(&name).is_some() {
            continue;
        }

        registry.register_provider(HttpProvider {
            name: name.clone(),
            api_base,
            model: model.clone(),
            protocol,
            api_key,
        });
        if std::env::var("SUSI_VERBOSE").is_ok() {
            eprintln!(
                "[AUTODISCOVER] Registered configured endpoint provider: {}",
                name
            );
        }
    }

    // Prefer OpenRouter when its key is present and no cloud preference is set.
    let openrouter_key =
        crate::susi_config::env_or_cloud_env("OPENROUTER_API_KEY").unwrap_or_default();
    if !openrouter_key.is_empty()
        && crate::engines::routing::InferenceRouter::load_preference()
            .preferred_cloud
            .is_none()
    {
        let _ = crate::engines::routing::InferenceRouter::set_preferred_cloud("openrouter");
    }

    register_model_catalog(registry);

    // VC-202-022 — a declared agent seat is a brain candidate like any
    // model API: it registers here so ranking, election, the matrix, the
    // ladder and the budget all see it uniformly.
    crate::seat_provider::register_configured_seats(registry);

    // VC-202-001 — scout every configured credential with its cheapest
    // call: classify live/expired/unauthorized/rate-limited/unknown and
    // record the fingerprinted evidence so failover steers around dead
    // keys. Interval-gated so repeat registrations stay free.
    if let Some(report) =
        crate::credential_scout::scout_configured_once(&crate::credential_scout::TransportCall)
    {
        for verdict in &report.verdicts {
            tracing::info!(
                vendor = %verdict.vendor,
                liveness = ?verdict.liveness,
                status = ?verdict.status,
                detail = %verdict.detail,
                "credential scout"
            );
        }
    }
}

/// Register the curated models catalog (~50) plus the top coding/agent models.
/// Each entry mounts when its engine api_base is known and any required API key
/// is present. Local catalog entries describe installable models, not running
/// providers: only live `/models` discovery or an explicit configured model
/// can admit those endpoints.
pub fn register_model_catalog(registry: &crate::susi_core::registry::CapabilityRegistry) {
    let endpoints = effective_inference_endpoints();
    let by_name: std::collections::BTreeMap<String, _> = endpoints
        .into_iter()
        .map(|e| (e.name.to_ascii_lowercase(), e))
        .collect();

    let mut entries: Vec<crate::susi_sandbox::manager::ModelCatalogEntry> = Vec::new();
    if let Ok(coding) = susi_gemi_models::coding_models::CodingModelManager::catalog() {
        for m in coding {
            entries.push(crate::susi_sandbox::manager::ModelCatalogEntry {
                id: m.model,
                engine: m.engine,
                protocol_type: m.protocol_type,
                api_key_env: m.api_key_env,
            });
        }
    }
    if let Ok(open_weight) = susi_gemi_models::open_weight::OpenWeightManager::catalog() {
        for m in open_weight {
            entries.push(crate::susi_sandbox::manager::ModelCatalogEntry {
                id: m.ollama_tag.clone(),
                engine: m.engine.clone(),
                protocol_type: m.protocol_type.clone(),
                api_key_env: String::new(),
            });
            for v in m.variants {
                if v.ollama_tag != m.ollama_tag {
                    entries.push(crate::susi_sandbox::manager::ModelCatalogEntry {
                        id: v.ollama_tag,
                        engine: m.engine.clone(),
                        protocol_type: m.protocol_type.clone(),
                        api_key_env: String::new(),
                    });
                }
            }
        }
    }
    if let Ok(frontier) = susi_gemi_models::frontier::FrontierManager::catalog() {
        for m in frontier {
            entries.push(crate::susi_sandbox::manager::ModelCatalogEntry {
                id: m.model.clone(),
                engine: m.engine.clone(),
                protocol_type: m.protocol_type.clone(),
                api_key_env: m.api_key_env.clone(),
            });
            for v in m.variants {
                entries.push(crate::susi_sandbox::manager::ModelCatalogEntry {
                    id: v.model,
                    engine: if v.engine.is_empty() {
                        m.engine.clone()
                    } else {
                        v.engine
                    },
                    protocol_type: if v.protocol_type.is_empty() {
                        m.protocol_type.clone()
                    } else {
                        v.protocol_type
                    },
                    api_key_env: v.api_key_env,
                });
            }
        }
    }
    entries.extend(
        crate::susi_sandbox::manager::SusiConfig::load_global()
            .unwrap_or_default()
            .model_catalog(),
    );

    for entry in entries {
        if entry.id.trim().is_empty()
            || entry.engine.trim().is_empty()
            || is_non_chat_model_id(&entry.id)
        {
            continue;
        }
        let Some(endpoint) = by_name.get(&entry.engine.to_ascii_lowercase()) else {
            continue;
        };
        let api_base = endpoint.api_base.trim();
        if api_base.is_empty()
            || !HttpProvider::is_remote_cloud(api_base)
            || is_custom_vendor_endpoint(endpoint)
        {
            continue;
        }
        let key_env = if entry.api_key_env.is_empty() {
            endpoint.api_key_env.as_str()
        } else {
            entry.api_key_env.as_str()
        };
        let api_key = HttpProvider::resolve_api_key(key_env, &entry.engine);
        if api_key.is_empty() {
            continue;
        }
        let protocol = InferenceProtocol::from_config(if entry.protocol_type.is_empty() {
            &endpoint.protocol_type
        } else {
            &entry.protocol_type
        });
        let name = format!(
            "catalog-{}-{}",
            entry.engine.to_ascii_lowercase().replace(' ', "-"),
            entry.id.replace('/', "-")
        );
        if registry.get_provider(&name).is_some() {
            continue;
        }
        registry.register_provider(HttpProvider {
            name,
            api_base: api_base.to_string(),
            model: entry.id.clone(),
            protocol,
            api_key,
        });
    }
}

/// True when an endpoint declares an API-key env var but no key resolved:
/// discovery must not contact it. Endpoints without `api_key_env` (local or
/// self-hosted engines) keep open admission.
fn key_required_but_missing(api_key_env: &str, resolved_key: &str) -> bool {
    !api_key_env.trim().is_empty() && resolved_key.trim().is_empty()
}

type Endpoint = crate::susi_sandbox::manager::InferenceEndpointItem;

fn bundled_endpoint(name: &str) -> Option<Endpoint> {
    static DEFAULTS: std::sync::OnceLock<Vec<Endpoint>> = std::sync::OnceLock::new();
    DEFAULTS
        .get_or_init(|| {
            crate::susi_sandbox::manager::SusiConfig::default()
                .inference_endpoints()
                .endpoints
        })
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
        .cloned()
}

fn is_custom_vendor_endpoint(endpoint: &Endpoint) -> bool {
    !endpoint.api_key_env.is_empty()
        && bundled_endpoint(&endpoint.name).is_none_or(|default| {
            endpoint.api_base.trim_end_matches('/') != default.api_base.trim_end_matches('/')
        })
}

fn fetch_endpoint_models(
    base: &str,
    key: &str,
    protocol: InferenceProtocol,
) -> Option<Vec<String>> {
    let (url, headers) = crate::credential_scout::probe_request(protocol, base, key);
    let call = susi_http_transport::http_call("GET", &url, &header_refs(&headers), 1, 0).ok()?;
    if !http_ok(call.status) {
        return None;
    }
    let bytes = call.into_bytes(2 * 1024 * 1024).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let rows = json
        .get(if protocol == InferenceProtocol::Gemini {
            "models"
        } else {
            "data"
        })?
        .as_array()?;
    Some(
        rows.iter()
            .filter_map(|row| {
                let id = if protocol == InferenceProtocol::Gemini {
                    if !row
                        .get("supportedGenerationMethods")?
                        .as_array()?
                        .iter()
                        .any(|m| m == "generateContent")
                    {
                        return None;
                    }
                    row.get("name")?.as_str()?.trim_start_matches("models/")
                } else {
                    row.get("id")?.as_str()?
                };
                (!id.trim().is_empty() && !is_non_chat_model_id(id)).then(|| id.to_string())
            })
            .collect(),
    )
}

fn cached_endpoint_models(
    base: &str,
    key: &str,
    protocol: InferenceProtocol,
) -> Option<Vec<String>> {
    use std::{
        collections::BTreeMap,
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    type Cache = BTreeMap<String, (Instant, Option<Vec<String>>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache_key = format!(
        "{base}|{protocol:?}|{}",
        susi_gemi_models::cloud_eligibility::credential_fingerprint(key)
    );
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    cache.retain(|_, (at, models)| {
        at.elapsed() < Duration::from_secs(if models.is_some() { 60 } else { 5 })
    });
    if let Some((_, models)) = cache.get(&cache_key) {
        return models.clone();
    }
    let models = fetch_endpoint_models(base, key, protocol);
    cache.insert(cache_key, (Instant::now(), models.clone()));
    models
}

fn custom_endpoint_model(endpoint: &Endpoint, key: &str) -> Option<String> {
    let protocol = InferenceProtocol::from_config(&endpoint.protocol_type);
    if let Some(models) = cached_endpoint_models(&endpoint.api_base, key, protocol) {
        let family = match protocol {
            InferenceProtocol::Anthropic => "claude",
            InferenceProtocol::Gemini => "gemini",
            _ => "",
        };
        return models
            .iter()
            .find(|id| *id == &endpoint.model)
            .or_else(|| {
                models
                    .iter()
                    .find(|id| !family.is_empty() && id.starts_with(family))
            })
            .or_else(|| models.first())
            .cloned();
    }
    // A saved nondefault model is intentional, unlike an automatically seeded
    // bundled guess. Preserve it when this service cannot list its models.
    let explicit = !endpoint.model.is_empty()
        && bundled_endpoint(&endpoint.name).is_none_or(|default| endpoint.model != default.model);
    if explicit {
        return Some(endpoint.model.clone());
    }
    tracing::debug!(endpoint = %endpoint.name, "Custom endpoint model listing unavailable; bundled model remains unverified");
    None
}

/// Probe an OpenAI-compatible `/models` listing and register each model id.
/// Returns how many new providers were registered.
pub fn register_openai_compat_models(
    registry: &crate::susi_core::registry::CapabilityRegistry,
    engine_label: &str,
    api_base: &str,
    api_key: &str,
) -> usize {
    let base = api_base.trim_end_matches('/');
    let Some(models) = fetch_endpoint_models(base, api_key, InferenceProtocol::OpenAiChat) else {
        return 0;
    };
    let mut registered = 0usize;
    for model_id in &models {
        let name = format!(
            "{}-{}",
            engine_label.to_ascii_lowercase().replace(' ', "-"),
            model_id
        );
        if registry.get_provider(&name).is_some() {
            continue;
        }
        registry.register_provider(HttpProvider {
            name: name.clone(),
            api_base: base.to_string(),
            model: model_id.to_string(),
            protocol: InferenceProtocol::OpenAiChat,
            api_key: api_key.to_string(),
        });
        registered += 1;
        if std::env::var("SUSI_VERBOSE").is_ok() {
            eprintln!(
                "[AUTODISCOVER] Found & Registered Model: {} via {}",
                model_id, engine_label
            );
        }
    }
    registered
}

/// Vendor `/models` listings include endpoints that can never serve chat
/// completions — audio transcription, moderation/guard classifiers,
/// embeddings, image generation. Registering them only wastes failover
/// attempts (and would let an error or a class label pose as an answer),
/// so they are filtered at discovery. The failover cascade applies the
/// same predicate — registered non-`HttpProvider`s like `susi-fastembed`
/// carry embedding-only names too.
pub(crate) fn is_non_chat_model_id(model_id: &str) -> bool {
    let l = model_id.to_ascii_lowercase();
    const NON_CHAT: &[&str] = &[
        "whisper",
        "tts",
        "orpheus",
        "realtime",
        "transcribe",
        "ocr",
        "fim",
        "prompt-guard",
        "llama-guard",
        "shieldgemma",
        "embed",
        "moderation",
        "dall-e",
        "imagen",
        "audio",
        "transcription",
    ];
    NON_CHAT.iter().any(|p| l.contains(p))
}

/// Zero-Config Autonomous Engine Discovery
/// Probes well-known local OpenAI-compat ports **and** every configured
/// `inference_endpoints` base URL — open admission, not a fixed vendor list.
pub async fn auto_discover_local_engines(
    registry: &crate::susi_core::registry::CapabilityRegistry,
) {
    let mut endpoints: Vec<(String, String, String)> = vec![
        (
            "Ollama".into(),
            "http://localhost:11434/v1".into(),
            String::new(),
        ),
        (
            "vLLM".into(),
            "http://localhost:8000/v1".into(),
            String::new(),
        ),
        (
            "llama.cpp".into(),
            "http://localhost:8080/v1".into(),
            String::new(),
        ),
        (
            "sglang".into(),
            "http://localhost:30000/v1".into(),
            String::new(),
        ),
    ];

    // Every other serving engine the local-ecosystem table knows (Jan, GPT4All,
    // LocalAI, …) is probed at its default port too.
    for (name, base) in susi_gemi_models::local_ecosystem::default_openai_endpoints() {
        if !endpoints
            .iter()
            .any(|(_, b, _)| b.trim_end_matches('/') == base.trim_end_matches('/'))
        {
            endpoints.push((name, base, String::new()));
        }
    }

    // Open admission: any user/bundled inference_endpoints base joins discovery.
    for ep in effective_inference_endpoints() {
        if is_custom_vendor_endpoint(&ep) {
            continue;
        }
        let api_base = ep.api_base.trim().to_string();
        if api_base.is_empty() {
            continue;
        }
        let protocol = InferenceProtocol::from_config(&ep.protocol_type);
        // Only OpenAI-compat listing applies here; Anthropic/Gemini use chat paths.
        if !matches!(
            protocol,
            InferenceProtocol::OpenAiChat | InferenceProtocol::OpenAiCompletions
        ) {
            continue;
        }
        let already = endpoints
            .iter()
            .any(|(_, b, _)| b.trim_end_matches('/') == api_base.trim_end_matches('/'));
        if already {
            continue;
        }
        let key = HttpProvider::resolve_api_key(&ep.api_key_env, &ep.name);
        if key_required_but_missing(&ep.api_key_env, &key) {
            // A keyed vendor rejects an unauthenticated /models call anyway;
            // probing it only discloses this host to a third party on every
            // rediscovery pass.
            continue;
        }
        endpoints.push((ep.name.clone(), api_base, key));
    }

    for (engine_type, api_base, api_key) in &endpoints {
        let _ = register_openai_compat_models(registry, engine_type, api_base, api_key);
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::susi_core::registry::CapabilityRegistry;

    #[test]
    fn first_model_catalog_does_not_invent_local_providers_but_live_models_register() {
        let _guard = crate::engines::env_test_lock();
        let temporary_home = tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", temporary_home.path());
        env.set("SUSI_HOME", temporary_home.path());
        // The isolated registry uses TEMP as its rendezvous parent. Keep its
        // sibling scan inside this fixture, not the operator's whole temp tree.
        env.set("TEMP", temporary_home.path());
        env.set("TMP", temporary_home.path());
        env.set("TMPDIR", temporary_home.path());
        let registry = CapabilityRegistry::new();
        register_model_catalog(&registry);
        assert!(!registry
            .list_providers()
            .iter()
            .any(|name| name.starts_with("catalog-ollama-")));

        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0; 2048];
                    let _ = stream.read(&mut request).unwrap();
                    let body = r#"{"data":[{"id":"installed-model"}]}"#;
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "live model probe never connected"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        assert_eq!(
            register_openai_compat_models(&registry, "Ollama", &format!("http://{address}/v1"), ""),
            1
        );
        server.join().unwrap();
        assert!(registry.get_provider("ollama-installed-model").is_some());
    }

    #[test]
    fn first_override_models_are_cached_and_do_not_invent_bundled_ids() {
        use std::io::{Read, Write};
        let _guard = crate::engines::env_test_lock();
        let temporary_home = tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", temporary_home.path());
        env.set("SUSI_HOME", temporary_home.path());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            loop {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                        .unwrap();
                    let mut request = [0; 2048];
                    let n = stream.read(&mut request).unwrap();
                    let request = String::from_utf8_lossy(&request[..n]);
                    assert!(request.starts_with("GET /v1/models "));
                    assert!(request
                        .to_ascii_lowercase()
                        .contains("x-api-key: fixture-key"));
                    let body = r#"{"data":[{"id":"embedding-only"},{"id":"unrelated-alias"},{"id":"claude-actual"},{"id":"claude-preferred"}]}"#;
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .unwrap();
                    return;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "model discovery did not connect"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        });
        let mut endpoint = bundled_endpoint("Anthropic").unwrap();
        endpoint.api_base = format!("http://{address}/v1");
        assert!(is_custom_vendor_endpoint(&endpoint));
        assert_eq!(
            custom_endpoint_model(&endpoint, "fixture-key").as_deref(),
            Some("claude-actual")
        );
        server.join().unwrap();
        // The listener is gone: this second selection must use the same live
        // roster, not another probe, and must honor a listed preferred model.
        endpoint.model = "claude-preferred".into();
        assert_eq!(
            custom_endpoint_model(&endpoint, "fixture-key").as_deref(),
            Some("claude-preferred")
        );
        endpoint.model = "invented-bundled-id".into();
        assert_eq!(
            custom_endpoint_model(&endpoint, "fixture-key").as_deref(),
            Some("claude-actual")
        );
    }

    #[test]
    fn first_unavailable_override_preserves_only_explicit_model() {
        let _guard = crate::engines::env_test_lock();
        let temporary_home = tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", temporary_home.path());
        env.set("SUSI_HOME", temporary_home.path());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let mut endpoint = bundled_endpoint("GoogleGemini").unwrap();
        endpoint.api_base = format!("http://{address}/v1");
        assert!(custom_endpoint_model(&endpoint, "fixture-key").is_none());
        endpoint.model = "explicit-user-model".into();
        assert_eq!(
            custom_endpoint_model(&endpoint, "fixture-key").as_deref(),
            Some("explicit-user-model")
        );
    }

    #[test]
    fn keyed_endpoints_without_a_key_are_not_probed() {
        assert!(key_required_but_missing("OPENAI_API_KEY", ""));
        assert!(key_required_but_missing("GROQ_API_KEY", "  "));
        assert!(!key_required_but_missing("OPENAI_API_KEY", "sk-live"));
        // No key env declared: local/self-hosted engines stay admitted.
        assert!(!key_required_but_missing("", ""));
    }

    #[test]
    fn non_chat_model_ids_are_filtered() {
        // The observed live leak: vendor /models listings registered
        // audio transcription and moderation classifiers as chat providers.
        for id in [
            "whisper-large-v3",
            "whisper-large-v3-turbo",
            "meta-llama/llama-prompt-guard-2-86m",
            "llama-guard-3-8b",
            "text-embedding-3-large",
            "allam-2-7b-embed",
            "dall-e-3",
            "gpt-4o-mini-tts",
            "canopylabs/orpheus-arabic-saudi",
            "canopylabs/orpheus-v1-english",
            "mistral-ocr-2512",
            "mistral-ocr-latest",
            "codestral-fim-latest",
            "voxtral-mini-transcribe-realtime",
            "voxtral-mini-realtime-latest",
        ] {
            assert!(is_non_chat_model_id(id), "expected filtered: {id}");
        }
        for id in [
            "llama-3.3-70b-versatile",
            "qwen3.8-27b",
            "mistral-large-latest",
            "codestral-latest",
            "gemini-2.5-flash",
            // Plain voxtral is a text+audio chat model — only its
            // transcribe/realtime variants are speech-only.
            "voxtral-mini-latest",
            "voxtral-small-2507",
        ] {
            assert!(!is_non_chat_model_id(id), "expected kept: {id}");
        }
    }

    #[test]
    fn test_universal_engine_registration() {
        let registry = CapabilityRegistry::new();

        registry.register_provider(HttpProvider::openai_local(
            "ollama-llama3",
            "http://localhost:11434/v1",
            "llama3",
        ));
        registry.register_provider(HttpProvider::openai_local(
            "vllm-mixtral",
            "http://localhost:8000/v1",
            "mistralai/Mixtral-8x7B-Instruct-v0.1",
        ));
        registry.register_provider(HttpProvider::openai_local(
            "llama.cpp-phi3",
            "http://localhost:8080/v1",
            "phi3-mini-4k-instruct",
        ));
        registry.register_provider(HttpProvider::openai_local(
            "sglang-llama3-70b",
            "http://localhost:30000/v1",
            "meta-llama/Meta-Llama-3-70B-Instruct",
        ));

        let providers = registry.list_providers();
        assert_eq!(providers.len(), 4);
        assert!(providers.contains(&"ollama-llama3".to_string()));
        assert!(providers.contains(&"vllm-mixtral".to_string()));
        assert!(providers.contains(&"llama.cpp-phi3".to_string()));
        assert!(providers.contains(&"sglang-llama3-70b".to_string()));
    }

    #[test]
    fn protocol_from_config_maps_vendors() {
        assert_eq!(
            InferenceProtocol::from_config("anthropic"),
            InferenceProtocol::Anthropic
        );
        assert_eq!(
            InferenceProtocol::from_config("gemini"),
            InferenceProtocol::Gemini
        );
        assert_eq!(
            InferenceProtocol::from_config("chat"),
            InferenceProtocol::OpenAiChat
        );
        assert_eq!(
            InferenceProtocol::from_config("completions"),
            InferenceProtocol::OpenAiCompletions
        );
    }

    #[test]
    fn upsert_and_register_roundtrip_in_temp_cloud_env() {
        let _guard = crate::engines::env_test_lock();
        let isolated = IsolatedCloudHome::new("susi_key_reg");
        // A key in the developer's shell env would (correctly) win over
        // cloud.env; clear it for the duration of the test.
        let prev_key = std::env::var_os("DEEPSEEK_API_KEY");
        unsafe {
            std::env::remove_var("DEEPSEEK_API_KEY");
        }

        let msg = register_api_key("deepseek", "sk-test-deepseek").unwrap();
        assert!(msg.contains("DEEPSEEK_API_KEY"));
        let path = cloud_env_path();
        assert!(
            path.starts_with(&isolated.dir),
            "cloud.env must land under isolated home, got {}",
            path.display()
        );
        assert!(path.exists());
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("DEEPSEEK_API_KEY=sk-test-deepseek"));
        // Visible to lookups, never copied into the process environment.
        assert_eq!(
            crate::susi_config::env_or_cloud_env("DEEPSEEK_API_KEY").unwrap(),
            "sk-test-deepseek"
        );
        assert!(std::env::var_os("DEEPSEEK_API_KEY").is_none());

        remove_api_key("deepseek").unwrap();
        let body = std::fs::read_to_string(cloud_env_path()).unwrap_or_default();
        assert!(!body.contains("DEEPSEEK_API_KEY"));
        assert!(crate::susi_config::env_or_cloud_env("DEEPSEEK_API_KEY").is_err());
        if let Some(v) = prev_key {
            unsafe {
                std::env::set_var("DEEPSEEK_API_KEY", v);
            }
        }
        drop(isolated);
    }

    #[test]
    fn register_configured_respects_api_key_presence() {
        let _guard = crate::engines::env_test_lock();
        let _isolated = IsolatedCloudHome::new("susi_key_cfg");
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY");
            std::env::remove_var("GEMINI_API_KEY");
            std::env::remove_var("GOOGLE_API_KEY");
            std::env::remove_var("DEEPSEEK_API_KEY");
            std::env::remove_var("MOONSHOT_API_KEY");
            std::env::remove_var("KIMI_API_KEY");
            std::env::remove_var("MINIMAX_API_KEY");
            std::env::remove_var("OPENROUTER_API_KEY");
        }

        let registry = CapabilityRegistry::new();
        register_configured_cloud_endpoints(&registry);
        let cloudish: Vec<_> = registry
            .list_providers()
            .into_iter()
            .filter(|n| is_bundled_cloud_prefix(n))
            .collect();
        assert!(
            cloudish.is_empty(),
            "must not register cloud providers without keys: {:?}",
            cloudish
        );

        unsafe {
            std::env::set_var("OPENAI_API_KEY", "sk-unit-test");
            std::env::set_var("DEEPSEEK_API_KEY", "sk-deepseek-test");
        }
        register_configured_cloud_endpoints(&registry);
        let providers = registry.list_providers();
        assert!(
            providers.iter().any(|n| n.starts_with("openai-")),
            "expected openai-* provider, got {:?}",
            providers
        );
        assert!(
            providers.iter().any(|n| n.starts_with("deepseek-")),
            "expected deepseek-* OpenAI-compat provider, got {:?}",
            providers
        );
        let openai = providers.iter().find(|n| n.starts_with("openai-")).unwrap();
        let p = registry.get_provider(openai).unwrap();
        let http = p.as_any().downcast_ref::<HttpProvider>().unwrap();
        assert_eq!(http.protocol, InferenceProtocol::OpenAiChat);
        assert_eq!(http.api_key, "sk-unit-test");
        assert!(!http.model.is_empty());
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("DEEPSEEK_API_KEY");
        }
    }

    /// Redirect HOME + XDG_* so cloud.env never reads the developer machine.
    /// Also scrubs `SUSI_HOME`/`SUSI_PORT_OFFSET`, which outrank HOME/XDG.
    struct IsolatedCloudHome {
        dir: std::path::PathBuf,
        env: Option<susi_paths::test_env::EnvGuard>,
    }

    impl IsolatedCloudHome {
        fn new(prefix: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("{}_{}", prefix, std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let xdg_config = dir.join("config");
            let xdg_data = dir.join("data");
            std::fs::create_dir_all(&xdg_config).unwrap();
            std::fs::create_dir_all(&xdg_data).unwrap();
            let mut env = susi_paths::test_env::EnvGuard::isolated();
            env.set("HOME", &dir);
            env.set("XDG_CONFIG_HOME", &xdg_config);
            env.set("XDG_DATA_HOME", &xdg_data);
            Self {
                dir,
                env: Some(env),
            }
        }
    }

    impl Drop for IsolatedCloudHome {
        fn drop(&mut self) {
            self.env = None;
            std::env::remove_var("DEEPSEEK_API_KEY");
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn is_bundled_cloud_prefix(n: &str) -> bool {
        n.starts_with("openai-")
            || n.starts_with("anthropic-")
            || n.starts_with("googlegemini-")
            || n.starts_with("deepseek-")
            || n.starts_with("kimi-")
            || n.starts_with("minimax-")
            || n.starts_with("openrouter-")
    }
}
