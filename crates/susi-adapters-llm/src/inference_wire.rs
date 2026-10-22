//! Provider inference wire shapes shared by every outbound caller (GEMI
//! provider adapters, the gawd proxy agents, eval and benchmark legs):
//! request bodies and reply-text extraction for the OpenAI chat and
//! completions APIs, Anthropic Messages, Gemini `generateContent`, and
//! Triton's generate endpoint.
//!
//! Extractors return `Err` for an error envelope or an empty reply — an
//! empty answer must fail over to the next provider, never pass as text.

use serde_json::{json, Value};

/// A reply-text extractor, for callers that pick the protocol at runtime.
pub type Extractor = fn(&Value) -> Result<String, String>;

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
    /// Map a config `protocol_type` string onto a wire protocol.
    /// Unknown values default to OpenAI chat (the OpenAI-compat majority).
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

/// OpenAI `/chat/completions` body with one user message.
pub fn openai_chat_body(model: &str, prompt: &str, max_tokens: u32) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens
    })
}

/// OpenAI legacy `/completions` body.
pub fn openai_completions_body(model: &str, prompt: &str, max_tokens: u32) -> Value {
    json!({"model": model, "prompt": prompt, "max_tokens": max_tokens})
}

/// Triton generate-endpoint body.
pub fn triton_body(prompt: &str, max_tokens: u32) -> Value {
    json!({
        "text_input": prompt,
        "parameters": {"max_tokens": max_tokens, "bad_words": [], "stop_words": []}
    })
}

/// The retry body when a provider rejected `max_tokens` in favour of
/// `max_completion_tokens` (OpenAI reasoning models answer 400 with
/// `code: "unsupported_parameter"`, `param: "max_tokens"`). `max_tokens`
/// stays the first choice because older OpenAI-compatible servers (vLLM,
/// llama.cpp) only know that name — and a server that silently ignores an
/// unknown `max_completion_tokens` would lose the output cap entirely.
pub fn token_param_retry(body: &Value, error_body: &str) -> Option<Value> {
    let fields = body.as_object()?;
    let limit = fields.get("max_tokens")?;
    let error: Value = serde_json::from_str(error_body).unwrap_or(Value::Null);
    let unsupported = error.pointer("/error/code").and_then(Value::as_str)
        == Some("unsupported_parameter")
        && error.pointer("/error/param").and_then(Value::as_str) == Some("max_tokens");
    if !unsupported && !error_body.contains("max_completion_tokens") {
        return None;
    }
    let mut retry = fields.clone();
    retry.remove("max_tokens");
    retry.insert("max_completion_tokens".to_string(), limit.clone());
    Some(Value::Object(retry))
}

/// Blocking JSON POST for provider calls: reads error bodies (so the
/// provider's reason reaches the caller), retries once through
/// [`token_param_retry`] on a 400, and returns the parsed 2xx reply.
pub fn post_json(url: &str, headers: &[(&str, &str)], body: &Value) -> Result<Value, String> {
    post_json_timeout(url, headers, body, 30)
}

/// Same as [`post_json`] with an explicit recv timeout in seconds. Cloud
/// generation can exceed the 30s default used by eval/benchmark.
pub fn post_json_timeout(
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
    timeout_secs: u64,
) -> Result<Value, String> {
    let send = |body: &Value| -> Result<(u16, Vec<u8>), String> {
        let bytes = serde_json::to_vec(body).map_err(|e| format!("request json: {e}"))?;
        let call = susi_http_transport::http_call_with_body(
            "POST",
            url,
            headers,
            Some(&bytes),
            timeout_secs,
            0,
        )?;
        let status = call.status;
        let buf = call
            .into_bytes(16 * 1024 * 1024)
            .map_err(|e| format!("unreadable response body: {e}"))?;
        Ok((status, buf))
    };
    let (mut status, mut buf) = send(body)?;
    if status == 400 {
        let text = String::from_utf8_lossy(&buf);
        match token_param_retry(body, &text) {
            Some(retry) => {
                let next = send(&retry)?;
                status = next.0;
                buf = next.1;
            }
            None => return Err(http_error(400, &text)),
        }
    }
    if !(200..300).contains(&status) {
        let text = String::from_utf8_lossy(&buf);
        return Err(http_error(status, &text));
    }
    serde_json::from_slice(&buf).map_err(|e| format!("unreadable response body: {e}"))
}

fn http_error(status: u16, body: &str) -> String {
    format!(
        "HTTP {status}: {}",
        body.chars().take(200).collect::<String>()
    )
}

/// `Err` carrying the provider's message when `json` is an error envelope
/// (`{"error": {"message": …}}` or `{"error": "…"}`).
fn error_envelope(json: &Value) -> Result<(), String> {
    match json.get("error") {
        None | Some(Value::Null) => Ok(()),
        Some(err) => Err(format!(
            "provider error: {}",
            err.get("message")
                .and_then(Value::as_str)
                .map_or_else(|| err.to_string(), str::to_string)
        )),
    }
}

fn non_empty(text: Option<&str>, field: &str) -> Result<String, String> {
    match text {
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        Some(_) | None => Err(format!("response carried no {field} text")),
    }
}

/// Reply of a `/chat/completions` response; a `refusal` is an error.
pub fn openai_chat_text(json: &Value) -> Result<String, String> {
    error_envelope(json)?;
    let message = &json["choices"][0]["message"];
    if let Some(refusal) = message["refusal"].as_str() {
        return Err(format!("model refused: {refusal}"));
    }
    non_empty(message["content"].as_str(), "choices[0].message.content")
}

/// Reply of a legacy `/completions` response.
pub fn openai_completions_text(json: &Value) -> Result<String, String> {
    error_envelope(json)?;
    non_empty(json["choices"][0]["text"].as_str(), "choices[0].text")
}

/// Reply of a Triton generate response (`text_output`, or the KServe v2
/// `outputs[0].data[0]` shape).
pub fn triton_text(json: &Value) -> Result<String, String> {
    error_envelope(json)?;
    non_empty(
        json["text_output"]
            .as_str()
            .or_else(|| json["outputs"][0]["data"][0].as_str()),
        "text_output",
    )
}

/// All `text` blocks of a Messages API response, in order — a reply may
/// span several blocks (e.g. around tool_use or after thinking blocks).
pub fn anthropic_text(json: &Value) -> Result<String, String> {
    error_envelope(json)?;
    let text: String = json["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    non_empty(Some(&text), "content[].text")
}

/// Answer of a `generateContent` response: every non-thought part of the
/// first candidate, in order (thinking models emit `thought: true` parts
/// ahead of the answer).
pub fn gemini_text(json: &Value) -> Result<String, String> {
    error_envelope(json)?;
    let text: String = json["candidates"][0]["content"]["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p.get("thought").and_then(Value::as_bool) != Some(true))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    non_empty(Some(&text), "candidates[0].content.parts[].text")
}

/// Gemini model resource segment: config may name `gemini-x` or the full
/// `models/gemini-x` resource.
pub fn gemini_model_path(model: &str) -> &str {
    model.strip_prefix("models/").unwrap_or(model)
}

/// Convert an OpenAI-compatible `/chat/completions` payload to Anthropic `/v1/messages`.
///
/// Extracts any `system` message into the top-level `system` property, preserves `user`
/// and `assistant` turns, maps `max_tokens` (required by Anthropic, defaults to 4096),
/// and passes through `temperature`.
pub fn openai_to_anthropic_messages(openai: &Value) -> Result<Value, String> {
    let model = openai
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("claude-3-5-sonnet-20241022");
    let max_tokens = openai
        .get("max_tokens")
        .or_else(|| openai.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(4096);

    let mut system_prompt = String::new();
    let mut messages = Vec::new();

    if let Some(msg_array) = openai.get("messages").and_then(Value::as_array) {
        for msg in msg_array {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = msg
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if role == "system" {
                if !system_prompt.is_empty() {
                    system_prompt.push_str(
                        "

",
                    );
                }
                system_prompt.push_str(content);
            } else if role == "user" || role == "assistant" {
                messages.push(json!({
                    "role": role,
                    "content": content
                }));
            }
        }
    }

    if messages.is_empty() {
        return Err("no user or assistant messages found in payload".to_string());
    }

    let mut payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages
    });

    if !system_prompt.is_empty() {
        payload["system"] = json!(system_prompt);
    }

    if let Some(temp) = openai.get("temperature") {
        payload["temperature"] = temp.clone();
    }

    Ok(payload)
}

/// Convert an Anthropic Messages API response into an OpenAI-compatible `/chat/completions` reply.
pub fn anthropic_to_openai_chat(anthropic: &Value, model: &str) -> Result<Value, String> {
    error_envelope(anthropic)?;
    let text = anthropic_text(anthropic)?;

    let input_tokens = anthropic
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = anthropic
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let id = anthropic
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("anthropic-cmpl");

    Ok(json!({
        "id": id,
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": text
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extraction_is_complete_and_never_silently_empty() {
        let anthropic = json!({"content": [
            {"type": "thinking", "thinking": "hmm"},
            {"type": "text", "text": "Hello, "},
            {"type": "text", "text": "world"}
        ]});
        assert_eq!(anthropic_text(&anthropic).unwrap(), "Hello, world");
        let gemini = json!({"candidates": [{"content": {"parts": [
            {"text": "plan", "thought": true},
            {"text": "An"},
            {"text": "swer"}
        ]}}]});
        assert_eq!(gemini_text(&gemini).unwrap(), "Answer");
        assert_eq!(gemini_model_path("models/gemini-2.5-pro"), "gemini-2.5-pro");
        assert_eq!(gemini_model_path("gemini-2.5-pro"), "gemini-2.5-pro");

        let chat = json!({"choices": [{"message": {"role": "assistant", "content": "hi"}}]});
        assert_eq!(openai_chat_text(&chat).unwrap(), "hi");
        let refused = json!({"choices": [{"message": {"content": null, "refusal": "no"}}]});
        assert!(openai_chat_text(&refused).unwrap_err().contains("refused"));
        let empty = json!({"choices": [{"message": {"content": ""}}]});
        assert!(openai_chat_text(&empty).is_err());
        assert_eq!(
            openai_completions_text(&json!({"choices": [{"text": "t"}]})).unwrap(),
            "t"
        );
        assert_eq!(triton_text(&json!({"text_output": "x"})).unwrap(), "x");
        assert_eq!(
            triton_text(&json!({"outputs": [{"data": ["y"]}]})).unwrap(),
            "y"
        );
    }

    #[test]
    fn error_envelopes_surface_the_provider_message() {
        let err = json!({"error": {"message": "bad key", "type": "invalid_request_error"}});
        assert_eq!(
            openai_chat_text(&err).unwrap_err(),
            "provider error: bad key"
        );
        let plain = json!({"error": "overloaded"});
        assert!(anthropic_text(&plain).unwrap_err().contains("overloaded"));
    }

    #[test]
    fn token_param_retry_only_on_the_documented_rejection() {
        let body = openai_chat_body("o3", "p", 64);
        let openai = r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","type":"invalid_request_error","param":"max_tokens","code":"unsupported_parameter"}}"#;
        let retry = token_param_retry(&body, openai).unwrap();
        assert_eq!(retry["max_completion_tokens"], 64);
        assert!(retry.get("max_tokens").is_none());
        assert_eq!(retry["messages"], body["messages"]);
        let other = r#"{"error":{"message":"model not found","param":"model"}}"#;
        assert!(token_param_retry(&body, other).is_none());
        assert!(token_param_retry(&triton_body("p", 8), openai).is_none());
    }

    /// A reasoning-model endpoint that rejects `max_tokens` is served on the
    /// retry; the retry is sent exactly once.
    #[test]
    fn post_json_retries_rejected_token_param_once() {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream);
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let body: Value = serde_json::from_slice(&body).unwrap();
                let (status, reply) = if body.get("max_tokens").is_some() {
                    (
                        "400 Bad Request",
                        r#"{"error":{"param":"max_tokens","code":"unsupported_parameter","message":"Use 'max_completion_tokens' instead."}}"#,
                    )
                } else {
                    ("200 OK", r#"{"choices":[{"message":{"content":"ok"}}]}"#)
                };
                bodies.push(body);
                let mut stream = reader.into_inner();
                write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            bodies
        });
        let url = format!("http://{addr}/v1/chat/completions");
        let reply = post_json(&url, &[], &openai_chat_body("o3", "p", 32)).unwrap();
        assert_eq!(openai_chat_text(&reply).unwrap(), "ok");
        let bodies = server.join().unwrap();
        assert_eq!(bodies[1]["max_completion_tokens"], 32);
    }

    #[test]
    fn request_bodies_match_each_api() {
        assert_eq!(
            openai_chat_body("m", "p", 8),
            json!({"model": "m", "messages": [{"role": "user", "content": "p"}], "max_tokens": 8})
        );
        assert_eq!(openai_completions_body("m", "p", 8)["prompt"], "p");
        assert_eq!(triton_body("p", 8)["parameters"]["max_tokens"], 8);
    }

    #[test]
    fn openai_to_anthropic_and_back_round_trips() {
        let openai_req = json!({
            "model": "claude-3-5-sonnet",
            "messages": [
                {"role": "system", "content": "You are a test assistant."},
                {"role": "user", "content": "Hello Claude"}
            ],
            "max_tokens": 1024,
            "temperature": 0.5
        });

        let anthropic_req = openai_to_anthropic_messages(&openai_req).unwrap();
        assert_eq!(anthropic_req["model"], "claude-3-5-sonnet");
        assert_eq!(anthropic_req["system"], "You are a test assistant.");
        assert_eq!(anthropic_req["max_tokens"], 1024);
        assert_eq!(anthropic_req["messages"][0]["role"], "user");
        assert_eq!(anthropic_req["messages"][0]["content"], "Hello Claude");

        let anthropic_resp = json!({
            "id": "msg_123",
            "content": [
                {"type": "text", "text": "Hello human!"}
            ],
            "usage": {
                "input_tokens": 15,
                "output_tokens": 8
            }
        });

        let openai_resp = anthropic_to_openai_chat(&anthropic_resp, "claude-3-5-sonnet").unwrap();
        assert_eq!(
            openai_resp["choices"][0]["message"]["content"],
            "Hello human!"
        );
        assert_eq!(openai_resp["usage"]["prompt_tokens"], 15);
        assert_eq!(openai_resp["usage"]["completion_tokens"], 8);
        assert_eq!(openai_resp["usage"]["total_tokens"], 23);
    }
}
