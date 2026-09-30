//! Live management checks for cloud model APIs (Fireworks, Together, xAI,
//! Anthropic, OpenAI, Gemini, DeepInfra, DeepSeek, …): does the registered key
//! authenticate, and which models does the vendor serve right now?
//!
//! One read-only `GET <api_base>/models` per vendor. Auth headers follow the
//! endpoint's protocol. Vendor error bodies are never surfaced (they can echo
//! credentials or prompts) and keys never appear in results.
use crate::cloud::{effective_inference_endpoints, resolve_api_key};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyState {
    /// Key accepted; model list returned.
    Valid,
    /// 401/403: the vendor rejected the key.
    Rejected,
    /// 429: authenticated traffic is being throttled, key state unknown.
    RateLimited,
    /// No key registered for this vendor.
    Missing,
    /// Network/egress failure or unexpected status; nothing certified.
    Unreachable,
}

#[derive(Debug, Clone, Serialize)]
pub struct KeyCheck {
    pub vendor: String,
    pub api_base: String,
    pub state: KeyState,
    pub http_status: Option<u16>,
    pub default_model: String,
    /// The configured default model is served by the vendor (None when unknown).
    pub default_model_served: Option<bool>,
    pub model_count: usize,
    /// Live model ids, capped for display.
    pub models: Vec<String>,
}

const MODEL_CAP: usize = 50;

fn is_local(api_base: &str) -> bool {
    !crate::cloud::is_remote_cloud(api_base)
}

/// Auth headers for a vendor's model-list request.
fn auth_headers(protocol: &str, key: &str) -> Vec<(String, String)> {
    match protocol {
        "anthropic" => vec![
            ("x-api-key".into(), key.into()),
            ("anthropic-version".into(), "2023-06-01".into()),
        ],
        "gemini" => vec![("x-goog-api-key".into(), key.into())],
        _ => vec![("Authorization".into(), format!("Bearer {key}"))],
    }
}

/// Model ids from an OpenAI/Anthropic (`data[].id`), Gemini (`models[].name`)
/// or bare-array (Together, some others) listing.
pub fn parse_model_ids(body: &Value) -> Vec<String> {
    let rows = body
        .get("data")
        .or_else(|| body.get("models"))
        .unwrap_or(body)
        .as_array();
    rows.map(|rows| {
        rows.iter()
            .filter_map(|r| {
                r.get("id")
                    .or_else(|| r.get("name"))
                    .and_then(Value::as_str)
                    .map(|s| s.strip_prefix("models/").unwrap_or(s).to_string())
            })
            .collect()
    })
    .unwrap_or_default()
}

fn classify(status: u16) -> KeyState {
    match status {
        200..=299 => KeyState::Valid,
        401 | 403 => KeyState::Rejected,
        429 => KeyState::RateLimited,
        _ => KeyState::Unreachable,
    }
}

fn check_endpoint(name: &str, api_base: &str, protocol: &str, model: &str, key: &str) -> KeyCheck {
    let mut out = KeyCheck {
        vendor: name.to_string(),
        api_base: api_base.to_string(),
        state: KeyState::Missing,
        http_status: None,
        default_model: model.to_string(),
        default_model_served: None,
        model_count: 0,
        models: Vec::new(),
    };
    if key.is_empty() {
        return out;
    }
    out.state = KeyState::Unreachable;
    if !crate::susi_core::mac_policy::egress_permitted(api_base) {
        return out;
    }
    let headers = auth_headers(protocol, key);
    let refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let url = format!("{}/models", api_base.trim_end_matches('/'));
    let Ok(call) = susi_http_transport::http_call("GET", &url, &refs, 10, 0) else {
        return out;
    };
    out.http_status = Some(call.status);
    out.state = classify(call.status);
    // Provider quota/rate-limit headers are real response metadata — feed
    // the quota inventory before the body is consumed. Remote endpoints
    // only, matching the eligibility guard below.
    if !is_local(api_base) {
        crate::cloud_quota::record_quota_headers(
            crate::cloud_eligibility::Subject {
                provider: name,
                api_key: key,
                account: None,
                region: None,
                model,
            },
            call.headers(),
        );
    }
    if out.state == KeyState::Valid {
        let ids = call
            .into_bytes(8 * 1024 * 1024)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| parse_model_ids(&v))
            .unwrap_or_default();
        out.default_model_served = (!ids.is_empty() && !model.is_empty()).then(|| {
            ids.iter()
                .any(|id| id == model || id.ends_with(&format!("/{model}")))
        });
        out.model_count = ids.len();
        out.models = ids.into_iter().take(MODEL_CAP).collect();
    }
    // Probe evidence feeds the shared eligibility ledger; a listing success
    // is deliberately not recorded — it is not proof a model is usable.
    // Remote endpoints only: eligibility tracks cloud credentials, and the
    // guard also keeps loopback fixture tests from writing state.
    if is_local(api_base) {
        return out;
    }
    match out.state {
        KeyState::Rejected => crate::cloud_eligibility::record_probe(
            name,
            key,
            out.http_status,
            "vendor rejected the credential",
        ),
        KeyState::RateLimited => crate::cloud_eligibility::record_probe(
            name,
            key,
            out.http_status,
            "vendor throttled the probe",
        ),
        KeyState::Unreachable => crate::cloud_eligibility::record_probe(
            name,
            key,
            out.http_status,
            "endpoint unreachable from probe",
        ),
        KeyState::Valid | KeyState::Missing => {}
    }
    out
}

/// Check one vendor (name or alias, e.g. `grok`) or, with `None`, every remote
/// vendor endpoint that has a key registered plus those still missing one.
pub fn check(vendor: Option<&str>) -> Result<Vec<KeyCheck>, String> {
    let endpoints = effective_inference_endpoints();
    let wanted = vendor.map(|v| crate::cloud::resolve_vendor_env_name(v).unwrap_or_default());
    let mut out = Vec::new();
    for ep in endpoints
        .iter()
        .filter(|e| !is_local(&e.api_base) && !e.api_key_env.is_empty())
    {
        if let Some(env) = &wanted {
            if !ep.api_key_env.eq_ignore_ascii_case(env) {
                continue;
            }
        }
        let key = resolve_api_key(&ep.api_key_env, &ep.name);
        out.push(check_endpoint(
            &ep.name,
            &ep.api_base,
            &ep.protocol_type,
            &ep.model,
            &key,
        ));
    }
    if out.is_empty() {
        return Err(match vendor {
            Some(v) => format!("no cloud endpoint configured for vendor `{v}`"),
            None => "no cloud endpoints configured".to_string(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn parses_openai_gemini_and_bare_array_listings() {
        assert_eq!(
            parse_model_ids(&json!({"data":[{"id":"a"},{"id":"b"}]})),
            ["a", "b"]
        );
        assert_eq!(
            parse_model_ids(&json!({"models":[{"name":"models/gemini-x"}]})),
            ["gemini-x"]
        );
        assert_eq!(
            parse_model_ids(&json!([{"id":"t1"},{"id":"t2"}])),
            ["t1", "t2"]
        );
        assert!(parse_model_ids(&json!({"error":"x"})).is_empty());
    }

    #[test]
    fn auth_headers_follow_protocol() {
        assert_eq!(
            auth_headers("chat", "k")[0],
            ("Authorization".into(), "Bearer k".into())
        );
        assert_eq!(auth_headers("anthropic", "k")[0].0, "x-api-key");
        assert_eq!(auth_headers("gemini", "k")[0].0, "x-goog-api-key");
    }

    #[test]
    fn statuses_never_certify_an_ambiguous_key() {
        assert_eq!(classify(200), KeyState::Valid);
        assert_eq!(classify(401), KeyState::Rejected);
        assert_eq!(classify(403), KeyState::Rejected);
        assert_eq!(classify(429), KeyState::RateLimited);
        assert_eq!(classify(500), KeyState::Unreachable);
        assert_eq!(classify(404), KeyState::Unreachable);
    }

    #[test]
    fn empty_key_is_reported_missing_without_any_request() {
        let c = check_endpoint("X", "http://127.0.0.1:1/v1", "chat", "m", "");
        assert_eq!(c.state, KeyState::Missing);
        assert!(c.http_status.is_none());
    }

    fn serve(status: &'static str, body: &'static str, expect_header: &'static str) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", l.local_addr().unwrap());
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = l.accept() {
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_ascii_lowercase();
                assert!(req.starts_with("get /v1/models"));
                assert!(req.contains(expect_header), "{req}");
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        base
    }

    #[test]
    fn valid_key_lists_models_and_flags_default_model() {
        let base = serve(
            "200 OK",
            r#"{"data":[{"id":"m1"},{"id":"org/m2"}]}"#,
            "authorization: bearer sk-x",
        );
        let c = check_endpoint("V", &base, "chat", "m2", "sk-x");
        assert_eq!(c.state, KeyState::Valid);
        assert_eq!((c.model_count, c.default_model_served), (2, Some(true)));
        let base = serve(
            "200 OK",
            r#"{"data":[{"id":"m1"}]}"#,
            "authorization: bearer sk-x",
        );
        assert_eq!(
            check_endpoint("V", &base, "chat", "gone", "sk-x").default_model_served,
            Some(false)
        );
    }

    #[test]
    fn rejected_key_leaks_nothing() {
        let base = serve("401 Unauthorized", "secret-echo sk-x", "authorization");
        let c = check_endpoint("V", &base, "chat", "m", "sk-x");
        assert_eq!(c.state, KeyState::Rejected);
        assert!(!serde_json::to_string(&c).unwrap().contains("sk-x"));
    }
}
