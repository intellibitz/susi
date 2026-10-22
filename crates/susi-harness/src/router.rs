//! Native multi-provider model routing.
//!
//! Directs completion requests directly to upstream provider HTTPS endpoints
//! (DeepSeek, SiliconFlow, DashScope, Gemini, Anthropic, OpenRouter) natively
//! without requiring any external Python proxy (LiteLLM) or background processes.

use susi_error::{EaiError, EaiResult};

/// Resolved upstream route for a model request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamRoute {
    /// Target HTTPS URL to post to.
    pub url: String,
    /// Model identifier the upstream provider expects.
    pub model_id: String,
    /// HTTP headers required for authentication and protocol negotiation.
    pub headers: Vec<(String, String)>,
    /// Whether the endpoint expects the Anthropic Messages API format.
    pub is_anthropic: bool,
}

/// Resolve the native route for a model request.
///
/// When `explicit_upstream` is set to an HTTP/HTTPS URL other than `"direct"`,
/// it routes to that URL with standard Bearer authorization (preserving
/// compatibility with test mock servers and custom gateways).
pub fn resolve_route(
    model: &str,
    explicit_upstream: Option<&str>,
    explicit_api_key: Option<&str>,
) -> EaiResult<UpstreamRoute> {
    let m = model.trim();
    let is_anthropic_model = m.starts_with("claude-") || m.starts_with("anthropic/");
    let anthropic_backend_model = match m {
        "claude-3-5-haiku" => "claude-3-5-haiku-20241022",
        "claude-3-5-sonnet" => "claude-3-5-sonnet-20241022",
        other => other.strip_prefix("anthropic/").unwrap_or(other),
    };

    if let Some(upstream) = explicit_upstream {
        let trimmed = upstream.trim();
        if !trimmed.is_empty() && trimmed != "direct" {
            let key = explicit_api_key.unwrap_or_default();
            let is_anthropic = is_anthropic_model || trimmed.ends_with("/messages");
            let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
            if is_anthropic {
                headers.push(("x-api-key".to_string(), key.to_string()));
                headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
                return Ok(UpstreamRoute {
                    url: trimmed.to_string(),
                    model_id: anthropic_backend_model.to_string(),
                    headers,
                    is_anthropic: true,
                });
            } else {
                if !key.is_empty() {
                    headers.push(("authorization".to_string(), format!("Bearer {key}")));
                }
                return Ok(UpstreamRoute {
                    url: trimmed.to_string(),
                    model_id: model.to_string(),
                    headers,
                    is_anthropic: false,
                });
            }
        }
    }

    let m = model.trim();

    // 1. DeepSeek
    if m == "deepseek-chat" || m == "deepseek-reasoner" || m.starts_with("deepseek/") {
        let key = env_key("DEEPSEEK_API_KEY", explicit_api_key)?;
        let backend_model = m.strip_prefix("deepseek/").unwrap_or(m);
        return Ok(UpstreamRoute {
            url: "https://api.deepseek.com/chat/completions".to_string(),
            model_id: backend_model.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), format!("Bearer {key}")),
            ],
            is_anthropic: false,
        });
    }

    // 2. SiliconFlow
    if m.starts_with("siliconflow-") || m.starts_with("siliconflow/") {
        let key = env_key("SILICONFLOW_API_KEY", explicit_api_key)?;
        let backend_model = match m {
            "siliconflow-qwen" => "Qwen/Qwen3-235B-A22B-Instruct-2507",
            "siliconflow-deepseek" => "deepseek-ai/DeepSeek-V3.2",
            "siliconflow-qwen-coder" => "Qwen/Qwen2.5-Coder-32B-Instruct",
            other => other
                .strip_prefix("siliconflow-")
                .or_else(|| other.strip_prefix("siliconflow/"))
                .unwrap_or(other),
        };
        return Ok(UpstreamRoute {
            url: "https://api.siliconflow.cn/v1/chat/completions".to_string(),
            model_id: backend_model.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), format!("Bearer {key}")),
            ],
            is_anthropic: false,
        });
    }

    // 3. DashScope (Alibaba Cloud)
    if m.starts_with("dashscope-") || m.starts_with("dashscope/") {
        let key = env_key("DASHSCOPE_API_KEY", explicit_api_key)?;
        let backend_model = match m {
            "dashscope-qwen-plus" => "qwen-plus",
            "dashscope-qwen-max" => "qwen-max",
            "dashscope-qwen-coder" => "qwen2.5-coder-32b-instruct",
            other => other
                .strip_prefix("dashscope-")
                .or_else(|| other.strip_prefix("dashscope/"))
                .unwrap_or(other),
        };
        return Ok(UpstreamRoute {
            url: "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions".to_string(),
            model_id: backend_model.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), format!("Bearer {key}")),
            ],
            is_anthropic: false,
        });
    }

    // 4. Google Gemini (OpenAI-compatible endpoint)
    if m.starts_with("gemini-") || m.starts_with("gemini/") {
        let key = env_key_any(&["GEMINI_API_KEY", "GOOGLE_API_KEY"], explicit_api_key)?;
        let backend_model = match m {
            "gemini-flash" => "gemini-2.5-flash",
            other => other.strip_prefix("gemini/").unwrap_or(other),
        };
        return Ok(UpstreamRoute {
            url: "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
                .to_string(),
            model_id: backend_model.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), format!("Bearer {key}")),
            ],
            is_anthropic: false,
        });
    }

    // 5. Anthropic Claude
    if m.starts_with("claude-") || m.starts_with("anthropic/") {
        let key = env_key("ANTHROPIC_API_KEY", explicit_api_key)?;
        let backend_model = match m {
            "claude-3-5-haiku" => "claude-3-5-haiku-20241022",
            "claude-3-5-sonnet" => "claude-3-5-sonnet-20241022",
            other => other.strip_prefix("anthropic/").unwrap_or(other),
        };
        return Ok(UpstreamRoute {
            url: "https://api.anthropic.com/v1/messages".to_string(),
            model_id: backend_model.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("x-api-key".to_string(), key),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
            ],
            is_anthropic: true,
        });
    }

    // 6. OpenRouter
    if m.starts_with("openrouter/") {
        let key = env_key("OPENROUTER_API_KEY", explicit_api_key)?;
        let backend_model = m.strip_prefix("openrouter/").unwrap_or(m);
        return Ok(UpstreamRoute {
            url: "https://openrouter.ai/api/v1/chat/completions".to_string(),
            model_id: backend_model.to_string(),
            headers: vec![
                ("content-type".to_string(), "application/json".to_string()),
                ("authorization".to_string(), format!("Bearer {key}")),
            ],
            is_anthropic: false,
        });
    }

    // Default: If an explicit API key or OpenAI key is set, attempt OpenAI format
    let key = env_key_any(&["OPENAI_API_KEY"], explicit_api_key)?;
    Ok(UpstreamRoute {
        url: "https://api.openai.com/v1/chat/completions".to_string(),
        model_id: m.to_string(),
        headers: vec![
            ("content-type".to_string(), "application/json".to_string()),
            ("authorization".to_string(), format!("Bearer {key}")),
        ],
        is_anthropic: false,
    })
}

fn env_key(name: &str, explicit: Option<&str>) -> EaiResult<String> {
    if let Some(key) = explicit {
        let t = key.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    match std::env::var(name) {
        Ok(v) if !v.trim().is_empty() => Ok(v.trim().to_string()),
        _ => Err(EaiError::config(format!(
            "environment variable {name} is unset or empty"
        ))),
    }
}

fn env_key_any(names: &[&str], explicit: Option<&str>) -> EaiResult<String> {
    if let Some(key) = explicit {
        let t = key.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    for name in names {
        if let Ok(v) = std::env::var(name) {
            let t = v.trim();
            if !t.is_empty() {
                return Ok(t.to_string());
            }
        }
    }
    Err(EaiError::config(format!(
        "none of {} are set in environment",
        names.join(" / ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_upstream_is_preferred_when_given() {
        let route = resolve_route(
            "any-model",
            Some("http://127.0.0.1:9999/v1/chat/completions"),
            Some("key123"),
        )
        .unwrap();
        assert_eq!(route.url, "http://127.0.0.1:9999/v1/chat/completions");
        assert_eq!(route.model_id, "any-model");
        assert_eq!(
            route.headers[1],
            ("authorization".to_string(), "Bearer key123".to_string())
        );
        assert!(!route.is_anthropic);
    }

    #[test]
    fn deepseek_routes_correctly() {
        let route = resolve_route("deepseek-chat", None, Some("test-ds-key")).unwrap();
        assert_eq!(route.url, "https://api.deepseek.com/chat/completions");
        assert_eq!(route.model_id, "deepseek-chat");
        assert_eq!(
            route.headers[1],
            (
                "authorization".to_string(),
                "Bearer test-ds-key".to_string()
            )
        );
        assert!(!route.is_anthropic);
    }

    #[test]
    fn anthropic_routes_with_headers_and_flag() {
        let route = resolve_route("claude-3-5-sonnet", None, Some("test-ant-key")).unwrap();
        assert_eq!(route.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(route.model_id, "claude-3-5-sonnet-20241022");
        assert!(route.is_anthropic);
        assert_eq!(
            route.headers[1],
            ("x-api-key".to_string(), "test-ant-key".to_string())
        );
        assert_eq!(
            route.headers[2],
            ("anthropic-version".to_string(), "2023-06-01".to_string())
        );
    }

    #[test]
    fn gemini_routes_to_openai_compat() {
        let route = resolve_route("gemini-flash", None, Some("test-gemini-key")).unwrap();
        assert_eq!(
            route.url,
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        assert_eq!(route.model_id, "gemini-2.5-flash");
        assert_eq!(
            route.headers[1],
            (
                "authorization".to_string(),
                "Bearer test-gemini-key".to_string()
            )
        );
        assert!(!route.is_anthropic);
    }
}
