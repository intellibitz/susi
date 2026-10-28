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
use crate::ledger::{append_entry, Dispatch, FailureClass, LedgerEntry, Outcome};
use crate::pricing::{ModelPrice, TokenUsage};
use crate::usage::token_usage_from_json;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
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
    /// Upstream credential override.
    ///
    /// `None` means "let the registry resolve each model's own key env", which
    /// is what a direct-mode gateway serving several providers needs: pinning
    /// one provider's key here made every other provider's calls carry it
    /// (Anthropic answered `invalid x-api-key` for a route that should have used
    /// its own). `Some` is the single-upstream case — one URL, one key — and an
    /// operator's explicit `--api-key-env`.
    pub api_key: Option<String>,
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

static FALLBACK_CACHE: std::sync::OnceLock<Arc<ResponseCache>> = std::sync::OnceLock::new();

impl GatewayState {
    /// Active response cache, ensuring full cache mode is always active.
    #[must_use]
    pub fn active_cache(&self) -> Arc<ResponseCache> {
        if let Some(ref c) = self.cache {
            return Arc::clone(c);
        }
        Arc::clone(FALLBACK_CACHE.get_or_init(|| {
            let cache_dir = self.ledger_path.parent().map(std::path::Path::to_path_buf);
            Arc::new(ResponseCache::new(cache_dir))
        }))
    }
}

/// One dispatch the gateway is about to price.
#[derive(Clone)]
struct Attempt {
    mission: String,
    task: String,
    attempt: u32,
    model: String,
    price: ModelPrice,
    /// The provider the model resolves to, recorded on every row so the
    /// ledger's per-provider views (cooldown, spend) see the real provider
    /// even when one gateway routes to several.
    provider: String,
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
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
}

/// Resolves when the process is asked to stop (Ctrl-C, or `SIGTERM` on
/// Unix). The servers stop accepting then, and finish the calls in flight,
/// so every call that reached a provider still gets its ledger row.
pub async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    eprintln!("harness gateway: stopping; finishing the calls in flight");
}

async fn health() -> &'static str {
    "ok"
}

async fn completions(
    State(state): State<Arc<GatewayState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    complete(state, headers, body).await
}

/// Forward one completion through the meter.
///
/// The authenticated ingress calls this after its own admission checks; the
/// gateway's own route calls it directly.
pub async fn complete(state: Arc<GatewayState>, headers: HeaderMap, body: String) -> Response {
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
    // A turn that declares tools needs a model that can call them. The check
    // runs before the call is priced, so an incapable model costs nothing.
    let needs_tools = request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    if needs_tools {
        let denied = crate::providers::Registry::builtin()
            .require(model, &[crate::providers::Capability::Tools])
            .err();
        if let Some(error) = denied {
            return error_response(StatusCode::BAD_REQUEST, &error.to_string());
        }
    }
    // A model the catalogue cannot price still runs: a working model is
    // never refused for lack of costing. Its rows record the tokens at a
    // zero price, so the spend is visible even though it is not costed.
    let price = state.catalogue.price(model).unwrap_or_default();

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

    let provider = crate::providers::Registry::builtin()
        .resolve(model)
        .map_or_else(|| state.provider.clone(), |resolved| resolved.provider);
    let attempt = Attempt {
        mission,
        task,
        attempt,
        model: model.to_string(),
        price,
        provider,
    };

    let cache_key = ResponseCache::compute_key(&request);
    if let Some(ref cache) = state.cache {
        if let Some(cached_response) = cache.get(&cache_key) {
            let usage = token_usage_from_json(&cached_response).unwrap_or_default();
            let entry = LedgerEntry::settle(Dispatch {
                mission: attempt.mission.clone(),
                task: attempt.task.clone(),
                attempt: attempt.attempt,
                model: attempt.model.clone(),
                provider: attempt.provider.clone(),
                usage,
                price: attempt.price,
                started_unix_ms: now_ms(),
                ended_unix_ms: now_ms(),
                outcome: Outcome::Completed,
            })
            .as_cached();
            let _ = append_entry(&state.ledger_path, &entry);

            if streaming {
                return cached_streaming_response(&cache_key, &cached_response, &attempt.model);
            }

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

    let wait = crate::ratelimit::Pacer::global().reserve(&attempt.provider, now_ms(), None);
    if wait > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    }

    if streaming {
        stream_upstream(state, attempt, forwarded).await
    } else {
        buffered_upstream(state, attempt, forwarded, cache_key).await
    }
}

fn cached_streaming_response(cache_key: &str, cached_response: &Value, model: &str) -> Response {
    let content = cached_response
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c0| c0.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let chunk1 = json!({
        "id": format!("chatcmpl-cache-{cache_key}"),
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "content": content
            },
            "finish_reason": Value::Null
        }]
    });
    let chunk2 = json!({
        "id": format!("chatcmpl-cache-{cache_key}"),
        "object": "chat.completion.chunk",
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": "stop"
        }]
    });
    let sse_body = format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk1).unwrap_or_default(),
        serde_json::to_string(&chunk2).unwrap_or_default()
    );
    let mut res = Response::new(axum::body::Body::from(sse_body));
    *res.status_mut() = StatusCode::OK;
    res.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    res.headers_mut().insert(
        header::HeaderName::from_static("x-susi-cache-key"),
        HeaderValue::from_str(cache_key).unwrap_or(HeaderValue::from_static("hit")),
    );
    res
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
    let tag = attempt.clone();
    let Attempt {
        mission,
        task,
        attempt,
        model,
        price,
        ..
    } = attempt;
    let started_unix_ms = now_ms();
    let timeout_secs = state.timeout_secs;
    let route = match crate::router::resolve_route(
        &model,
        Some(&state.upstream_chat_url),
        state.api_key.as_deref(),
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
            let message = format!("upstream call failed: {error}");
            record_failure(
                &state.ledger_path,
                &tag,
                started_unix_ms,
                transport_failure(&message),
            );
            return error_response(StatusCode::BAD_GATEWAY, &message);
        }
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream task failed: {error}"),
            );
        }
    };
    let status = call.status;
    pause_on_retry_after(&tag.provider, status, call.header("retry-after"));
    // The cache marks the response when it served the answer; the provider was
    // not called, so the row must not charge for it.
    let cached = call.header("x-susi-cache-key").is_some();
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
    if let Some(class) = provider_failure(status, &response_bytes) {
        record_failure(&state.ledger_path, &tag, started_unix_ms, class);
    }

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
        crate::ratelimit::Pacer::global().record_tokens(
            &tag.provider,
            ended_unix_ms,
            usage.input.saturating_add(usage.output),
        );
        let entry = LedgerEntry::settle(Dispatch {
            mission,
            task,
            attempt,
            model,
            provider: tag.provider.clone(),
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
    let tag = attempt.clone();
    let Attempt {
        mission,
        task,
        attempt,
        model,
        price,
        ..
    } = attempt;
    let started_unix_ms = now_ms();
    let route = match crate::router::resolve_route(
        &model,
        Some(&state.upstream_chat_url),
        state.api_key.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("resolve route: {e}")),
    };

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<Result<(u16, String), String>>();
    let target_url = route.url.clone();
    let route_headers = route.headers.clone();
    let timeout_secs = state.timeout_secs;
    let provider = tag.provider.clone();
    let ledger_path = state.ledger_path.clone();

    let outgoing_body = if route.model_id != model {
        if let Ok(mut parsed) = serde_json::from_str::<Value>(&body) {
            if let Some(obj) = parsed.as_object_mut() {
                obj.insert("model".to_string(), Value::String(route.model_id.clone()));
            }
            serde_json::to_string(&parsed).unwrap_or(body)
        } else {
            body
        }
    } else {
        body
    };

    tokio::task::spawn_blocking(move || {
        let headers_ref: Vec<(&str, &str)> = route_headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let call = match susi_http_transport::http_call_with_body(
            "POST",
            &target_url,
            &headers_ref,
            Some(outgoing_body.as_bytes()),
            timeout_secs,
            0,
        ) {
            Ok(call) => call,
            Err(error) => {
                let message = format!("upstream call failed: {error}");
                record_failure(
                    &ledger_path,
                    &tag,
                    started_unix_ms,
                    transport_failure(&message),
                );
                let _ = meta_tx.send(Err(message));
                return;
            }
        };
        let status = call.status;
        pause_on_retry_after(&provider, status, call.header("retry-after"));
        let content_type = call
            .header("content-type")
            .unwrap_or("text/event-stream")
            .to_string();
        if meta_tx.send(Ok((status, content_type))).is_err() {
            return;
        }
        if !(200..300).contains(&status) {
            let bytes = call.into_bytes(MAX_RESPONSE_BYTES).unwrap_or_default();
            if let Some(class) = provider_failure(status, &bytes) {
                record_failure(&ledger_path, &tag, started_unix_ms, class);
            }
            let _ = chunk_tx.blocking_send(bytes);
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
                let ended_unix_ms = now_ms();
                crate::ratelimit::Pacer::global().record_tokens(
                    &provider,
                    ended_unix_ms,
                    usage.input.saturating_add(usage.output),
                );
                let entry = LedgerEntry::settle(Dispatch {
                    mission,
                    task,
                    attempt,
                    model,
                    provider,
                    usage,
                    price,
                    started_unix_ms,
                    ended_unix_ms,
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

/// The failure class of a provider's error answer, or `None` when the error
/// is about the request itself (a 4xx over its content), which says nothing
/// about whether the provider can serve.
///
/// Some providers report an empty account as a plain 400 (Anthropic's "credit
/// balance is too low"), so the body is read for that as well as the status.
pub(crate) fn provider_failure(status: u16, body: &[u8]) -> Option<FailureClass> {
    if (200..300).contains(&status) {
        return None;
    }
    let head = body.get(..body.len().min(4096)).unwrap_or(body);
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    const NO_CREDIT: &[&str] = &[
        "credit balance",
        "insufficient balance",
        "insufficient_quota",
        "payment required",
    ];
    if NO_CREDIT.iter().any(|needle| text.contains(needle)) {
        return Some(FailureClass::NoCredit);
    }
    match status {
        // The key cannot pay for calls: missing, revoked or out of credit.
        401..=403 => Some(FailureClass::NoCredit),
        429 => Some(FailureClass::RateLimited),
        408 | 504 => Some(FailureClass::Timeout),
        500..=599 => Some(FailureClass::Other),
        _ => None,
    }
}

/// Pause every call to `provider` for as long as a 429's `Retry-After` asks,
/// so the agents sharing this gateway stop hitting the limit together.
fn pause_on_retry_after(provider: &str, status: u16, retry_after: Option<&str>) {
    if status != 429 {
        return;
    }
    if let Some(wait) = retry_after.and_then(crate::ratelimit::retry_after_ms) {
        crate::ratelimit::Pacer::global().pause(provider, now_ms(), wait);
    }
}

/// The failure class of a call that never got an answer.
pub(crate) fn transport_failure(message: &str) -> FailureClass {
    let text = message.to_ascii_lowercase();
    if text.contains("timed out") || text.contains("timeout") {
        FailureClass::Timeout
    } else {
        FailureClass::Other
    }
}

/// Record a provider failure: no tokens and no cost, but the ranking learns
/// from it ([`crate::policy::history`], [`crate::policy::cooling_providers`]).
fn record_failure(
    ledger_path: &Path,
    attempt: &Attempt,
    started_unix_ms: u64,
    class: FailureClass,
) {
    let entry = LedgerEntry::settle(Dispatch {
        mission: attempt.mission.clone(),
        task: attempt.task.clone(),
        attempt: attempt.attempt,
        model: attempt.model.clone(),
        provider: attempt.provider.clone(),
        usage: TokenUsage::default(),
        price: attempt.price,
        started_unix_ms,
        ended_unix_ms: now_ms(),
        outcome: Outcome::ModelFailed { class },
    });
    if let Err(error) = append_entry(ledger_path, &entry) {
        eprintln!("harness-gateway: ledger append failed: {error}");
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
/// The worker sends this as its bearer token, so the gateway can attribute and
/// meter every call of a run. The upstream never sees this key; the gateway
/// substitutes its own provider key.
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
    use crate::money::MicroUsd;
    use std::net::SocketAddr;

    const CATALOGUE: &str = r#"{
        "schema": "susi/harness/prices/v1",
        "models": {
            "deepseek-flash": {"input_miss": 150000, "input_hit": 3000, "output": 600000},
            "claude-sonnet": {"input_miss": 3000000, "input_hit": 300000, "output": 15000000}
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
            // As `susi harness gateway` sets it: a name for the gateway, not a
            // provider. Rows must still carry the model's own provider.
            provider: "susi".into(),
            api_key: Some("test-key".into()),
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
    async fn an_unpriced_model_runs_and_its_tokens_are_recorded_uncosted() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, _) = post_json(gateway, r#"{"model":"mystery","messages":[]}"#, &[]).await;
        assert_eq!(status, 200);
        let rows = read_entries(&ledger_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model, "mystery");
        assert_eq!(rows[0].cost, MicroUsd::ZERO);
        assert!(rows[0].usage.input > 0);
    }

    #[tokio::test]
    async fn an_upstream_error_passes_through_unbilled_and_is_recorded_as_a_failure() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(failing))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, _) =
            post_json(gateway, r#"{"model":"deepseek-flash","messages":[]}"#, &[]).await;
        assert_eq!(status, 500);
        let rows = read_entries(&ledger_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].cost, MicroUsd::ZERO);
        assert_eq!(
            rows[0].outcome,
            Outcome::ModelFailed {
                class: FailureClass::Other
            }
        );
    }

    #[test]
    fn a_429_with_retry_after_pauses_its_provider_only() {
        // A provider name no other test uses: the pacer is process-wide.
        let limits = Some(crate::ratelimit::Limits::default());
        pause_on_retry_after("pause-test-provider", 429, Some("3"));
        let wait =
            crate::ratelimit::Pacer::global().reserve("pause-test-provider", now_ms(), limits);
        assert!((2_000..=3_000).contains(&wait), "waited {wait}ms");
        // Another status, or no header, pauses nothing.
        pause_on_retry_after("pause-test-other", 503, Some("30"));
        pause_on_retry_after("pause-test-other", 429, None);
        assert_eq!(
            crate::ratelimit::Pacer::global().reserve("pause-test-other", now_ms(), limits),
            0
        );
    }

    #[test]
    fn provider_errors_are_classified_and_request_errors_are_not() {
        assert_eq!(provider_failure(200, b""), None);
        assert_eq!(
            provider_failure(429, b"slow down"),
            Some(FailureClass::RateLimited)
        );
        assert_eq!(provider_failure(402, b""), Some(FailureClass::NoCredit));
        assert_eq!(
            provider_failure(401, b"bad key"),
            Some(FailureClass::NoCredit)
        );
        assert_eq!(
            provider_failure(
                400,
                br#"{"error":{"message":"Your credit balance is too low"}}"#
            ),
            Some(FailureClass::NoCredit)
        );
        assert_eq!(provider_failure(400, b"context length exceeded"), None);
        assert_eq!(
            provider_failure(503, b"overloaded"),
            Some(FailureClass::Other)
        );
        assert_eq!(provider_failure(504, b""), Some(FailureClass::Timeout));
        assert_eq!(
            transport_failure("upstream call failed: operation timed out"),
            FailureClass::Timeout
        );
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
            api_key: Some("test-key".into()),
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
                assert_eq!(req["model"], "claude-sonnet-5-5");
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
            api_key: Some("test-anthropic-key".into()),
            timeout_secs: 10,
            max_output_tokens: None,
            cache: None,
        });
        let gateway = spawn(router(state)).await;

        // Post standard OpenAI format to the gateway
        let body = json!({
            "model": "claude-sonnet",
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
