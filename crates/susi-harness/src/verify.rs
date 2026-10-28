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
}
