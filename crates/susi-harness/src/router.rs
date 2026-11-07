//! Native multi-provider model routing.
//!
//! Directs completion requests straight to upstream provider HTTPS endpoints
//! (DeepSeek, SiliconFlow, DashScope, Gemini, Anthropic, OpenRouter), with no
//! intermediate proxy and no background process.
//!
//! Model ids, endpoints and capabilities live in [`crate::providers`]; this
//! module turns a resolved model into the request shape a provider expects.
//! An explicit upstream still wins, so a mock server or a self-hosted gateway
//! can stand in for any provider.

use crate::providers::{ResolvedModel, Wire};
use crate::secrets;
use std::fmt;
use susi_error::{EaiError, EaiResult};

/// Resolved upstream route for a model request.
#[derive(Clone, PartialEq, Eq)]
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

impl UpstreamRoute {
    /// Verify structural and authentication invariants of a resolved upstream route.
    ///
    /// # Errors
    ///
    /// Returns a descriptive error string when the URL is not HTTP/HTTPS,
    /// `model_id` is empty, headers are empty or contain blank names/values,
    /// `content-type: application/json` is missing, or protocol-specific
    /// authentication headers (`x-api-key` + `anthropic-version` for Anthropic,
    /// `Bearer <token>` for OpenAI-compatible routes) are malformed.
    pub fn verify_route_invariants(&self) -> Result<(), String> {
        let url = self.url.trim();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!(
                "upstream route url '{}' must start with http:// or https://",
                self.url
            ));
        }
        if self.model_id.trim().is_empty() {
            return Err("upstream route model_id must not be empty".to_string());
        }
        if self.headers.is_empty() {
            return Err("upstream route headers must not be empty".to_string());
        }
        for (name, value) in &self.headers {
            if name.trim().is_empty() {
                return Err("upstream route header name must not be empty".to_string());
            }
            if value.trim().is_empty() {
                return Err(format!(
                    "upstream route header '{name}' value must not be empty"
                ));
            }
        }
        let has_json_content_type = self
            .headers
            .iter()
            .any(|(k, v)| k.eq_ignore_ascii_case("content-type") && v == "application/json");
        if !has_json_content_type {
            return Err(
                "upstream route must include content-type: application/json header".to_string(),
            );
        }

        if self.is_anthropic {
            let has_api_key = self
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("x-api-key"));
            let has_version = self
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("anthropic-version"));
            let has_bearer = self
                .headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
            if !has_api_key || !has_version {
                return Err(
                    "anthropic upstream route must include x-api-key and anthropic-version headers"
                        .to_string(),
                );
            }
            if has_bearer {
                return Err(
                    "anthropic upstream route must not include authorization header".to_string(),
                );
            }
        } else {
            let has_anthropic_header = self.headers.iter().any(|(k, _)| {
                k.eq_ignore_ascii_case("x-api-key") || k.eq_ignore_ascii_case("anthropic-version")
            });
            if has_anthropic_header {
                return Err(
                    "non-anthropic upstream route must not include anthropic headers".to_string(),
                );
            }
            for (k, v) in &self.headers {
                if k.eq_ignore_ascii_case("authorization") {
                    let token = v.strip_prefix("Bearer ").map(str::trim).unwrap_or("");
                    if token.is_empty() {
                        return Err(
                            "authorization header must be 'Bearer <non-empty-token>'".to_string()
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

impl fmt::Debug for UpstreamRoute {
    /// The route as a log line: the credential headers are redacted, so a
    /// `{:?}` of a route can never print an API key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let headers: Vec<(&str, &str)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let secret = matches!(name.as_str(), "authorization" | "x-api-key");
                (
                    name.as_str(),
                    if secret { "<redacted>" } else { value.as_str() },
                )
            })
            .collect();
        f.debug_struct("UpstreamRoute")
            .field("url", &self.url)
            .field("model_id", &self.model_id)
            .field("headers", &headers)
            .field("is_anthropic", &self.is_anthropic)
            .finish()
    }
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
        "claude-haiku" => "claude-haiku-5-5",
        "claude-sonnet" => "claude-sonnet-5-5",
        other => other.strip_prefix("anthropic/").unwrap_or(other),
    };
    let is_anthropic = model.starts_with("claude-")
        || model.starts_with("anthropic/")
        || upstream.ends_with("/messages");
    let mut headers = vec![("content-type".to_string(), "application/json".to_string())];
    if is_anthropic {
        headers.push(("x-api-key".to_string(), key.to_string()));
        headers.push(("anthropic-version".to_string(), "2023-06-01".to_string()));
        if let Some(workspace) = anthropic_workspace() {
            headers.push(("anthropic-workspace-id".to_string(), workspace));
        }
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
        if let Some(workspace) = anthropic_workspace() {
            headers.push(("anthropic-workspace-id".to_string(), workspace));
        }
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

/// The workspace an Anthropic request must name, when the key is not scoped to
/// one.
///
/// A user-scoped console key (`sk-ant-usr-…`) is accepted but every request is
/// then refused with `invalid_request_error` demanding an
/// `anthropic-workspace-id` header; the harness had no way to send one, so such
/// a key could not be used at all. `ANTHROPIC_WORKSPACE_ID` supplies it, and an
/// absent or blank value keeps the previous behaviour (no header) for keys that
/// are already workspace-scoped.
fn anthropic_workspace() -> Option<String> {
    std::env::var("ANTHROPIC_WORKSPACE_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_key_any(names: &[&str], explicit: Option<&str>) -> EaiResult<String> {
    if let Some(key) = explicit {
        let t = key.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    for name in names {
        if let Some(key) = secrets::lookup(name) {
            return Ok(key.expose().to_string());
        }
    }
    Err(EaiError::config(format!(
        "none of {} are set in the environment or the user and machine scopes",
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
        let route = resolve_route("deepseek-flash", None, Some("test-ds-key")).unwrap();
        assert_eq!(route.url, "https://api.deepseek.com/chat/completions");
        assert_eq!(route.model_id, "deepseek-flash");
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
        let route = resolve_route("claude-sonnet", None, Some("test-ant-key")).unwrap();
        assert_eq!(route.url, "https://api.anthropic.com/v1/messages");
        assert_eq!(route.model_id, "claude-sonnet-5-5");
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

    /// A console key that is not workspace-scoped is refused by the API unless
    /// the request names the workspace; without this header such a key could
    /// not be used at all.
    #[test]
    fn an_anthropic_workspace_is_sent_when_configured() {
        let previous = std::env::var("ANTHROPIC_WORKSPACE_ID").ok();
        std::env::set_var("ANTHROPIC_WORKSPACE_ID", "  wrkspc_test  ");
        let route = resolve_route("claude-sonnet", None, Some("test-ant-key")).unwrap();
        assert!(
            route
                .headers
                .iter()
                .any(|(name, value)| name == "anthropic-workspace-id" && value == "wrkspc_test"),
            "a configured workspace must be trimmed and sent: {:?}",
            route.headers
        );
        // An explicit upstream keeps the workspace too: it is the same wire.
        let direct = resolve_route("claude-sonnet", Some("direct"), Some("k")).unwrap();
        assert!(direct
            .headers
            .iter()
            .any(|(name, _)| name == "anthropic-workspace-id"));

        std::env::remove_var("ANTHROPIC_WORKSPACE_ID");
        let bare = resolve_route("claude-sonnet", None, Some("test-ant-key")).unwrap();
        assert!(
            !bare
                .headers
                .iter()
                .any(|(name, _)| name == "anthropic-workspace-id"),
            "an unset workspace must not send an empty header"
        );
        if let Some(value) = previous {
            std::env::set_var("ANTHROPIC_WORKSPACE_ID", value);
        }
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
        assert_eq!(route.model_id, "deepseek-flash");
    }

    #[test]
    fn direct_mode_ignores_the_upstream_url() {
        let route = resolve_route("deepseek-flash", Some("direct"), Some("test-key")).unwrap();
        assert_eq!(route.url, "https://api.deepseek.com/chat/completions");
    }

    #[test]
    fn an_unknown_model_falls_back_to_openai() {
        let route = resolve_route("mystery-model", None, Some("test-openai-key")).unwrap();
        assert_eq!(route.url, "https://api.openai.com/v1/chat/completions");
        assert_eq!(route.model_id, "mystery-model");
    }

    #[test]
    fn upstream_route_verify_route_invariants_validates_and_rejects_malformed_routes() {
        let ds = resolve_route("deepseek-flash", None, Some("test-ds-key")).unwrap();
        assert!(ds.verify_route_invariants().is_ok());

        let ant = resolve_route("claude-sonnet", None, Some("test-ant-key")).unwrap();
        assert!(ant.verify_route_invariants().is_ok());

        let gem = resolve_route("gemini-flash", None, Some("test-gem-key")).unwrap();
        assert!(gem.verify_route_invariants().is_ok());

        let mut bad_scheme = ds.clone();
        bad_scheme.url = "ws://api.deepseek.com/chat/completions".to_string();
        assert!(bad_scheme.verify_route_invariants().is_err());

        let mut empty_model = ds.clone();
        empty_model.model_id = "   ".to_string();
        assert!(empty_model.verify_route_invariants().is_err());

        let mut no_headers = ds.clone();
        no_headers.headers.clear();
        assert!(no_headers.verify_route_invariants().is_err());

        let mut blank_header_val = ant.clone();
        blank_header_val.headers[1].1 = "   ".to_string();
        assert!(blank_header_val.verify_route_invariants().is_err());

        let mut wrong_wire_headers = ant;
        wrong_wire_headers.is_anthropic = false;
        assert!(wrong_wire_headers.verify_route_invariants().is_err());

        let mut bad_bearer = ds;
        bad_bearer.headers[1] = ("authorization".to_string(), "Basic abc".to_string());
        assert!(bad_bearer.verify_route_invariants().is_err());
    }
}
