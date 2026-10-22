//! Native multi-provider model routing.
//!
//! Directs completion requests directly to upstream provider HTTPS endpoints
//! (DeepSeek, SiliconFlow, DashScope, Gemini, Anthropic, OpenRouter) natively
//! without requiring any external Python proxy (LiteLLM) or background
//! processes.
//!
//! Model ids, endpoints and capabilities live in [`crate::providers`]; this
//! module turns a resolved model into the request shape a provider expects.
//! An explicit upstream still wins, so a mock server or a self-hosted gateway
//! can stand in for any provider.

use crate::providers::{ResolvedModel, Wire};
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
///
/// # Errors
///
/// Returns a config error when the provider's key is unset, or when the
/// resolved provider has no usable endpoint.
pub fn resolve_route(
    model: &str,
    explicit_upstream: Option<&str>,
    explicit_api_key: Option<&str>,
) -> EaiResult<UpstreamRoute> {
    let m = model.trim();

    if let Some(upstream) = explicit_upstream {
        let trimmed = upstream.trim();
        if !trimmed.is_empty() && trimmed != "direct" {
            return explicit_route(m, trimmed, explicit_api_key);
        }
    }

    if let Some(resolved) = crate::providers::Registry::builtin().resolve(m) {
        return route_from_model(&resolved, explicit_api_key);
    }

    // Default: an unknown model goes to OpenAI in its native shape. The
    // registry deliberately does not guess at capabilities it has no record
    // for, and this is the one place that fallback is chosen.
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

/// Route to an explicit upstream URL, keeping the provider's wire shape.
fn explicit_route(
    model: &str,
    upstream: &str,
    explicit_api_key: Option<&str>,
) -> EaiResult<UpstreamRoute> {
    let key = explicit_api_key.unwrap_or_default();
    let backend_model = match model {
        "claude-3-5-haiku" => "claude-3-5-haiku-20241022",
        "claude-3-5-sonnet" => "claude-3-5-sonnet-20241022",
        other => other.strip_prefix("anthropic/").unwrap_or(other),
    };
    let is_anthropic = model.starts_with("claude-")
        || model.starts_with("anthropic/")
        || upstream.ends_with("/messages");
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    if is_anthropic {
        headers.push(("x-api-key".to_string(), key.to_string()));
        headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
    } else if !key.trim().is_empty() {
        headers.push(("authorization".to_string(), format!("Bearer {key}")));
    }
    Ok(UpstreamRoute {
        url: upstream.to_string(),
        model_id: backend_model.to_string(),
        headers,
        is_anthropic,
    })
}

/// Build a route from a registry record.
fn route_from_model(
    resolved: &ResolvedModel,
    explicit_api_key: Option<&str>,
) -> EaiResult<UpstreamRoute> {
    let is_anthropic = resolved.wire == Wire::Anthropic;
    let key = if resolved.key_envs.is_empty() {
        explicit_api_key.unwrap_or_default().trim().to_string()
    } else {
        let names = resolved
            .key_envs
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        env_key_any(&names, explicit_api_key)?
    };
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    if is_anthropic {
        headers.push(("x-api-key".to_string(), key));
        headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
    } else if !key.is_empty() {
        headers.push(("authorization".to_string(), format!("Bearer {key}")));
    }
    Ok(UpstreamRoute {
        url: resolved.endpoint.clone(),
        model_id: resolved.model_id.clone(),
        headers,
        is_anthropic,
    })
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
        assert_eq!(route.model_id, "gemini-3.8-flash");
        assert_eq!(
            route.headers[1],
            (
                "authorization".to_string(),
                "Bearer test-gemini-key".to_string()
            )
        );
        assert!(!route.is_anthropic);
    }

    #[test]
    fn an_alias_resolves_to_its_provider() {
        let route = resolve_route("coder", None, Some("test-key")).unwrap();
        assert_eq!(route.url, "https://api.deepseek.com/chat/completions");
        assert_eq!(route.model_id, "deepseek-chat");
    }

    #[test]
    fn direct_mode_ignores_the_upstream_url() {
        let route = resolve_route("deepseek-chat", Some("direct"), Some("test-key")).unwrap();
        assert_eq!(route.url, "https://api.deepseek.com/chat/completions");
    }

    #[test]
    fn an_unknown_model_falls_back_to_openai() {
        let route = resolve_route("mystery-model", None, Some("test-openai-key")).unwrap();
        assert_eq!(route.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(route.model_id, "mystery-model");
    }
}
