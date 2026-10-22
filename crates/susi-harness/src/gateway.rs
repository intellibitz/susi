//! The metering gateway.
//!
//! Workers point their OpenAI-compatible base URL here. The gateway forwards a
//! chat completion, reads the provider's own `usage` (cache hits included),
//! prices it from the catalogue, and appends a ledger row — so a call that
//! reached a provider is never spend without a record.
//!
//! Both shapes are metered:
//!
//! - **Buffered** (`stream: false`) — the answer is read in full, priced, then
//!   returned.
//! - **Streamed** (`stream: true`) — SSE chunks are forwarded as they arrive;
//!   the gateway forces `stream_options.include_usage` upstream and prices the
//!   final usage chunk before the stream closes. A stream that ends without a
//!   usage chunk is logged loudly and writes no row: it is uncountable, and
//!   inventing a number would be worse than saying so.

use crate::cache::ResponseCache;
use crate::catalog::PriceCatalogue;
use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
use crate::pricing::ModelPrice;
use crate::usage::token_usage_from_json;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// How much of an upstream response the gateway will buffer, in bytes.
pub const MAX_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

/// Everything the gateway needs to price and forward one call.
#[derive(Clone)]
pub struct GatewayState {
    /// Prices in force for this gateway.
    pub catalogue: PriceCatalogue,
    /// Append-only ledger the gateway writes.
    pub ledger_path: PathBuf,
    /// The full upstream chat-completions URL.
    pub upstream_chat_url: String,
    /// Provider id recorded on each row (`deepseek`, `gemini`, …).
    pub provider: String,
    /// Upstream credential, sent as `Authorization: Bearer …`.
    pub api_key: String,
    /// Upstream call timeout, in seconds.
    pub timeout_secs: u64,
    /// Upper bound placed on `max_tokens` in every forwarded request.
    ///
    /// A worker turn that asks for tens of thousands of output tokens can run
    /// for half an hour and then die on the transport deadline; capping it
    /// makes the turn iterate instead of outrunning the clock.
    pub max_output_tokens: Option<u32>,
    /// Optional in-memory and persistent SHA-256 request/response cache.
    pub cache: Option<Arc<ResponseCache>>,
}

/// One dispatch the gateway is about to price.
struct Attempt {
    mission: String,
    task: String,
    attempt: u32,
    model: String,
    price: ModelPrice,
}

/// Build the gateway router.
pub fn router(state: Arc<GatewayState>) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(completions))
        .with_state(state)
}

/// Serve the gateway on an already-bound listener.
///
/// # Errors
///
/// Returns the listener's I/O error when the server stops.
pub async fn serve(
    listener: tokio::net::TcpListener,
    state: Arc<GatewayState>,
) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

async fn health() -> &'static str {
    "ok"
}

async fn completions(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Ok(mut request) = serde_json::from_str::<Value>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "request body is not JSON");
    };
    if let Some(cap) = state.max_output_tokens {
        clamp_output_tokens(&mut request, cap);
    }
    let body = serde_json::to_string(&request).unwrap_or(body);
    let streaming = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(model) = request.get("model").and_then(Value::as_str) else {
        return error_response(StatusCode::BAD_REQUEST, "request has no model");
    };
    let Some(price) = state.catalogue.price(model) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            &format!("model `{model}` has no catalogue price"),
        );
    };

    let tagged = run_tag(&headers);
    let mission = header_tag(&headers, "x-susi-mission")
        .or_else(|| tagged.as_ref().map(|(mission, _, _)| mission.clone()))
        .unwrap_or_else(|| "unscoped".into());
    let task = header_tag(&headers, "x-susi-task")
        .or_else(|| tagged.as_ref().map(|(_, task, _)| task.clone()))
        .unwrap_or_else(|| "unscoped".into());
    let attempt = header_tag(&headers, "x-susi-attempt")
        .and_then(|value| value.parse::<u32>().ok())
        .or_else(|| tagged.map(|(_, _, attempt)| attempt))
        .unwrap_or(1);

    let forwarded = if streaming {
        with_usage_in_stream(&request, &body)
    } else {
        body
    };

    let attempt = Attempt {
        mission,
        task,
        attempt,
        model: model.to_string(),
        price,
    };

    let cache_key = ResponseCache::compute_key(&request);
    if !streaming {
        if let Some(ref cache) = state.cache {
            if let Some(cached_response) = cache.get(&cache_key) {
                let usage = token_usage_from_json(&cached_response).unwrap_or_default();
                let entry = LedgerEntry::settle(Dispatch {
                    mission: attempt.mission.clone(),
                    task: attempt.task.clone(),
                    attempt: attempt.attempt,
                    model: attempt.model.clone(),
                    provider: state.provider.clone(),
                    usage,
                    price: attempt.price,
                    started_unix_ms: now_ms(),
                    ended_unix_ms: now_ms(),
                    outcome: Outcome::Completed,
                })
                .as_cached();
                let _ = append_entry(&state.ledger_path, &entry);

                let body_bytes = serde_json::to_vec(&cached_response).unwrap_or_default();
                let mut res = Response::new(axum::body::Body::from(body_bytes));
                *res.status_mut() = StatusCode::OK;
                res.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/json"),
                );
                res.headers_mut().insert(
                    header::HeaderName::from_static("x-susi-cache-key"),
                    HeaderValue::from_str(&cache_key).unwrap_or(HeaderValue::from_static("hit")),
                );
                return res;
            }
        }
    }

    if streaming {
        stream_upstream(state, attempt, forwarded).await
    } else {
        buffered_upstream(state, attempt, forwarded, cache_key).await
    }
}

fn with_usage_in_stream(request: &Value, original: &str) -> String {
    let mut request = request.clone();
    if let Value::Object(map) = &mut request {
        map.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    serde_json::to_string(&request).unwrap_or_else(|_| original.to_string())
}

async fn buffered_upstream(
    state: Arc<GatewayState>,
    attempt: Attempt,
    body: String,
    cache_key: String,
) -> Response {
    let Attempt {
        mission,
        task,
        attempt,
        model,
        price,
    } = attempt;
    let started_unix_ms = now_ms();
    let timeout_secs = state.timeout_secs;
    let route = match crate::router::resolve_route(
        &model,
        Some(&state.upstream_chat_url),
        Some(&state.api_key),
    ) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("resolve route: {e}")),
    };

    let is_anthropic = route.is_anthropic;
    let model_name = model.clone();

    let outgoing_body = if is_anthropic {
        let Ok(mut parsed) = serde_json::from_str::<Value>(&body) else {
            return error_response(StatusCode::BAD_REQUEST, "invalid json request body");
        };
        if let Some(obj) = parsed.as_object_mut() {
            obj.insert("model".to_string(), Value::String(route.model_id.clone()));
        }
        match susi_adapters_llm::inference_wire::openai_to_anthropic_messages(&parsed) {
            Ok(v) => serde_json::to_vec(&v).unwrap_or_else(|_| body.into_bytes()),
            Err(e) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    &format!("anthropic translation: {e}"),
                )
            }
        }
    } else if route.model_id != model {
        if let Ok(mut parsed) = serde_json::from_str::<Value>(&body) {
            if let Some(obj) = parsed.as_object_mut() {
                obj.insert("model".to_string(), Value::String(route.model_id.clone()));
            }
            serde_json::to_vec(&parsed).unwrap_or_else(|_| body.into_bytes())
        } else {
            body.into_bytes()
        }
    } else {
        body.into_bytes()
    };

    let target_url = route.url.clone();
    let route_headers = route.headers.clone();

    let upstream = tokio::task::spawn_blocking(move || {
        let headers_ref: Vec<(&str, &str)> = route_headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        susi_http_transport::http_call_with_body(
            "POST",
            &target_url,
            &headers_ref,
            Some(&outgoing_body),
            timeout_secs,
            0,
        )
    })
    .await;

    let call = match upstream {
        Ok(Ok(call)) => call,
        Ok(Err(error)) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream call failed: {error}"),
            );
        }
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream task failed: {error}"),
            );
        }
    };
    let status = call.status;
    // LiteLLM sets this header only when it served the answer from its cache;
    // the provider was not called, so the row must not charge for it.
    let cached =
        call.header("x-litellm-cache-key").is_some() || call.header("x-susi-cache-key").is_some();
    let content_type = call
        .header("content-type")
        .unwrap_or("application/json")
        .to_string();
    let response_bytes = match call.into_bytes(MAX_RESPONSE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) => {
            return error_response(StatusCode::BAD_GATEWAY, &format!("upstream body: {error}"));
        }
    };
    let ended_unix_ms = now_ms();

    let response_bytes = if (200..300).contains(&status) && is_anthropic {
        if let Ok(anth_val) = serde_json::from_slice::<Value>(&response_bytes) {
            match susi_adapters_llm::inference_wire::anthropic_to_openai_chat(
                &anth_val,
                &model_name,
            ) {
                Ok(openai_val) => serde_json::to_vec(&openai_val).unwrap_or(response_bytes),
                Err(_) => response_bytes,
            }
        } else {
            response_bytes
        }
    } else {
        response_bytes
    };

    if (200..300).contains(&status) {
        let Ok(value) = serde_json::from_slice::<Value>(&response_bytes) else {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "upstream answer is not JSON, so it cannot be priced",
            );
        };
        let usage = match token_usage_from_json(&value) {
            Ok(usage) => usage,
            Err(error) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("unmetered provider answer: {error}"),
                );
            }
        };
        let entry = LedgerEntry::settle(Dispatch {
            mission,
            task,
            attempt,
            model,
            provider: state.provider.clone(),
            usage,
            price,
            started_unix_ms,
            ended_unix_ms,
            outcome: Outcome::Completed,
        });
        if let Some(ref cache) = state.cache {
            cache.set(&cache_key, &value);
        }
        let entry = if cached { entry.as_cached() } else { entry };
        if let Err(error) = append_entry(&state.ledger_path, &entry) {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("ledger append failed: {error}"),
            );
        }
    }

    let mut response = Response::new(axum::body::Body::from(response_bytes));
    *response.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
}

async fn stream_upstream(state: Arc<GatewayState>, attempt: Attempt, body: String) -> Response {
    let Attempt {
        mission,
        task,
        attempt,
        model,
        price,
    } = attempt;
    let started_unix_ms = now_ms();
    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<Result<(u16, String), String>>();
    let url = state.upstream_chat_url.clone();
    let api_key = state.api_key.clone();
    let timeout_secs = state.timeout_secs;
    let provider = state.provider.clone();
    let ledger_path = state.ledger_path.clone();

    tokio::task::spawn_blocking(move || {
        let authorization = format!("Bearer {api_key}");
        let forwarded = [
            ("authorization", authorization.as_str()),
            ("content-type", "application/json"),
        ];
        let call = match susi_http_transport::http_call_with_body(
            "POST",
            &url,
            &forwarded,
            Some(body.as_bytes()),
            timeout_secs,
            0,
        ) {
            Ok(call) => call,
            Err(error) => {
                let _ = meta_tx.send(Err(format!("upstream call failed: {error}")));
                return;
            }
        };
        let status = call.status;
        let content_type = call
            .header("content-type")
            .unwrap_or("text/event-stream")
            .to_string();
        if meta_tx.send(Ok((status, content_type))).is_err() {
            return;
        }
        if !(200..300).contains(&status) {
            if let Ok(bytes) = call.into_bytes(MAX_RESPONSE_BYTES) {
                let _ = chunk_tx.blocking_send(bytes);
            }
            return;
        }

        let mut reader = std::io::BufReader::new(call.into_reader());
        let mut usage = None;
        loop {
            let mut line = Vec::new();
            match std::io::BufRead::read_until(&mut reader, b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => break,
            }
            if let Some(data) = sse_data(&line) {
                if let Ok(value) = serde_json::from_str::<Value>(data) {
                    if let Ok(parsed) = token_usage_from_json(&value) {
                        usage = Some(parsed);
                    }
                }
            }
            if chunk_tx.blocking_send(line).is_err() {
                return;
            }
        }
        match usage {
            Some(usage) => {
                let entry = LedgerEntry::settle(Dispatch {
                    mission,
                    task,
                    attempt,
                    model,
                    provider,
                    usage,
                    price,
                    started_unix_ms,
                    ended_unix_ms: now_ms(),
                    outcome: Outcome::Completed,
                });
                if let Err(error) = append_entry(&ledger_path, &entry) {
                    eprintln!("harness-gateway: ledger append failed: {error}");
                }
            }
            None => eprintln!(
                "harness-gateway: stream for {model} ({mission}/{task} attempt {attempt}) ended without usage; not recorded"
            ),
        }
    });

    match meta_rx.await {
        Ok(Ok((status, content_type))) => {
            let stream = futures::stream::poll_fn(move |cx| chunk_rx.poll_recv(cx))
                .map(Ok::<Vec<u8>, std::io::Error>);
            let mut response = Response::new(axum::body::Body::from_stream(stream));
            *response.status_mut() =
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            if let Ok(value) = HeaderValue::from_str(&content_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            response
        }
        Ok(Err(message)) => error_response(StatusCode::BAD_GATEWAY, &message),
        Err(_) => error_response(StatusCode::BAD_GATEWAY, "upstream task failed"),
    }
}

fn sse_data(line: &[u8]) -> Option<&str> {
    let text = std::str::from_utf8(line).ok()?;
    let trimmed = text.trim_end_matches(['\r', '\n']);
    trimmed.strip_prefix("data:").map(str::trim_start)
}

fn error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": { "message": message } }))).into_response()
}

/// A per-run key: `Bearer susi:<mission>:<task>:<attempt>`.
///
/// OpenClaw cannot add custom headers, but its provider config carries an API
/// key — so the executor writes the run's identity there and the gateway reads
/// it back. The upstream never sees this key; the gateway substitutes its own.
fn run_tag(headers: &HeaderMap) -> Option<(String, String, u32)> {
    let authorization = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let key = authorization.strip_prefix("Bearer ")?.trim();
    let rest = key.strip_prefix("susi:")?;
    let mut parts = rest.split(':');
    let mission = parts.next()?.trim();
    let task = parts.next()?.trim();
    let attempt = parts.next().unwrap_or("1").parse().unwrap_or(1);
    if parts.next().is_some() || mission.is_empty() || task.is_empty() {
        return None;
    }
    Some((mission.to_string(), task.to_string(), attempt))
}

fn header_tag(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Cap the output tokens a request may ask for.
///
/// A turn that requests tens of thousands of output tokens can outrun the
/// transport deadline and die having written nothing; the cap makes it
/// iterate instead. A request that asks for fewer is left alone.
fn clamp_output_tokens(request: &mut Value, cap: u32) {
    let Some(object) = request.as_object_mut() else {
        return;
    };
    let cap = u64::from(cap);
    let mut found = false;
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(value) = object.get(key).and_then(Value::as_u64) {
            found = true;
            if value > cap {
                object.insert(key.to_string(), Value::from(cap));
            }
        }
    }
    if !found {
        object.insert("max_tokens".to_string(), Value::from(cap));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::read_entries;
    use std::net::SocketAddr;

    const CATALOGUE: &str = r#"{
        "schema": "susi/harness/prices/v1",
        "models": {
            "deepseek-flash": {"input_miss": 150000, "input_hit": 3000, "output": 600000},
            "claude-3-5-sonnet": {"input_miss": 3000000, "input_hit": 300000, "output": 15000000}
        }
    }"#;

    fn catalogue() -> PriceCatalogue {
        PriceCatalogue::from_json(CATALOGUE).unwrap()
    }

    async fn spawn(app: Router) -> SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        address
    }

    fn gateway_state(upstream: SocketAddr, ledger_path: PathBuf) -> Arc<GatewayState> {
        Arc::new(GatewayState {
            catalogue: catalogue(),
            ledger_path,
            upstream_chat_url: format!("http://{upstream}/v1/chat/completions"),
            provider: "deepseek".into(),
            api_key: "test-key".into(),
            timeout_secs: 10,
            max_output_tokens: None,
            cache: None,
        })
    }

    async fn post_json(address: SocketAddr, body: &str, tags: &[(&str, &str)]) -> (u16, String) {
        let url = format!("http://{address}/v1/chat/completions");
        let body = body.to_string();
        let tags: Vec<(String, String)> = tags
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        tokio::task::spawn_blocking(move || {
            let forwarded: Vec<(&str, &str)> = tags
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str()))
                .collect();
            let call = susi_http_transport::http_call_with_body(
                "POST",
                &url,
                &forwarded,
                Some(body.as_bytes()),
                10,
                0,
            )
            .unwrap();
            let status = call.status;
            let text = call.into_utf8(MAX_RESPONSE_BYTES).unwrap();
            (status, text)
        })
        .await
        .unwrap()
    }

    async fn complete() -> Json<Value> {
        Json(json!({
            "id": "cmpl-1",
            "object": "chat.completion",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "ok"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 3_000_000,
                "completion_tokens": 200_000,
                "prompt_cache_hit_tokens": 2_400_000
            }
        }))
    }

    async fn streamed() -> impl IntoResponse {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            concat!(
                "data: {\"id\":\"cmpl-3\",\"choices\":[{\"delta\":{\"content\":\"OK\"}}]}\n\n",
                "data: {\"id\":\"cmpl-3\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,",
                "\"completion_tokens\":2,\"prompt_cache_hit_tokens\":4}}\n\n",
                "data: [DONE]\n\n"
            ),
        )
    }

    async fn failing() -> (StatusCode, Json<Value>) {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"message": "boom"}})),
        )
    }

    async fn without_usage() -> Json<Value> {
        Json(json!({"id": "cmpl-2", "choices": []}))
    }

    #[tokio::test]
    async fn a_completion_is_priced_and_recorded() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, body) = post_json(
            gateway,
            r#"{"model":"deepseek-flash","messages":[]}"#,
            &[
                ("x-susi-mission", "m-1"),
                ("x-susi-task", "t-1"),
                ("x-susi-attempt", "2"),
            ],
        )
        .await;

        assert_eq!(status, 200);
        assert!(body.contains("cmpl-1"));
        let entries = read_entries(&ledger_path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mission, "m-1");
        assert_eq!(entries[0].task, "t-1");
        assert_eq!(entries[0].attempt, 2);
        assert_eq!(entries[0].model, "deepseek-flash");
        assert_eq!(entries[0].provider, "deepseek");
        assert_eq!(entries[0].cost.micros(), 217_200);
        assert_eq!(entries[0].outcome, Outcome::Completed);
    }

    #[tokio::test]
    async fn a_streamed_completion_is_forwarded_and_priced() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(streamed))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, body) = post_json(
            gateway,
            r#"{"model":"deepseek-flash","stream":true,"messages":[]}"#,
            &[("x-susi-mission", "m-2"), ("x-susi-task", "t-2")],
        )
        .await;

        assert_eq!(status, 200);
        assert!(body.contains("data: [DONE]"));
        let entries = read_entries(&ledger_path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mission, "m-2");
        assert_eq!(entries[0].task, "t-2");
        assert_eq!(entries[0].usage.input, 10);
        assert_eq!(entries[0].usage.cache_hit, 4);
        assert_eq!(entries[0].usage.output, 2);
        // 6 miss * $0.15 + 4 hit * $0.003 + 2 out * $0.60 per million, rounded up.
        assert_eq!(entries[0].cost.micros(), 3);
    }

    #[tokio::test]
    async fn a_run_key_attributes_the_row() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, _) = post_json(
            gateway,
            r#"{"model":"deepseek-flash","messages":[]}"#,
            &[("authorization", "Bearer susi:pilot-2:exec-1:3")],
        )
        .await;

        assert_eq!(status, 200);
        let entries = read_entries(&ledger_path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].mission, "pilot-2");
        assert_eq!(entries[0].task, "exec-1");
        assert_eq!(entries[0].attempt, 3);
    }

    #[tokio::test]
    async fn an_unpriced_model_is_refused_without_a_row() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, _) = post_json(gateway, r#"{"model":"mystery","messages":[]}"#, &[]).await;
        assert_eq!(status, 400);
        assert!(!ledger_path.exists());
    }

    #[tokio::test]
    async fn an_upstream_error_passes_through_unbilled() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(failing))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, _) =
            post_json(gateway, r#"{"model":"deepseek-flash","messages":[]}"#, &[]).await;
        assert_eq!(status, 500);
        assert!(!ledger_path.exists());
    }

    #[tokio::test]
    async fn an_answer_without_usage_is_loud() {
        let upstream =
            spawn(Router::new().route("/v1/chat/completions", post(without_usage))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, _) =
            post_json(gateway, r#"{"model":"deepseek-flash","messages":[]}"#, &[]).await;
        assert_eq!(status, 502);
        assert!(!ledger_path.exists());
    }

    #[test]
    fn a_requests_output_tokens_are_capped_not_raised() {
        let mut large = serde_json::json!({"model": "m", "max_tokens": 50_000});
        clamp_output_tokens(&mut large, 8_000);
        assert_eq!(large["max_tokens"], 8_000);

        let mut small = serde_json::json!({"model": "m", "max_tokens": 100});
        clamp_output_tokens(&mut small, 8_000);
        assert_eq!(small["max_tokens"], 100);

        let mut absent = serde_json::json!({"model": "m"});
        clamp_output_tokens(&mut absent, 8_000);
        assert_eq!(absent["max_tokens"], 8_000);

        let mut not_object = serde_json::json!([1]);
        clamp_output_tokens(&mut not_object, 8_000);
        assert_eq!(not_object, serde_json::json!([1]));
    }

    #[tokio::test]
    async fn a_cached_request_is_served_at_zero_cost() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = temp.path().join("ledger.jsonl");

        let app = Router::new().route(
            "/v1/chat/completions",
            post(|_: String| async {
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    json!({
                        "id": "1",
                        "choices": [{"message": {"role": "assistant", "content": "cached output"}}],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                    })
                    .to_string(),
                )
            }),
        );
        let upstream = spawn(app).await;

        let cache = Arc::new(ResponseCache::new(Some(temp.path().to_path_buf())));
        let state = Arc::new(GatewayState {
            catalogue: catalogue(),
            ledger_path: ledger.clone(),
            upstream_chat_url: format!("http://{upstream}/v1/chat/completions"),
            provider: "deepseek".into(),
            api_key: "test-key".into(),
            timeout_secs: 10,
            max_output_tokens: None,
            cache: Some(cache),
        });
        let gateway = spawn(router(state)).await;

        let body = json!({
            "model": "deepseek-flash",
            "messages": [{"role": "user", "content": "ping"}]
        })
        .to_string();

        // Call 1: Populates cache
        let (status, _) = post_json(
            gateway,
            &body,
            &[("x-susi-mission", "m1"), ("x-susi-task", "t1")],
        )
        .await;
        assert_eq!(status, 200);

        // Call 2: Hit cache
        let (status, resp2) = post_json(
            gateway,
            &body,
            &[("x-susi-mission", "m1"), ("x-susi-task", "t1")],
        )
        .await;
        assert_eq!(status, 200);
        let val: Value = serde_json::from_str(&resp2).unwrap();
        assert_eq!(val["choices"][0]["message"]["content"], "cached output");

        // Verify ledger recorded cache hit with 0 micro-USD
        let entries = read_entries(&ledger).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[1].cached);
        assert_eq!(entries[1].cost.micros(), 0);
    }
    #[tokio::test]
    async fn an_anthropic_model_is_routed_and_translated_natively() {
        let temp = tempfile::tempdir().unwrap();
        let ledger = temp.path().join("ledger.jsonl");

        // Mock Anthropic upstream server expecting Anthropic Messages format
        let app = Router::new().route(
            "/v1/messages",
            post(|body: String| async move {
                let req: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(req["model"], "claude-3-5-sonnet-20241022");
                assert!(req["messages"].is_array());
                (
                    StatusCode::OK,
                    [("content-type", "application/json")],
                    json!({
                        "id": "msg_anthropic_123",
                        "content": [{"type": "text", "text": "native anthropic reply"}],
                        "usage": {"input_tokens": 20, "output_tokens": 10}
                    })
                    .to_string(),
                )
            }),
        );
        let upstream = spawn(app).await;

        let state = Arc::new(GatewayState {
            catalogue: catalogue(),
            ledger_path: ledger.clone(),
            // Pass explicit mock upstream with custom port
            upstream_chat_url: format!("http://{upstream}/v1/messages"),
            provider: "anthropic".into(),
            api_key: "test-anthropic-key".into(),
            timeout_secs: 10,
            max_output_tokens: None,
            cache: None,
        });
        let gateway = spawn(router(state)).await;

        // Post standard OpenAI format to the gateway
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [
                {"role": "system", "content": "You are a test assistant."},
                {"role": "user", "content": "hello"}
            ]
        })
        .to_string();

        let (status, resp) = post_json(
            gateway,
            &body,
            &[("x-susi-mission", "m1"), ("x-susi-task", "t1")],
        )
        .await;
        assert_eq!(status, 200);
        let val: Value = serde_json::from_str(&resp).unwrap();
        // Gateway answered in standard OpenAI format
        assert_eq!(
            val["choices"][0]["message"]["content"],
            "native anthropic reply"
        );
        assert_eq!(val["usage"]["prompt_tokens"], 20);
        assert_eq!(val["usage"]["completion_tokens"], 10);

        // Verify ledger recorded the row
        let entries = read_entries(&ledger).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].usage.input, 20);
        assert_eq!(entries[0].usage.output, 10);
    }
}
