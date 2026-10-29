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
    let lower = model.to_lowercase();
    let mut val = json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "max_tokens": max_tokens
    });
    if lower.contains("kimi") || lower.contains("moonshot") {
        if let Some(obj) = val.as_object_mut() {
            obj.insert("thinking".into(), json!({"type": "enabled"}));
        }
    }
    val
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

/// The retry body when a provider rejected `max_tokens` or returned a provider-specific
/// payload error like Moonshot/Kimi's `invalid thinking: only type=enabled is allowed for this model`.
pub fn token_param_retry(body: &Value, error_body: &str) -> Option<Value> {
    let fields = body.as_object()?;

    // Moonshot / Kimi thinking parameter adjustment retry
    if error_body.contains("invalid thinking")
        || error_body.contains("type=enabled")
        || error_body.contains("MoonshotException")
    {
        let mut retry = fields.clone();
        retry.insert(
            "thinking".to_string(),
            serde_json::json!({"type": "enabled"}),
        );
        return Some(Value::Object(retry));
    }

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
/// and `assistant` turns, and maps `max_tokens` (required by Anthropic, defaults to 4096).
///
/// **Tool calling is translated**, because a worker turn is a tool loop: OpenAI
/// `tools` become Messages `tools` (`function.parameters` → `input_schema`), an
/// assistant `tool_calls` array becomes `tool_use` blocks, and a `tool` result
/// becomes a `tool_result` block in the user turn that answers it. Without this
/// a Claude turn could answer but never act.
///
/// **Sampling parameters are deliberately not forwarded.** The current Messages
/// models deprecate `temperature`, `top_p` and `top_k` — the API answers
/// `invalid_request_error: "`temperature` is deprecated for this model."` — so a
/// payload that carried them would fail every call. Dropping them lets the model
/// use its own defaults, which is the only sampling the API allows.
pub fn openai_to_anthropic_messages(openai: &Value) -> Result<Value, String> {
    let model = openai
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("claude-sonnet-5-5");
    let max_tokens = openai
        .get("max_tokens")
        .or_else(|| openai.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(4096);

    let mut system_prompt = String::new();
    let mut messages: Vec<Value> = Vec::new();

    if let Some(msg_array) = openai.get("messages").and_then(Value::as_array) {
        for msg in msg_array {
            let role = msg.get("role").and_then(Value::as_str).unwrap_or("user");
            let text = content_text(msg);
            match role {
                "system" => {
                    if !text.is_empty() {
                        if !system_prompt.is_empty() {
                            system_prompt.push_str("\n\n");
                        }
                        system_prompt.push_str(&text);
                    }
                }
                "assistant" => {
                    let mut blocks = Vec::new();
                    if !text.is_empty() {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                    for call in msg
                        .get("tool_calls")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let name = call
                            .pointer("/function/name")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }
                        let id = call
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or("call_unknown");
                        // Arguments arrive as a JSON string; Anthropic wants the
                        // parsed object. Unparseable arguments become `{}` rather
                        // than failing the whole turn — the tool will refuse them.
                        let input = call
                            .pointer("/function/arguments")
                            .and_then(Value::as_str)
                            .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                            .unwrap_or_else(|| json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input
                        }));
                    }
                    if blocks.is_empty() {
                        continue;
                    }
                    messages.push(json!({"role": "assistant", "content": blocks}));
                }
                "tool" => {
                    let id = msg
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let block = json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": text
                    });
                    // Parallel tool calls: results belong to one user turn.
                    match messages.last_mut() {
                        Some(previous) if is_tool_results(previous) => {
                            if let Some(blocks) =
                                previous.get_mut("content").and_then(Value::as_array_mut)
                            {
                                blocks.push(block);
                            }
                        }
                        _ => messages.push(json!({"role": "user", "content": [block]})),
                    }
                }
                _ => {
                    if !text.is_empty() {
                        messages.push(json!({"role": "user", "content": text}));
                    }
                }
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
        // The system prompt is the stable prefix of every turn in a worker run,
        // so it is where prompt caching pays. The Messages API caches nothing
        // without an explicit breakpoint (verified live: repeated identical
        // prefixes report 0 cache_creation and 0 cache_read), and with one the
        // second turn reads the whole prefix back at the hit rate.
        payload["system"] = json!([{
            "type": "text",
            "text": system_prompt,
            "cache_control": {"type": "ephemeral"}
        }]);
    }

    if let Some(tools) = anthropic_tools(openai) {
        payload["tools"] = tools;
    }

    Ok(payload)
}

/// A message's text, whether it is a plain string or OpenAI content parts.
fn content_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// True when the message is a user turn made only of `tool_result` blocks, so
/// the next result can join it instead of opening a second user turn.
fn is_tool_results(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("user")
        && message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                !blocks.is_empty()
                    && blocks.iter().all(|block| {
                        block.get("type").and_then(Value::as_str) == Some("tool_result")
                    })
            })
}

/// The OpenAI `tools` list in the Messages shape, when there is one.
///
/// The tool block is part of the stable prefix, so its last entry carries the
/// cache breakpoint: the whole tool set is then cached alongside the system
/// prompt and read back at the hit rate on every later turn of the run.
fn anthropic_tools(openai: &Value) -> Option<Value> {
    let tools = openai.get("tools").and_then(Value::as_array)?;
    let mut mapped = tools
        .iter()
        .filter_map(|tool| {
            let function = tool.get("function")?;
            let name = function.get("name").and_then(Value::as_str)?;
            let mut entry = json!({
                "name": name,
                "input_schema": function
                    .get("parameters")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}))
            });
            if let Some(description) = function.get("description").and_then(Value::as_str) {
                entry["description"] = json!(description);
            }
            Some(entry)
        })
        .collect::<Vec<_>>();
    if let Some(last) = mapped.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    (!mapped.is_empty()).then(|| json!(mapped))
}

/// Convert an Anthropic Messages API response into an OpenAI-compatible `/chat/completions` reply.
///
/// `tool_use` blocks become `tool_calls` (arguments as the JSON *string* OpenAI
/// carries) and `stop_reason: tool_use` becomes `finish_reason: tool_calls`, so
/// the worker's loop sees the same shape it gets from any OpenAI-compatible
/// provider. A response that only calls tools has a null `content`, which is
/// the OpenAI convention.
pub fn anthropic_to_openai_chat(anthropic: &Value, model: &str) -> Result<Value, String> {
    error_envelope(anthropic)?;

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in anthropic
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                    text.push_str(chunk);
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let id = block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("call_unknown");
                let arguments = block
                    .get("input")
                    .map_or_else(|| "{}".to_string(), Value::to_string);
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments}
                }));
            }
            // `thinking` and unknown blocks carry no answer of their own.
            _ => {}
        }
    }
    let truncated = anthropic.get("stop_reason").and_then(Value::as_str) == Some("max_tokens");
    if text.is_empty() && tool_calls.is_empty() && !truncated {
        return Err("response carried no content[].text or content[].tool_use".to_string());
    }

    let input_tokens = anthropic
        .pointer("/usage/input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read_tokens = anthropic
        .pointer("/usage/cache_read_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_written_tokens = anthropic
        .pointer("/usage/cache_creation_input_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let prompt_tokens = input_tokens + cache_read_tokens + cache_written_tokens;
    let output_tokens = anthropic
        .pointer("/usage/output_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);

    let id = anthropic
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("anthropic-cmpl");

    let finish_reason = match anthropic.get("stop_reason").and_then(Value::as_str) {
        Some("tool_use") => "tool_calls",
        Some("max_tokens") => "length",
        _ => "stop",
    };
    let content = if text.is_empty() {
        Value::Null
    } else {
        json!(text)
    };
    let mut message = json!({"role": "assistant", "content": content});
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    Ok(json!({
        "id": id,
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": message,
                "finish_reason": finish_reason
            }
        ],
        // Prompt caching, in the OpenAI shape the meter reads. Anthropic
        // reports `input_tokens` *excluding* everything served from or written
        // to its cache, so the OpenAI `prompt_tokens` (which includes the whole
        // prompt) is the sum, and `prompt_tokens_details.cached_tokens` carries
        // the read half — that is what lets the ledger charge the hit rate
        // instead of the miss rate.
        "usage": {
            "prompt_tokens": prompt_tokens,
            "prompt_tokens_details": {"cached_tokens": cache_read_tokens},
            "completion_tokens": output_tokens,
            "total_tokens": prompt_tokens + output_tokens
        }
    }))
}

/// Token count usage reported by an inference provider call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Construct a new TokenUsage with automatic sum.
    #[must_use]
    pub const fn new(prompt_tokens: u64, completion_tokens: u64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            cached_tokens: 0,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }

    /// Construct a new TokenUsage with explicit cached prompt tokens.
    #[must_use]
    pub const fn with_cache(
        prompt_tokens: u64,
        completion_tokens: u64,
        cached_tokens: u64,
    ) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            cached_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }
}

/// Extract reported token usage from an API response body according to protocol.
#[must_use]
pub fn extract_token_usage(protocol: InferenceProtocol, json: &Value) -> TokenUsage {
    match protocol {
        InferenceProtocol::OpenAiChat | InferenceProtocol::OpenAiCompletions => {
            let usage = &json["usage"];
            let prompt = usage["prompt_tokens"].as_u64().unwrap_or(0);
            let completion = usage["completion_tokens"].as_u64().unwrap_or(0);
            let cached = usage["prompt_cache_hit_tokens"]
                .as_u64()
                .or_else(|| usage["prompt_tokens_details"]["cached_tokens"].as_u64())
                .unwrap_or(0);
            let total = usage["total_tokens"]
                .as_u64()
                .unwrap_or_else(|| prompt.saturating_add(completion));
            TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                cached_tokens: cached,
                total_tokens: total,
            }
        }
        InferenceProtocol::Anthropic => {
            let usage = &json["usage"];
            let cache_read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
            let cache_write = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            let input = usage["input_tokens"].as_u64().unwrap_or(0);
            let prompt = input.saturating_add(cache_read).saturating_add(cache_write);
            let completion = usage["output_tokens"].as_u64().unwrap_or(0);
            TokenUsage::with_cache(prompt, completion, cache_read)
        }
        InferenceProtocol::Gemini => {
            let usage = &json["usageMetadata"];
            let prompt = usage["promptTokenCount"].as_u64().unwrap_or(0);
            let completion = usage["candidatesTokenCount"].as_u64().unwrap_or(0);
            let cached = usage["cachedContentTokenCount"].as_u64().unwrap_or(0);
            let total = usage["totalTokenCount"]
                .as_u64()
                .unwrap_or_else(|| prompt.saturating_add(completion));
            TokenUsage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                cached_tokens: cached,
                total_tokens: total,
            }
        }
        InferenceProtocol::Triton => TokenUsage::default(),
    }
}

/// Calculate estimated cost in millionths of a USD currency unit (micros), factoring in cache discounts.
#[must_use]
pub fn estimate_cost_micros(model: &str, usage: &TokenUsage) -> u64 {
    let model_lower = model.to_ascii_lowercase();
    let (in_rate, out_rate, cache_mult): (f64, f64, f64) = if model_lower.contains("haiku") {
        (0.25, 1.25, 0.10)
    } else if model_lower.contains("sonnet") {
        (3.0, 15.0, 0.10)
    } else if model_lower.contains("opus") {
        (15.0, 75.0, 0.10)
    } else if model_lower.contains("gpt-4o-mini") {
        (0.15, 0.60, 0.50)
    } else if model_lower.contains("gpt-4o") {
        (2.50, 10.0, 0.50)
    } else if model_lower.contains("flash") {
        (0.075, 0.30, 0.25)
    } else if model_lower.contains("pro") {
        (1.25, 5.0, 0.25)
    } else if model_lower.contains("deepseek") {
        (0.14, 0.28, 0.10)
    } else {
        (1.0, 3.0, 0.50)
    };

    let uncached_prompt = usage.prompt_tokens.saturating_sub(usage.cached_tokens);
    let uncached_micros = (uncached_prompt as f64 * in_rate) as u64;
    let cached_micros = (usage.cached_tokens as f64 * in_rate * cache_mult) as u64;
    let completion_micros = (usage.completion_tokens as f64 * out_rate) as u64;
    uncached_micros
        .saturating_add(cached_micros)
        .saturating_add(completion_micros)
}

/// Build an Anthropic Messages request payload with Full Cache Mode enabled.
///
/// Anchors ephemeral cache breakpoints on the stable system prompt and tools list.
#[must_use]
pub fn anthropic_cache_payload(
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    tools: Option<Value>,
    max_tokens: u64,
) -> Value {
    let mut payload = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            {
                "role": "user",
                "content": user_prompt
            }
        ]
    });

    if !system_prompt.is_empty() {
        payload["system"] = json!([
            {
                "type": "text",
                "text": system_prompt,
                "cache_control": {"type": "ephemeral"}
            }
        ]);
    }

    if let Some(mut tool_array) = tools {
        if let Some(arr) = tool_array.as_array_mut() {
            if let Some(last) = arr.last_mut() {
                last["cache_control"] = json!({"type": "ephemeral"});
            }
        }
        payload["tools"] = tool_array;
    }

    payload
}

/// Build a Gemini generateContent request payload with Full Cache Mode enabled.
///
/// Formats structured systemInstruction and contents, and supports explicit cachedContent.
#[must_use]
pub fn gemini_cache_payload(
    system_instruction: &str,
    user_prompt: &str,
    cached_content: Option<&str>,
) -> Value {
    let mut payload = json!({
        "contents": [
            {
                "role": "user",
                "parts": [{"text": user_prompt}]
            }
        ]
    });

    if !system_instruction.is_empty() {
        payload["systemInstruction"] = json!({
            "parts": [{"text": system_instruction}]
        });
    }

    if let Some(cache_name) = cached_content {
        payload["cachedContent"] = json!(cache_name);
    }

    payload
}

/// Build a DeepSeek OpenAI-compatible chat payload structured for prefix caching.
///
/// Ensures strict prefix stability (system message followed by user prompt)
/// so repeated worker turns hit DeepSeek's server-side prompt cache.
#[must_use]
pub fn deepseek_cache_payload(
    model: &str,
    system_prompt: &str,
    user_prompt: &str,
    max_tokens: u64,
) -> Value {
    let mut messages = Vec::with_capacity(2);
    if !system_prompt.is_empty() {
        messages.push(json!({
            "role": "system",
            "content": system_prompt
        }));
    }
    messages.push(json!({
        "role": "user",
        "content": user_prompt
    }));

    json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens
    })
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

    /// The 5.x Messages models reject `temperature`, `top_p` and `top_k`
    /// outright, so a translator that forwarded them would break every call.
    #[test]
    fn sampling_parameters_are_not_forwarded_to_anthropic() {
        let openai_req = json!({
            "model": "claude-sonnet-5-5",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 16,
            "temperature": 0.0,
            "top_p": 0.9,
            "top_k": 40
        });
        let anthropic_req = openai_to_anthropic_messages(&openai_req).unwrap();
        for dropped in ["temperature", "top_p", "top_k"] {
            assert!(
                anthropic_req.get(dropped).is_none(),
                "{dropped} must not reach the Messages API: {anthropic_req}"
            );
        }
        assert_eq!(anthropic_req["model"], "claude-sonnet-5-5");
        assert_eq!(anthropic_req["max_tokens"], 16);
    }

    /// A worker turn is a tool loop: tools must survive into the Messages
    /// shape, and a tool call must come back in the OpenAI shape the loop
    /// consumes, or a Claude turn can talk but never act.
    #[test]
    fn tool_declarations_and_calls_cross_the_anthropic_wire() {
        let openai_req = json!({
            "model": "claude-haiku",
            "messages": [
                {"role": "system", "content": "worker"},
                {"role": "user", "content": "write the file"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "write_to_file",
                                  "arguments": "{\"path\":\"a.txt\",\"content\":\"OK\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "wrote a.txt"}
            ],
            "tools": [
                {"type": "function", "function": {
                    "name": "write_to_file",
                    "description": "write a file",
                    "parameters": {"type": "object",
                                   "properties": {"path": {"type": "string"}},
                                   "required": ["path"]}
                }}
            ],
            "max_tokens": 64,
            "temperature": 0.0
        });
        let anthropic_req = openai_to_anthropic_messages(&openai_req).unwrap();

        assert_eq!(anthropic_req["tools"][0]["name"], "write_to_file");
        assert_eq!(anthropic_req["tools"][0]["input_schema"]["type"], "object");
        assert!(
            anthropic_req["tools"][0].get("function").is_none(),
            "the Messages tools shape has no `function` wrapper"
        );

        let calls = &anthropic_req["messages"][1];
        assert_eq!(calls["role"], "assistant");
        assert_eq!(calls["content"][0]["type"], "tool_use");
        assert_eq!(calls["content"][0]["id"], "call_1");
        assert_eq!(calls["content"][0]["name"], "write_to_file");
        assert_eq!(
            calls["content"][0]["input"],
            json!({"path": "a.txt", "content": "OK"}),
            "arguments must arrive parsed, not as a JSON string"
        );

        let result = &anthropic_req["messages"][2];
        assert_eq!(result["role"], "user");
        assert_eq!(result["content"][0]["type"], "tool_result");
        assert_eq!(result["content"][0]["tool_use_id"], "call_1");
        assert_eq!(result["content"][0]["content"], "wrote a.txt");

        // The answer: a tool_use block comes back as tool_calls, with a null
        // content and the OpenAI finish reason.
        let reply = json!({
            "id": "msg_1",
            "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "tool_use", "id": "call_2", "name": "run_command",
                 "input": {"command": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let openai_resp = anthropic_to_openai_chat(&reply, "claude-haiku").unwrap();
        let message = &openai_resp["choices"][0]["message"];
        assert!(message["content"].is_null(), "{openai_resp}");
        assert_eq!(message["tool_calls"][0]["id"], "call_2");
        assert_eq!(message["tool_calls"][0]["function"]["name"], "run_command");
        assert_eq!(
            message["tool_calls"][0]["function"]["arguments"], "{\"command\":\"ls\"}",
            "arguments travel as a JSON string, as OpenAI carries them"
        );
        assert_eq!(openai_resp["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(openai_resp["usage"]["prompt_tokens"], 10);
    }

    /// Parallel tool calls: every result belongs to the one user turn that
    /// answers the assistant's batch, so they are grouped, not split.
    #[test]
    fn parallel_tool_results_share_one_user_turn() {
        let openai_req = json!({
            "model": "claude-haiku",
            "messages": [
                {"role": "user", "content": "go"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"id": "a", "type": "function", "function": {"name": "one", "arguments": "{}"}},
                    {"id": "b", "type": "function", "function": {"name": "two", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "a", "content": "r1"},
                {"role": "tool", "tool_call_id": "b", "content": "r2"}
            ]
        });
        let anthropic_req = openai_to_anthropic_messages(&openai_req).unwrap();
        let messages = anthropic_req["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3, "{anthropic_req}");
        assert_eq!(messages[2]["content"].as_array().unwrap().len(), 2);
    }

    /// A truncated answer (`max_tokens`) with no text is still an answer: it
    /// carries usage, and the worker reports the length stop rather than the
    /// gateway failing the call as unmeterable.
    #[test]
    fn a_truncated_anthropic_answer_still_carries_its_usage() {
        let reply = json!({
            "id": "msg_3",
            "content": [],
            "stop_reason": "max_tokens",
            "usage": {
                "input_tokens": 5,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 7000,
                "output_tokens": 8
            }
        });
        let openai_resp = anthropic_to_openai_chat(&reply, "claude-haiku").unwrap();
        assert_eq!(openai_resp["choices"][0]["finish_reason"], "length");
        assert_eq!(openai_resp["usage"]["prompt_tokens"], 7005);
        assert_eq!(
            openai_resp["usage"]["prompt_tokens_details"]["cached_tokens"],
            7000
        );

        // An empty complete answer is still an anomaly.
        let empty = json!({"content": [], "stop_reason": "end_turn", "usage": {}});
        assert!(anthropic_to_openai_chat(&empty, "claude-haiku").is_err());
    }

    /// The meter charges the hit rate from `prompt_tokens_details.cached_tokens`,
    /// and Anthropic reports cache reads outside `input_tokens` — so both halves
    /// have to be put back together or the cache would be invisible (and priced
    /// as a miss).
    #[test]
    fn anthropic_cache_usage_maps_into_the_openai_shape() {
        let reply = json!({
            "id": "msg_2",
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 12,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 2763,
                "output_tokens": 5
            }
        });
        let openai_resp = anthropic_to_openai_chat(&reply, "claude-haiku").unwrap();
        let usage = &openai_resp["usage"];
        assert_eq!(usage["prompt_tokens"], 12 + 2763);
        assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], 2763);
        assert_eq!(usage["completion_tokens"], 5);
        assert_eq!(usage["total_tokens"], 12 + 2763 + 5);
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
        assert_eq!(
            anthropic_req["system"][0]["text"],
            "You are a test assistant."
        );
        assert_eq!(
            anthropic_req["system"][0]["cache_control"]["type"], "ephemeral",
            "the stable prefix carries the cache breakpoint"
        );
        assert_eq!(anthropic_req["max_tokens"], 1024);
        assert_eq!(anthropic_req["messages"][0]["role"], "user");
        assert_eq!(anthropic_req["messages"][0]["content"], "Hello Claude");
        assert!(
            anthropic_req.get("temperature").is_none(),
            "the Messages API deprecates sampling parameters; forwarding them fails every call: {anthropic_req}"
        );

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

    #[test]
    fn token_usage_extracts_and_estimates_cost() {
        let openai = json!({
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 500,
                "total_tokens": 1500
            }
        });
        let usage = extract_token_usage(InferenceProtocol::OpenAiChat, &openai);
        assert_eq!(usage.prompt_tokens, 1000);
        assert_eq!(usage.completion_tokens, 500);
        assert_eq!(usage.total_tokens, 1500);

        let cost = estimate_cost_micros("gpt-4o", &usage);
        assert!(cost > 0);

        let anthropic = json!({
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 200,
                "cache_read_input_tokens": 500
            }
        });
        let a_usage = extract_token_usage(InferenceProtocol::Anthropic, &anthropic);
        assert_eq!(a_usage.prompt_tokens, 1500);
        assert_eq!(a_usage.completion_tokens, 200);

        let a_cost = estimate_cost_micros("claude-3-5-sonnet", &a_usage);
        assert!(a_cost > 0);

        let gemini = json!({
            "usageMetadata": {
                "promptTokenCount": 2000,
                "candidatesTokenCount": 300,
                "totalTokenCount": 2300
            }
        });
        let g_usage = extract_token_usage(InferenceProtocol::Gemini, &gemini);
        assert_eq!(g_usage.prompt_tokens, 2000);
        assert_eq!(g_usage.completion_tokens, 300);
        assert_eq!(g_usage.total_tokens, 2300);
    }

    #[test]
    fn full_cache_mode_wire_payloads_and_extractions() {
        // 1. Claude (Anthropic) Full Cache Mode
        let tools = json!([
            {
                "name": "lookup",
                "description": "lookup symbol"
            }
        ]);
        let anthropic_req = anthropic_cache_payload(
            "claude-3-5-sonnet",
            "System prompt for worker",
            "User task instruction",
            Some(tools),
            1024,
        );
        assert_eq!(anthropic_req["model"], "claude-3-5-sonnet");
        assert_eq!(
            anthropic_req["system"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(
            anthropic_req["tools"][0]["cache_control"]["type"],
            "ephemeral"
        );

        let anthropic_resp = json!({
            "usage": {
                "input_tokens": 100,
                "cache_creation_input_tokens": 200,
                "cache_read_input_tokens": 700,
                "output_tokens": 50
            }
        });
        let claude_usage = extract_token_usage(InferenceProtocol::Anthropic, &anthropic_resp);
        assert_eq!(claude_usage.prompt_tokens, 1000);
        assert_eq!(claude_usage.cached_tokens, 700);
        assert_eq!(claude_usage.completion_tokens, 50);

        let claude_full_cost =
            estimate_cost_micros("claude-3-5-sonnet", &TokenUsage::new(1000, 50));
        let claude_cache_cost = estimate_cost_micros("claude-3-5-sonnet", &claude_usage);
        assert!(
            claude_cache_cost < claude_full_cost,
            "Cache hit must discount cost"
        );

        // 2. DeepSeek Full Cache Mode
        let deepseek_req = deepseek_cache_payload(
            "deepseek-chat",
            "System instructions with stable prefix",
            "Solve subtask",
            2048,
        );
        assert_eq!(deepseek_req["messages"][0]["role"], "system");
        assert_eq!(deepseek_req["messages"][1]["role"], "user");

        let deepseek_resp = json!({
            "usage": {
                "prompt_tokens": 2000,
                "completion_tokens": 100,
                "prompt_cache_hit_tokens": 1800,
                "prompt_cache_miss_tokens": 200,
                "total_tokens": 2100
            }
        });
        let ds_usage = extract_token_usage(InferenceProtocol::OpenAiChat, &deepseek_resp);
        assert_eq!(ds_usage.prompt_tokens, 2000);
        assert_eq!(ds_usage.cached_tokens, 1800);
        assert_eq!(ds_usage.completion_tokens, 100);

        let ds_full_cost = estimate_cost_micros("deepseek-chat", &TokenUsage::new(2000, 100));
        let ds_cache_cost = estimate_cost_micros("deepseek-chat", &ds_usage);
        assert!(
            ds_cache_cost < ds_full_cost,
            "DeepSeek cache hit must provide ~90% prompt savings"
        );

        // 3. Gemini Full Cache Mode
        let gemini_req = gemini_cache_payload(
            "System instruction for Gemini worker",
            "Execute worker step",
            Some("cachedContents/proj1-cache-123"),
        );
        assert_eq!(
            gemini_req["systemInstruction"]["parts"][0]["text"],
            "System instruction for Gemini worker"
        );
        assert_eq!(
            gemini_req["cachedContent"],
            "cachedContents/proj1-cache-123"
        );

        let gemini_resp = json!({
            "usageMetadata": {
                "promptTokenCount": 5000,
                "candidatesTokenCount": 250,
                "cachedContentTokenCount": 4000,
                "totalTokenCount": 5250
            }
        });
        let g_usage = extract_token_usage(InferenceProtocol::Gemini, &gemini_resp);
        assert_eq!(g_usage.prompt_tokens, 5000);
        assert_eq!(g_usage.cached_tokens, 4000);
        assert_eq!(g_usage.completion_tokens, 250);

        let g_full_cost = estimate_cost_micros("gemini-2.5-flash", &TokenUsage::new(5000, 250));
        let g_cache_cost = estimate_cost_micros("gemini-2.5-flash", &g_usage);
        assert!(
            g_cache_cost < g_full_cost,
            "Gemini cache hit must provide 75% prompt savings"
        );
    }

    #[test]
    fn moonshot_kimi_thinking_and_retry_test() {
        let body = openai_chat_body("kimi-k2.7-code-highspeed", "solve this", 1000);
        assert_eq!(body["thinking"]["type"], "enabled");

        let err_resp = r#"{"error":{"message":"MoonshotException - invalid thinking: only type=enabled is allowed for this model","type":"invalid_request_error","code":"invalid_thinking"}}"#;
        let retry_body = token_param_retry(
            &json!({"model": "kimi-k2-7-code", "messages": []}),
            err_resp,
        )
        .unwrap();
        assert_eq!(retry_body["thinking"]["type"], "enabled");
    }
}
