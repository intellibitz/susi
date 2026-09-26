use std::any::Any;

use crate::susi_core::inference_wire as wire;
use crate::susi_core::provider::{BoxFuture, Provider};

/// Wire protocol for an HTTP inference backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceProtocol {
    /// OpenAI-compatible `/chat/completions` (+ optional Bearer auth).
    OpenAiChat,
    /// OpenAI-compatible `/completions`.
    OpenAiCompletions,
    /// Anthropic Messages API (`/messages`, `x-api-key`).
    Anthropic,
    /// Google Gemini `generateContent`.
    Gemini,
    /// NVIDIA Triton generate endpoint (raw URL).
    Triton,
}

impl InferenceProtocol {
    pub fn from_config(protocol_type: &str) -> Self {
        match protocol_type.to_ascii_lowercase().as_str() {
            "anthropic" => Self::Anthropic,
            "gemini" => Self::Gemini,
            "triton" => Self::Triton,
            "completions" => Self::OpenAiCompletions,
            // "chat" and unknown OpenAI-shaped defaults
            _ => Self::OpenAiChat,
        }
    }
}

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

    /// True for HTTPS remote bases (not localhost). Used to treat any
    /// OpenAI-compatible cloud vendor as a cloud provider for routing.
    pub fn is_remote_cloud(api_base: &str) -> bool {
        susi_gemi_models::cloud::is_remote_cloud(api_base)
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
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .map_err(|e| crate::susi_core::susi_error::EaiError::network(e.to_string()))?;

            match protocol {
                InferenceProtocol::Anthropic => {
                    // Anthropic has no cheap public ping; key presence is the gate.
                    Ok(!api_key.is_empty())
                }
                InferenceProtocol::Gemini => {
                    if api_key.is_empty() {
                        return Ok(false);
                    }
                    let url = format!(
                        "{}/models/{}",
                        api_base.trim_end_matches('/'),
                        wire::gemini_model_path(&model)
                    );
                    // A health probe that can't reach the endpoint is the
                    // expected answer for an absent engine — `false`, not a
                    // logged error. Constructing EaiError::network here wrote
                    // a metrics record per absent provider per rediscovery
                    // pass (perpetual noise on hosts without e.g. Ollama).
                    let Ok(res) = client
                        .get(&url)
                        .header("x-goog-api-key", &api_key)
                        .send()
                        .await
                    else {
                        return Ok(false);
                    };
                    Ok(res.status().is_success())
                }
                InferenceProtocol::Triton => {
                    let Ok(res) = client.get(&api_base).send().await else {
                        return Ok(false);
                    };
                    Ok(res.status().is_success() || res.status().as_u16() == 405)
                }
                InferenceProtocol::OpenAiChat | InferenceProtocol::OpenAiCompletions => {
                    let url = format!("{}/models", api_base.trim_end_matches('/'));
                    let mut req = client.get(&url);
                    if !api_key.is_empty() {
                        req = req.bearer_auth(&api_key);
                    }
                    req = apply_openrouter_attribution(req, &api_base);
                    let Ok(res) = req.send().await else {
                        return Ok(false);
                    };
                    Ok(res.status().is_success())
                }
            }
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
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .map_err(|e| crate::susi_core::susi_error::EaiError::network(e.to_string()))?;

            match protocol {
                InferenceProtocol::OpenAiChat => {
                    generate_openai_chat(&client, &api_base, &model, &api_key, &prompt).await
                }
                InferenceProtocol::OpenAiCompletions => {
                    generate_openai_completions(&client, &api_base, &model, &api_key, &prompt).await
                }
                InferenceProtocol::Anthropic => {
                    generate_anthropic(&client, &api_base, &model, &api_key, &prompt).await
                }
                InferenceProtocol::Gemini => {
                    generate_gemini(&client, &api_base, &model, &api_key, &prompt).await
                }
                InferenceProtocol::Triton => generate_triton(&client, &api_base, &prompt).await,
            }
            .map_err(|e| {
                crate::susi_core::susi_error::EaiError::process(format!(
                    "Provider '{}': {}",
                    name, e
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
            if protocol != InferenceProtocol::OpenAiChat
                && protocol != InferenceProtocol::OpenAiCompletions
            {
                return Err(crate::susi_core::susi_error::EaiError::inference(
                    "Embeddings only supported on OpenAI-compatible providers",
                ));
            }
            // Bounded like the chat path: a stalled endpoint must not hang
            // the embedding caller (semantic index refresh) forever.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .unwrap_or_default();
            let url = format!("{}/embeddings", api_base);
            let body = serde_json::json!({
                "model": model,
                "input": text,
                "encoding_format": "float"
            });
            let mut req = client.post(&url).json(&body);
            if !api_key.is_empty() {
                req = req.bearer_auth(&api_key);
            }
            let res = req
                .send()
                .await
                .map_err(|e| crate::susi_core::susi_error::EaiError::network(e.to_string()))?;
            if !res.status().is_success() {
                return Err(crate::susi_core::susi_error::EaiError::process(format!(
                    "HTTP Error: {}",
                    res.status()
                )));
            }
            let json: serde_json::Value = res
                .json()
                .await
                .map_err(|e| crate::susi_core::susi_error::EaiError::network(e.to_string()))?;
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
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

async fn generate_openai_chat(
    client: &reqwest::Client,
    api_base: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<String, String> {
    let url = format!("{}/chat/completions", api_base.trim_end_matches('/'));
    let body = wire::openai_chat_body(model, prompt, 2048);
    let json = post_openai(client, &url, api_key, api_base, &body).await?;
    wire::openai_chat_text(&json)
}

/// POST an OpenAI-API body, retrying once with `max_completion_tokens` when
/// the provider rejects `max_tokens` (see `inference_wire::token_param_retry`).
async fn post_openai(
    client: &reqwest::Client,
    url: &str,
    api_key: &str,
    api_base: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let send = |body: &serde_json::Value| {
        let mut req = client.post(url).json(body);
        if !api_key.is_empty() {
            req = req.bearer_auth(api_key);
        }
        apply_openrouter_attribution(req, api_base).send()
    };
    let mut res = send(body).await.map_err(|e| e.to_string())?;
    if res.status() == reqwest::StatusCode::BAD_REQUEST {
        let text = res.text().await.unwrap_or_default();
        let Some(retry) = wire::token_param_retry(body, &text) else {
            return Err(format!(
                "HTTP 400 Bad Request: {}",
                text.chars().take(200).collect::<String>()
            ));
        };
        res = send(&retry).await.map_err(|e| e.to_string())?;
    }
    if !res.status().is_success() {
        return Err(http_failure(res).await);
    }
    res.json().await.map_err(|e| e.to_string())
}

/// HTTP failure with the provider's (truncated) error body.
async fn http_failure(res: reqwest::Response) -> String {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    format!(
        "HTTP {}: {}",
        status,
        body.chars().take(200).collect::<String>()
    )
}

async fn generate_openai_completions(
    client: &reqwest::Client,
    api_base: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<String, String> {
    let url = format!("{}/completions", api_base.trim_end_matches('/'));
    let body = wire::openai_completions_body(model, prompt, 2048);
    let json = post_openai(client, &url, api_key, api_base, &body).await?;
    wire::openai_completions_text(&json)
}

fn apply_openrouter_attribution(
    mut req: reqwest::RequestBuilder,
    api_base: &str,
) -> reqwest::RequestBuilder {
    if susi_gemi_models::openrouter::is_openrouter_base(api_base) {
        let (referer, title) = susi_gemi_models::openrouter::attribution_headers();
        req = req.header("HTTP-Referer", referer).header("X-Title", title);
    }
    req
}

async fn generate_anthropic(
    client: &reqwest::Client,
    api_base: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("ANTHROPIC_API_KEY (or api_key_env) not set".into());
    }
    let url = format!("{}/messages", api_base);
    let body = serde_json::json!({
        "model": model,
        "max_tokens": 2048,
        "messages": [{"role": "user", "content": prompt}]
    });
    let res = client
        .post(&url)
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(http_failure(res).await);
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    wire::anthropic_text(&json)
}

async fn generate_gemini(
    client: &reqwest::Client,
    api_base: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<String, String> {
    if api_key.is_empty() {
        return Err("GEMINI_API_KEY / GOOGLE_API_KEY (or api_key_env) not set".into());
    }
    // Key travels in `x-goog-api-key`, never the URL: reqwest errors
    // render the request URL, which would leak a query-string key into
    // error strings and logs.
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
    let res = client
        .post(&url)
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(http_failure(res).await);
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    wire::gemini_text(&json)
}

async fn generate_triton(
    client: &reqwest::Client,
    api_base: &str,
    prompt: &str,
) -> Result<String, String> {
    let body = wire::triton_body(prompt, 512);
    let res = client
        .post(api_base)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(http_failure(res).await);
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    wire::triton_text(&json)
}

// Cloud env / endpoint metadata lives in susi-gemi-models (models must not
// depend on engines). Re-export for existing `susi_gemi::http_provider::…` callers.
pub use susi_gemi_models::cloud::{
    apply_cloud_env_file, cloud_env_path, effective_inference_endpoints,
    effective_inference_endpoints_pub, known_cloud_vendors, list_api_key_status, parse_env_file,
    remove_api_key, resolve_vendor_env_name,
};

/// Upsert `KEY=value` in `~/.susi/cloud.env`, apply into the process, then
/// register matching cloud providers (engines-only side effect).
pub fn register_api_key(vendor: &str, api_key: &str) -> Result<String, String> {
    let (env_name, path) = susi_gemi_models::cloud::register_api_key(vendor, api_key)?;
    register_configured_cloud_endpoints(crate::susi_core::registry::CapabilityRegistry::global());
    // Zero-config sticky pick: first registered vendor becomes preferred unless
    // the user already chose one (so a single `keys set` / env key is enough).
    let pref = crate::routing::InferenceRouter::load_preference();
    if pref.preferred_cloud.is_none() {
        let _ = crate::routing::InferenceRouter::set_preferred_cloud(vendor);
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
    let prefer_note = match crate::routing::InferenceRouter::load_preference().preferred_cloud {
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
    apply_cloud_env_file();
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
        if is_cloud && api_key.is_empty() {
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
                InferenceProtocol::Anthropic => "claude-3-5-haiku-20241022".to_string(),
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
    let openrouter_key = std::env::var("OPENROUTER_API_KEY").unwrap_or_default();
    if !openrouter_key.is_empty()
        && crate::routing::InferenceRouter::load_preference()
            .preferred_cloud
            .is_none()
    {
        let _ = crate::routing::InferenceRouter::set_preferred_cloud("openrouter");
    }

    register_model_catalog(registry);
}

/// Register the curated models catalog (~50) plus the top coding/agent models.
/// Each entry mounts when its engine api_base is known and any required API key
/// is present (local engines with empty api_key_env always admit). Live `/models`
/// discovery remains unbounded on top of this ladder.
pub fn register_model_catalog(registry: &crate::susi_core::registry::CapabilityRegistry) {
    apply_cloud_env_file();
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
        if api_base.is_empty() {
            continue;
        }
        let key_env = if entry.api_key_env.is_empty() {
            endpoint.api_key_env.as_str()
        } else {
            entry.api_key_env.as_str()
        };
        let api_key = HttpProvider::resolve_api_key(key_env, &entry.engine);
        if HttpProvider::is_remote_cloud(api_base) && api_key.is_empty() {
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

/// Probe an OpenAI-compatible `/models` listing and register each model id.
/// Returns how many new providers were registered.
pub async fn register_openai_compat_models(
    registry: &crate::susi_core::registry::CapabilityRegistry,
    engine_label: &str,
    api_base: &str,
    api_key: &str,
    client: &reqwest::Client,
) -> usize {
    let base = api_base.trim_end_matches('/');
    let url = format!("{base}/models");
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let Ok(res) = req.send().await else {
        return 0;
    };
    if !res.status().is_success() {
        return 0;
    }
    let Ok(json) = res.json::<serde_json::Value>().await else {
        return 0;
    };
    let Some(models) = json.get("data").and_then(|d| d.as_array()) else {
        return 0;
    };
    let mut registered = 0usize;
    for model in models {
        let Some(model_id) = model.get("id").and_then(|id| id.as_str()) else {
            continue;
        };
        if is_non_chat_model_id(model_id) {
            continue;
        }
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
        (
            "LMStudio".into(),
            "http://localhost:1234/v1".into(),
            String::new(),
        ),
    ];

    // Open admission: any user/bundled inference_endpoints base joins discovery.
    for ep in effective_inference_endpoints() {
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

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap_or_default();

    for (engine_type, api_base, api_key) in &endpoints {
        let _ =
            register_openai_compat_models(registry, engine_type, api_base, api_key, &client).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_core::registry::CapabilityRegistry;

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
        assert_eq!(
            std::env::var("DEEPSEEK_API_KEY").unwrap(),
            "sk-test-deepseek"
        );

        remove_api_key("deepseek").unwrap();
        let body = std::fs::read_to_string(cloud_env_path()).unwrap_or_default();
        assert!(!body.contains("DEEPSEEK_API_KEY"));
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
    struct IsolatedCloudHome {
        dir: std::path::PathBuf,
        prev_home: Option<std::ffi::OsString>,
        prev_xdg_config: Option<std::ffi::OsString>,
        prev_xdg_data: Option<std::ffi::OsString>,
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
            let prev_home = std::env::var_os("HOME");
            let prev_xdg_config = std::env::var_os("XDG_CONFIG_HOME");
            let prev_xdg_data = std::env::var_os("XDG_DATA_HOME");
            unsafe {
                std::env::set_var("HOME", &dir);
                std::env::set_var("XDG_CONFIG_HOME", &xdg_config);
                std::env::set_var("XDG_DATA_HOME", &xdg_data);
            }
            Self {
                dir,
                prev_home,
                prev_xdg_config,
                prev_xdg_data,
            }
        }
    }

    impl Drop for IsolatedCloudHome {
        fn drop(&mut self) {
            unsafe {
                match &self.prev_home {
                    Some(h) => std::env::set_var("HOME", h),
                    None => std::env::remove_var("HOME"),
                }
                match &self.prev_xdg_config {
                    Some(h) => std::env::set_var("XDG_CONFIG_HOME", h),
                    None => std::env::remove_var("XDG_CONFIG_HOME"),
                }
                match &self.prev_xdg_data {
                    Some(h) => std::env::set_var("XDG_DATA_HOME", h),
                    None => std::env::remove_var("XDG_DATA_HOME"),
                }
                std::env::remove_var("DEEPSEEK_API_KEY");
            }
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
