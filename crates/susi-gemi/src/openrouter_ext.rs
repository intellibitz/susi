//! Engines-side OpenRouter helpers (live `/models`, paid probe).
use anyhow::{bail, Context, Result};
use susi_core::provider::Provider;
use susi_gemi_models::openrouter::{
    attribution_headers, is_openrouter_base, OpenRouterManager, API_BASE,
};

use crate::http_provider::{HttpProvider, InferenceProtocol};
use crate::routing::InferenceRouter;

/// Build an [`HttpProvider`] for the effective OpenRouter model pin.
pub fn provider(manager: &OpenRouterManager) -> Result<HttpProvider> {
    manager.doctor()?;
    Ok(HttpProvider {
        name: format!("openrouter-{}", manager.effective_model().replace('/', "-")),
        api_base: API_BASE.to_string(),
        model: manager.effective_model(),
        protocol: InferenceProtocol::OpenAiChat,
        api_key: manager.resolve_api_key(),
    })
}

/// Prefer OpenRouter for cloud routing and optionally pin a model/route.
pub fn prefer(model: Option<&str>, clear_model: bool) -> Result<String> {
    let manager = OpenRouterManager::new()?;
    if clear_model {
        let cleared = manager.clear_preferred_model()?;
        let _ = InferenceRouter::set_preferred_cloud("openrouter");
        return Ok(format!("{cleared}; cloud preference → openrouter"));
    }
    manager.doctor()?;
    let pin = manager.prefer(model)?;
    let cloud =
        InferenceRouter::set_preferred_cloud("openrouter").map_err(|e| anyhow::anyhow!(e))?;
    Ok(format!("{pin} | {cloud}"))
}

/// Paid probe against the effective OpenRouter model.
pub fn probe(prompt: &str) -> Result<String> {
    if prompt.trim().is_empty() {
        bail!("probe prompt must not be empty");
    }
    let manager = OpenRouterManager::new()?;
    let provider = provider(&manager)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime for OpenRouter probe")?;
    let text = runtime
        .block_on(provider.generate(prompt))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(redact_secrets(&text))
}

/// Live model ids from OpenRouter `GET /models` (requires key). Caps list size.
pub fn list_live(limit: usize) -> Result<Vec<String>> {
    let manager = OpenRouterManager::new()?;
    manager.doctor()?;
    let key = manager.resolve_api_key();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime for OpenRouter models")?;
    runtime.block_on(fetch_live_models(&key, limit))
}

async fn fetch_live_models(api_key: &str, limit: usize) -> Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("http client")?;
    let url = format!("{API_BASE}/models");
    let (referer, title) = attribution_headers();
    let mut req = client.get(&url).bearer_auth(api_key);
    if is_openrouter_base(API_BASE) {
        req = req
            .header("HTTP-Referer", &referer)
            .header("X-Title", &title);
    }
    let res = req.send().await.context("OpenRouter /models request")?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        bail!(
            "OpenRouter /models HTTP {}: {}",
            status,
            body.chars().take(200).collect::<String>()
        );
    }
    let json: serde_json::Value = res.json().await.context("OpenRouter /models json")?;
    let mut ids = Vec::new();
    if let Some(arr) = json.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                ids.push(id.to_string());
                if ids.len() >= limit {
                    break;
                }
            }
        }
    }
    if ids.is_empty() {
        bail!("OpenRouter /models returned no model ids");
    }
    Ok(ids)
}

fn redact_secrets(text: &str) -> String {
    let mut result = text.to_owned();
    for (key, value) in std::env::vars() {
        if value.len() >= 8
            && (key.ends_with("_API_KEY") || key.ends_with("_TOKEN") || key.ends_with("_SECRET"))
        {
            result = result.replace(&value, "[REDACTED]");
        }
    }
    susi_core::redact::redact_patterns(&["sk-".into(), "sk-or-".into(), "ghp_".into()], &result)
}
