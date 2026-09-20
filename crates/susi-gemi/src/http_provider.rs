use std::any::Any;

use susi_core::provider::{BoxFuture, Provider};
use susi_error::{EaiError, EaiResult};

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
        if !api_key_env.is_empty() {
            if let Ok(v) = std::env::var(api_key_env) {
                if !v.is_empty() {
                    return v;
                }
            }
        }
        // Convention fallbacks by vendor name (OpenAI-compat + native APIs)
        let lower = endpoint_name.to_ascii_lowercase();
        let candidates: &[&str] = match lower.as_str() {
            "openai" => &["OPENAI_API_KEY"],
            "anthropic" => &["ANTHROPIC_API_KEY"],
            "googlegemini" | "gemini" | "google" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
            "deepseek" => &["DEEPSEEK_API_KEY"],
            "minimax" => &["MINIMAX_API_KEY"],
            "kimi" | "moonshot" => &["MOONSHOT_API_KEY", "KIMI_API_KEY"],
            "openrouter" => &["OPENROUTER_API_KEY"],
            "mistral" => &["MISTRAL_API_KEY"],
            "groq" => &["GROQ_API_KEY"],
            "together" => &["TOGETHER_API_KEY"],
            "fireworks" => &["FIREWORKS_API_KEY"],
            _ => &[],
        };
        for env in candidates {
            if let Ok(v) = std::env::var(env) {
                if !v.is_empty() {
                    return v;
                }
            }
        }
        // Any named endpoint: try `{NAME}_API_KEY` (e.g. DeepSeek → DEEPSEEK_API_KEY).
        let generic = format!(
            "{}_API_KEY",
            endpoint_name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
                .trim_matches('_')
                .replace("__", "_")
        );
        if !generic.is_empty() {
            if let Ok(v) = std::env::var(&generic) {
                if !v.is_empty() {
                    return v;
                }
            }
        }
        String::new()
    }

    /// True for HTTPS remote bases (not localhost). Used to treat any
    /// OpenAI-compatible cloud vendor as a cloud provider for routing.
    pub fn is_remote_cloud(api_base: &str) -> bool {
        let lower = api_base.to_ascii_lowercase();
        lower.starts_with("https://")
            && !lower.contains("localhost")
            && !lower.contains("127.0.0.1")
    }
}

impl Provider for HttpProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_healthy(&self) -> BoxFuture<'_, EaiResult<bool>> {
        let api_base = self.api_base.clone();
        let api_key = self.api_key.clone();
        let protocol = self.protocol;
        let model = self.model.clone();
        Box::pin(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .map_err(|e| EaiError::network(e.to_string()))?;

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
                        "{}/models/{}?key={}",
                        api_base.trim_end_matches('/'),
                        model,
                        api_key
                    );
                    let res = client
                        .get(&url)
                        .send()
                        .await
                        .map_err(|e| EaiError::network(e.to_string()))?;
                    Ok(res.status().is_success())
                }
                InferenceProtocol::Triton => {
                    let res = client
                        .get(&api_base)
                        .send()
                        .await
                        .map_err(|e| EaiError::network(e.to_string()))?;
                    Ok(res.status().is_success() || res.status().as_u16() == 405)
                }
                InferenceProtocol::OpenAiChat | InferenceProtocol::OpenAiCompletions => {
                    let url = format!("{}/models", api_base.trim_end_matches('/'));
                    let mut req = client.get(&url);
                    if !api_key.is_empty() {
                        req = req.bearer_auth(&api_key);
                    }
                    let res = req
                        .send()
                        .await
                        .map_err(|e| EaiError::network(e.to_string()))?;
                    Ok(res.status().is_success())
                }
            }
        })
    }

    fn generate(&self, prompt: &str) -> BoxFuture<'_, EaiResult<String>> {
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
                .map_err(|e| EaiError::network(e.to_string()))?;

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
            .map_err(|e| EaiError::process(format!("Provider '{}': {}", name, e)))
        })
    }

    fn embed(&self, text: &str) -> BoxFuture<'_, EaiResult<Vec<f32>>> {
        let text = text.to_string();
        let api_base = self.api_base.trim_end_matches('/').to_string();
        let model = self.model.clone();
        let api_key = self.api_key.clone();
        let protocol = self.protocol;

        Box::pin(async move {
            if protocol != InferenceProtocol::OpenAiChat
                && protocol != InferenceProtocol::OpenAiCompletions
            {
                return Err(EaiError::inference(
                    "Embeddings only supported on OpenAI-compatible providers",
                ));
            }
            let client = reqwest::Client::new();
            let url = format!("{}/embeddings", api_base);
            let body = serde_json::json!({
                "model": model,
                "input": text
            });
            let mut req = client.post(&url).json(&body);
            if !api_key.is_empty() {
                req = req.bearer_auth(&api_key);
            }
            let res = req
                .send()
                .await
                .map_err(|e| EaiError::network(e.to_string()))?;
            if !res.status().is_success() {
                return Err(EaiError::process(format!("HTTP Error: {}", res.status())));
            }
            let json: serde_json::Value = res
                .json()
                .await
                .map_err(|e| EaiError::network(e.to_string()))?;
            let embedding = json["data"][0]["embedding"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect();
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
    let url = format!("{}/chat/completions", api_base);
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": 2048
    });
    let mut req = client.post(&url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {}: {}",
            status,
            body.chars().take(200).collect::<String>()
        ));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    Ok(json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

async fn generate_openai_completions(
    client: &reqwest::Client,
    api_base: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
) -> Result<String, String> {
    let url = format!("{}/completions", api_base);
    let body = serde_json::json!({
        "model": model,
        "prompt": prompt,
        "max_tokens": 2048
    });
    let mut req = client.post(&url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let res = req.send().await.map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    Ok(json["choices"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string())
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
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {}: {}",
            status,
            body.chars().take(200).collect::<String>()
        ));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    // content is an array of blocks; take first text block
    if let Some(parts) = json["content"].as_array() {
        for part in parts {
            if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                    return Ok(text.to_string());
                }
            }
        }
    }
    Ok(String::new())
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
    let url = format!(
        "{}/models/{}:generateContent?key={}",
        api_base, model, api_key
    );
    let body = serde_json::json!({
        "contents": [{
            "parts": [{"text": prompt}]
        }]
    });
    let res = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(format!(
            "HTTP {}: {}",
            status,
            body.chars().take(200).collect::<String>()
        ));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    Ok(json["candidates"][0]["content"]["parts"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string())
}

async fn generate_triton(
    client: &reqwest::Client,
    api_base: &str,
    prompt: &str,
) -> Result<String, String> {
    let body = serde_json::json!({
        "text_input": prompt,
        "parameters": { "max_tokens": 512, "bad_words": [], "stop_words": [] }
    });
    let res = client
        .post(api_base)
        .json(&body)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("HTTP {}", res.status()));
    }
    let json: serde_json::Value = res.json().await.map_err(|e| e.to_string())?;
    Ok(json["text_output"]
        .as_str()
        .or_else(|| json["outputs"][0]["data"][0].as_str())
        .unwrap_or_default()
        .to_string())
}

/// Register configured cloud / remote endpoints that have API keys available —
/// the config-driven counterpart to localhost auto-discovery.
pub fn register_configured_cloud_endpoints(registry: &susi_core::registry::CapabilityRegistry) {
    for endpoint in effective_inference_endpoints() {
        let api_base = endpoint.api_base.trim().to_string();
        if api_base.is_empty() {
            continue;
        }
        // Local OpenAI-compat engines are handled by auto_discover_local_engines.
        if !HttpProvider::is_remote_cloud(&api_base) {
            continue;
        }

        let protocol = InferenceProtocol::from_config(&endpoint.protocol_type);
        let api_key = HttpProvider::resolve_api_key(&endpoint.api_key_env, &endpoint.name);
        // Cloud vendors require a key; skip silently when unset so offline installs stay clean.
        if api_key.is_empty() {
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

        let model = if endpoint.model.is_empty() {
            match protocol {
                InferenceProtocol::Anthropic => "claude-3-5-haiku-20241022".to_string(),
                InferenceProtocol::Gemini => "gemini-2.0-flash".to_string(),
                _ => "gpt-4o-mini".to_string(),
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
                "[AUTODISCOVER] Registered cloud provider: {} ({:?})",
                name, protocol
            );
        }
    }
}

/// Bundled defaults ∪ user `inference_endpoints` by name (user wins).
/// Ensures new OpenAI-compat presets (DeepSeek, Kimi, …) appear even when
/// `~/.susi/config.json` still has an older endpoints array.
fn effective_inference_endpoints() -> Vec<susi_sandbox::manager::InferenceEndpointItem> {
    use std::collections::BTreeMap;
    let bundled = susi_sandbox::manager::SusiConfig::default()
        .inference_endpoints()
        .endpoints;
    let user = susi_sandbox::manager::SusiConfig::load_global()
        .unwrap_or_default()
        .inference_endpoints()
        .endpoints;
    let mut by_name: BTreeMap<String, susi_sandbox::manager::InferenceEndpointItem> =
        BTreeMap::new();
    for endpoint in bundled {
        by_name.insert(endpoint.name.to_ascii_lowercase(), endpoint);
    }
    for endpoint in user {
        by_name.insert(endpoint.name.to_ascii_lowercase(), endpoint);
    }
    by_name.into_values().collect()
}

/// Zero-Config Autonomous Engine Discovery
/// Probes standard ports for active local inference engines and automatically
/// registers them with the capability registry.
pub async fn auto_discover_local_engines(registry: &susi_core::registry::CapabilityRegistry) {
    let endpoints = vec![
        ("Ollama", "http://localhost:11434/v1"),
        ("vLLM", "http://localhost:8000/v1"),
        ("llama.cpp", "http://localhost:8080/v1"),
        ("sglang", "http://localhost:30000/v1"),
        ("LMStudio", "http://localhost:1234/v1"),
    ];

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(500))
        .build()
        .unwrap_or_default();

    for (engine_type, api_base) in endpoints {
        let url = format!("{}/models", api_base);
        if let Ok(res) = client.get(&url).send().await {
            if res.status().is_success() {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    if let Some(models) = json.get("data").and_then(|d| d.as_array()) {
                        for model in models {
                            if let Some(model_id) = model.get("id").and_then(|id| id.as_str()) {
                                let name = format!("{}-{}", engine_type.to_lowercase(), model_id);

                                // Only register if not already registered to prevent spam
                                if registry.get_provider(&name).is_none() {
                                    registry.register_provider(HttpProvider::openai_local(
                                        name.clone(),
                                        api_base,
                                        model_id,
                                    ));
                                    if std::env::var("SUSI_VERBOSE").is_ok() {
                                        eprintln!(
                                            "[AUTODISCOVER] Found & Registered Model: {} via {}",
                                            model_id, engine_type
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use susi_core::registry::CapabilityRegistry;

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
    fn resolve_api_key_reads_env() {
        // SAFETY: test-only env mutation in a single-threaded unit test.
        unsafe {
            std::env::set_var("SUSI_TEST_CLOUD_KEY", "sk-test-123");
        }
        let key = HttpProvider::resolve_api_key("SUSI_TEST_CLOUD_KEY", "OpenAI");
        assert_eq!(key, "sk-test-123");
        unsafe {
            std::env::remove_var("SUSI_TEST_CLOUD_KEY");
        }
    }

    #[test]
    fn is_remote_cloud_detects_https_vendors() {
        assert!(HttpProvider::is_remote_cloud("https://api.openai.com/v1"));
        assert!(!HttpProvider::is_remote_cloud("http://localhost:11434/v1"));
        assert!(!HttpProvider::is_remote_cloud("https://127.0.0.1:8443/v1"));
    }

    #[test]
    fn register_configured_respects_api_key_presence() {
        use std::sync::Mutex;
        static ENV_LOCK: Mutex<()> = Mutex::new(());
        let _guard = ENV_LOCK.lock().unwrap();

        let registry = CapabilityRegistry::new();
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
