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
    fn request_bodies_match_each_api() {
        assert_eq!(
            openai_chat_body("m", "p", 8),
            json!({"model": "m", "messages": [{"role": "user", "content": "p"}], "max_tokens": 8})
        );
        assert_eq!(openai_completions_body("m", "p", 8)["prompt"], "p");
        assert_eq!(triton_body("p", 8)["parameters"]["max_tokens"], 8);
    }
}
