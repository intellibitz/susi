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

/// Zero-config cloud secrets: `~/.susi/cloud.env` (KEY=value lines).
/// Shell / process env always wins; this file only fills missing keys so an
/// always-on systemd daemon still sees API keys without editing config.json.
pub fn cloud_env_path() -> std::path::PathBuf {
    susi_paths::SusiDirs::config_dir().join("cloud.env")
}

/// Parse a dotenv-style file into key/value pairs (no side effects).
pub fn parse_env_file(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let mut value = value.trim().to_string();
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            value = value[1..value.len().saturating_sub(1)].to_string();
        }
        if value.is_empty() {
            continue;
        }
        out.push((key.to_string(), value));
    }
    out
}

/// Load `~/.susi/cloud.env` into the process environment for any key not
/// already set. Idempotent; safe to call from CLI and daemon boot.
pub fn apply_cloud_env_file() {
    let path = cloud_env_path();
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    for (key, value) in parse_env_file(&content) {
        match std::env::var(&key) {
            Ok(existing) if !existing.is_empty() => continue,
            _ => {
                // SAFETY: susi owns these vendor key names; we only set when unset.
                unsafe {
                    std::env::set_var(&key, &value);
                }
            }
        }
    }
}

/// Map a user-facing vendor name (or raw `FOO_API_KEY`) to the env var name.
pub fn resolve_vendor_env_name(vendor: &str) -> Option<String> {
    let trimmed = vendor.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase().replace('-', "_");
    if upper.ends_with("_API_KEY") || upper.ends_with("_KEY") {
        return Some(upper);
    }
    let env = match trimmed.to_ascii_lowercase().as_str() {
        "openai" => "OPENAI_API_KEY",
        "anthropic" | "claude" => "ANTHROPIC_API_KEY",
        "gemini" | "google" | "googlegemini" => "GEMINI_API_KEY",
        "deepseek" => "DEEPSEEK_API_KEY",
        "kimi" | "moonshot" => "MOONSHOT_API_KEY",
        "minimax" => "MINIMAX_API_KEY",
        "openrouter" => "OPENROUTER_API_KEY",
        "mistral" => "MISTRAL_API_KEY",
        "groq" => "GROQ_API_KEY",
        "together" => "TOGETHER_API_KEY",
        "fireworks" => "FIREWORKS_API_KEY",
        other => {
            // Unknown vendor → `{VENDOR}_API_KEY` so custom OpenAI-compat still works
            // once they also have an endpoint (or use OpenRouter).
            return Some(format!(
                "{}_API_KEY",
                other
                    .chars()
                    .map(|c| {
                        if c.is_ascii_alphanumeric() {
                            c.to_ascii_uppercase()
                        } else {
                            '_'
                        }
                    })
                    .collect::<String>()
                    .trim_matches('_')
                    .replace("__", "_")
            ));
        }
    };
    Some(env.to_string())
}

/// Known vendors the CLI can suggest (name → env var).
pub fn known_cloud_vendors() -> &'static [(&'static str, &'static str)] {
    &[
        ("openai", "OPENAI_API_KEY"),
        ("anthropic", "ANTHROPIC_API_KEY"),
        ("gemini", "GEMINI_API_KEY"),
        ("deepseek", "DEEPSEEK_API_KEY"),
        ("kimi", "MOONSHOT_API_KEY"),
        ("minimax", "MINIMAX_API_KEY"),
        ("openrouter", "OPENROUTER_API_KEY"),
        ("mistral", "MISTRAL_API_KEY"),
        ("groq", "GROQ_API_KEY"),
        ("together", "TOGETHER_API_KEY"),
        ("fireworks", "FIREWORKS_API_KEY"),
    ]
}

/// Upsert `KEY=value` in `~/.susi/cloud.env` (chmod 600 on Unix) and apply
/// into the current process, then register matching cloud providers.
pub fn register_api_key(vendor: &str, api_key: &str) -> Result<String, String> {
    let key = api_key.trim();
    if key.is_empty() {
        return Err("API key must not be empty".into());
    }
    let env_name = resolve_vendor_env_name(vendor)
        .ok_or_else(|| "vendor name must not be empty".to_string())?;
    let path = upsert_cloud_env_key(&env_name, key)?;
    // Force into this process even if a stale empty value existed.
    unsafe {
        std::env::set_var(&env_name, key);
    }
    apply_cloud_env_file();
    register_configured_cloud_endpoints(susi_core::registry::CapabilityRegistry::global());
    // Zero-config sticky pick: first registered vendor becomes preferred unless
    // the user already chose one (so a single `keys set` / env key is enough).
    let pref = crate::routing::InferenceRouter::load_preference();
    if pref.preferred_cloud.is_none() {
        let _ = crate::routing::InferenceRouter::set_preferred_cloud(vendor);
    }
    let registered: Vec<String> = susi_core::registry::CapabilityRegistry::global()
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

/// Remove a vendor key from `~/.susi/cloud.env` and the current process env.
pub fn remove_api_key(vendor: &str) -> Result<String, String> {
    let env_name = resolve_vendor_env_name(vendor)
        .ok_or_else(|| "vendor name must not be empty".to_string())?;
    let path = cloud_env_path();
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept = Vec::new();
    let mut removed = false;
    for raw in content.lines() {
        let trimmed = raw.trim();
        let check = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        if let Some((k, _)) = check.split_once('=') {
            if k.trim().eq_ignore_ascii_case(&env_name) {
                removed = true;
                continue;
            }
        }
        kept.push(raw.to_string());
    }
    if !removed
        && std::env::var(&env_name)
            .ok()
            .filter(|v| !v.is_empty())
            .is_none()
    {
        return Err(format!("{} was not registered", env_name));
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = if kept.is_empty() {
        String::new()
    } else {
        format!("{}\n", kept.join("\n"))
    };
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    unsafe {
        std::env::remove_var(&env_name);
    }
    Ok(format!("Removed {} from {}", env_name, path.display()))
}

/// Status of registered cloud keys (env var names only — never values).
pub fn list_api_key_status() -> Vec<(String, String, bool)> {
    let file_keys: std::collections::HashSet<String> = std::fs::read_to_string(cloud_env_path())
        .ok()
        .map(|c| parse_env_file(&c).into_iter().map(|(k, _)| k).collect())
        .unwrap_or_default();
    known_cloud_vendors()
        .iter()
        .map(|(vendor, env)| {
            let present = file_keys.contains(*env)
                || std::env::var(env).ok().filter(|v| !v.is_empty()).is_some();
            ((*vendor).to_string(), (*env).to_string(), present)
        })
        .collect()
}

fn upsert_cloud_env_key(env_name: &str, value: &str) -> Result<std::path::PathBuf, String> {
    let path = cloud_env_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;
    for raw in existing.lines() {
        let trimmed = raw.trim();
        let check = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        if let Some((k, _)) = check.split_once('=') {
            if k.trim().eq_ignore_ascii_case(env_name) {
                lines.push(format!("{}={}", env_name, value));
                replaced = true;
                continue;
            }
        }
        lines.push(raw.to_string());
    }
    if !replaced {
        if !lines.is_empty() && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            // keep file tidy
        }
        lines.push(format!("{}={}", env_name, value));
    }
    let body = format!("{}\n", lines.join("\n"));
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// Register every configured `inference_endpoints` entry that can be admitted:
/// cloud vendors when their API key is present, and any local/non-cloud
/// OpenAI-compat (or Anthropic/Gemini/Triton) base with an explicit model id.
/// Bundled OpenAI-compatible presets register themselves when the matching key
/// is present — no config.json edits required.
///
/// Zero-config contract: user only sets vendor API keys (shell env,
/// `~/.susi/cloud.env`, or `susi keys set <vendor>`).
pub fn register_configured_cloud_endpoints(registry: &susi_core::registry::CapabilityRegistry) {
    apply_cloud_env_file();
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

        let model = if endpoint.model.is_empty() {
            match protocol {
                InferenceProtocol::Anthropic => "claude-3-5-haiku-20241022".to_string(),
                InferenceProtocol::Gemini => "gemini-3.6-flash".to_string(),
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

/// Register the curated models catalog (~50). Each entry mounts when its
/// engine api_base is known and any required API key is present (local engines
/// with empty api_key_env always admit). Live `/models` discovery remains
/// unbounded on top of this ladder.
pub fn register_model_catalog(registry: &susi_core::registry::CapabilityRegistry) {
    apply_cloud_env_file();
    let endpoints = effective_inference_endpoints();
    let by_name: std::collections::BTreeMap<String, _> = endpoints
        .into_iter()
        .map(|e| (e.name.to_ascii_lowercase(), e))
        .collect();

    for entry in susi_sandbox::manager::SusiConfig::load_global()
        .unwrap_or_default()
        .model_catalog()
    {
        if entry.id.trim().is_empty() || entry.engine.trim().is_empty() {
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

/// Probe an OpenAI-compatible `/models` listing and register each model id.
/// Returns how many new providers were registered.
pub async fn register_openai_compat_models(
    registry: &susi_core::registry::CapabilityRegistry,
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

/// Zero-Config Autonomous Engine Discovery
/// Probes well-known local OpenAI-compat ports **and** every configured
/// `inference_endpoints` base URL — open admission, not a fixed vendor list.
pub async fn auto_discover_local_engines(registry: &susi_core::registry::CapabilityRegistry) {
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
    fn parse_env_file_supports_export_and_comments() {
        let parsed = parse_env_file(
            r#"
# comment
export DEEPSEEK_API_KEY=sk-deep
MOONSHOT_API_KEY="sk-kimi"
MINIMAX_API_KEY='sk-mm'
EMPTY=
INVALID LINE
OPENAI_API_KEY=sk-oai
"#,
        );
        assert_eq!(
            parsed,
            vec![
                ("DEEPSEEK_API_KEY".into(), "sk-deep".into()),
                ("MOONSHOT_API_KEY".into(), "sk-kimi".into()),
                ("MINIMAX_API_KEY".into(), "sk-mm".into()),
                ("OPENAI_API_KEY".into(), "sk-oai".into()),
            ]
        );
    }

    #[test]
    fn resolve_vendor_env_name_maps_aliases() {
        assert_eq!(
            resolve_vendor_env_name("deepseek").as_deref(),
            Some("DEEPSEEK_API_KEY")
        );
        assert_eq!(
            resolve_vendor_env_name("kimi").as_deref(),
            Some("MOONSHOT_API_KEY")
        );
        assert_eq!(
            resolve_vendor_env_name("gemini").as_deref(),
            Some("GEMINI_API_KEY")
        );
        assert_eq!(
            resolve_vendor_env_name("OPENAI_API_KEY").as_deref(),
            Some("OPENAI_API_KEY")
        );
    }

    #[test]
    fn upsert_and_register_roundtrip_in_temp_cloud_env() {
        let _guard = cloud_env_test_lock();
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
        let _guard = cloud_env_test_lock();
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

    fn cloud_env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        use std::sync::{Mutex, OnceLock};
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
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
