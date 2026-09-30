use crate::cloud_eligibility::{inference_observation, record, state_path, Target};
use susi_core::inference_wire as wire;
use susi_error::{EaiError, EaiResult};
use wire::InferenceProtocol;

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
    if crate::openrouter::is_openrouter_base(api_base) {
        let (referer, title) = crate::openrouter::attribution_headers();
        headers.push(("HTTP-Referer".into(), referer));
        headers.push(("X-Title".into(), title));
    }
    headers
}

/// Run actual inference and persist its availability evidence. Catalog and
/// health probes do not call this function and cannot fabricate success.
pub fn generate(
    api_base: &str,
    model: &str,
    api_key: &str,
    protocol: InferenceProtocol,
    prompt: &str,
) -> EaiResult<String> {
    generate_observed(
        Request {
            api_base,
            model,
            api_key,
            protocol,
            prompt,
        },
        &state_path(),
        crate::cloud::is_remote_cloud(api_base),
    )
}

struct Request<'a> {
    api_base: &'a str,
    model: &'a str,
    api_key: &'a str,
    protocol: InferenceProtocol,
    prompt: &'a str,
}

fn generate_observed(
    request: Request<'_>,
    path: &std::path::Path,
    track: bool,
) -> EaiResult<String> {
    let Request {
        api_base,
        model,
        api_key,
        protocol,
        prompt,
    } = request;
    let result = generate_wire(api_base, model, api_key, protocol, prompt);
    if track {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let observation = inference_observation(
            Target::new(api_base, api_key, model, None, ""),
            result.as_ref().map(|_| ()).map_err(String::as_str),
            now,
        );
        record(path, observation)?;
    }
    result.map_err(|_| {
        EaiError::inference("cloud inference failed; inspect redacted availability state")
    })
}

fn generate_wire(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_eligibility::{Availability, Eligibility};
    use std::io::{Read, Write};

    #[test]
    fn cloud_eligibility_state_real_inference_records_success_and_credit_failure() {
        let _lock = crate::cloud_eligibility::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = tempfile::tempdir().unwrap();
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", root.path())
            .set("XDG_DATA_HOME", root.path().join("data"))
            .set("XDG_CONFIG_HOME", root.path().join("config"));
        let path = root.path().join("availability.json");
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for (status, body) in [
                (
                    "200 OK",
                    r#"{"choices":[{"message":{"content":"verified answer"}}]}"#,
                ),
                (
                    "402 Payment Required",
                    r#"{"error":{"message":"insufficient balance secret-canary"}}"#,
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buf = [0; 1024];
                loop {
                    let count = stream.read(&mut buf).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buf[..count]);
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&request[..end]);
                        let length: usize = headers
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse().unwrap())
                            })
                            .unwrap();
                        if request.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let text = String::from_utf8_lossy(&request);
                assert!(text.contains("POST /chat/completions"));
                assert!(text.contains("model-a"));
                write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
        });
        let target = Target::new(&base, "secret-canary", "model-a", None, "");
        assert_eq!(
            generate_observed(
                Request {
                    api_base: &base,
                    model: "model-a",
                    api_key: "secret-canary",
                    protocol: InferenceProtocol::OpenAiChat,
                    prompt: "answer"
                },
                &path,
                true
            )
            .unwrap(),
            "verified answer"
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert_eq!(
            Eligibility::load(&path).unwrap().state(&target, now),
            Availability::Working
        );
        assert!(generate_observed(
            Request {
                api_base: &base,
                model: "model-a",
                api_key: "secret-canary",
                protocol: InferenceProtocol::OpenAiChat,
                prompt: "answer"
            },
            &path,
            true
        )
        .is_err());
        assert_eq!(
            Eligibility::load(&path).unwrap().state(&target, now),
            Availability::InsufficientCredit
        );
        assert!(!std::fs::read_to_string(path)
            .unwrap()
            .contains("secret-canary"));
        worker.join().unwrap();
    }
}
