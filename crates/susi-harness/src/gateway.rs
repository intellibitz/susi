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
//!   inventing a number would be worse than saying so. A streamed call to an
//!   Anthropic model is made buffered (the Messages API streams its own event
//!   protocol) and its translated answer is sent as an OpenAI stream.
//!
//! A client that hangs up does not cancel the meter: once a call has gone to
//! the provider, it is read to its end and settled on a blocking thread of
//! its own, which the runtime finishes even when the gateway stops. What a
//! caller holds for the call ([`complete_holding`], the ingress's reservation
//! against an account cap) is kept until then too, not until the client goes.
//!
//! A provider with a configured rate limit ([`crate::ratelimit`]) is paced:
//! a call waits for its slot, and a call whose slot is further off than the
//! gateway's upstream timeout (at most [`crate::ratelimit::MAX_PAUSE_MS`]) is
//! answered 429 with a `Retry-After` at once, sending nothing upstream and
//! writing no row.
//!
//! A provider that refuses `max_tokens` and asks for `max_completion_tokens`
//! (OpenAI's reasoning models answer 400 `unsupported_parameter`) is sent the
//! same call once more with the same limit under that name, before the caller
//! hears of the refusal ([`token_limit_resend`]). The resend is a call of its
//! own and takes a paced slot of its own; the refused call billed nothing and
//! writes no row.
//!
//! A provider whose account cannot pay for the call (an "insufficient
//! balance" body, which Anthropic sends as a plain 400, or a 402) is answered
//! 429 `insufficient_quota` with the provider's cooldown as `Retry-After`
//! ([`unfunded`]): the model cannot serve right now, which is a temporary
//! condition, not a malformed request, so the caller backs off or escalates
//! as it would from a rate limit, and the provider's own words go with it.
//! A 400 from the gateway always means a request it cannot read or forward.

use crate::cache::ResponseCache;
use crate::catalog::PriceCatalogue;
use crate::ledger::{append_entry, Dispatch, FailureClass, LedgerEntry, Outcome};
use crate::pricing::{ModelPrice, TokenUsage};
use crate::usage::token_usage_from_json;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Router;
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
/// so every call that reached a provider still gets its ledger row. A call
/// whose client has already gone is no longer in flight for the server; it
/// is settled on the blocking thread that made it, which the runtime
/// finishes before the process exits.
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
/// The gateway's own route calls this; the authenticated ingress calls
/// [`complete_holding`] after its own admission checks.
pub async fn complete(state: Arc<GatewayState>, headers: HeaderMap, body: String) -> Response {
    complete_holding(state, headers, body, None).await
}

/// What a caller of [`complete_holding`] keeps alive until the call is
/// settled. Dropping it is the signal: nothing is read from it.
pub type Held = Option<Box<dyn Send>>;

/// Forward one completion through the meter, keeping `held` until the call is
/// settled: it is dropped once the call's ledger row is written, or once it
/// is clear that none will be (the request is refused before any provider is
/// called, a paced call is turned away or abandoned while it waits, a stream
/// ends without usage).
///
/// A client that hangs up does not end the hold while the provider is still
/// answering. The ingress holds an account's reservation this way: dropped
/// with the request, it went back the moment a client hung up, while the
/// detached call went on billing, so a caller that closed each socket once
/// its request was sent had every call admitted against the same remaining
/// dollar.
pub async fn complete_holding(
    state: Arc<GatewayState>,
    headers: HeaderMap,
    body: String,
    held: Held,
) -> Response {
    let Ok(mut request) = serde_json::from_str::<Value>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "request body is not JSON");
    };
    if let Some(cap) = state.max_output_tokens {
        clamp_output_tokens(&mut request, cap);
    }
    // `stream` decides how the answer is read and metered, so it must mean the
    // same here and upstream. `"stream":"true"` read as buffered here while a
    // lenient server streamed: the answer could not be parsed, the call was
    // billed, and no row was written.
    if request
        .get("stream")
        .is_some_and(|stream| !stream.is_boolean() && !stream.is_null())
    {
        return error_response(StatusCode::BAD_REQUEST, "`stream` must be true or false");
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
                let id = format!("chatcmpl-cache-{cache_key}");
                let mut res = sse_response(stream_of(&cached_response, &id, &attempt.model));
                res.headers_mut().insert(
                    header::HeaderName::from_static("x-susi-cache-key"),
                    HeaderValue::from_str(&cache_key).unwrap_or(HeaderValue::from_static("hit")),
                );
                return res;
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

    // The wait for a paced slot happens here, in the request: a client that
    // gives up meanwhile drops this future and the reservation with it, so
    // the slot goes back instead of counting against the provider's minute.
    // A slot further off than the cap is not queued for: under sustained
    // overload the queue otherwise ran past every client's timeout and the
    // provider served nothing for as long as the load lasted.
    let reservation = match crate::ratelimit::Pacer::global().hold_within(
        &attempt.provider,
        now_ms(),
        None,
        queue_cap_ms(&state),
    ) {
        Ok(reservation) => reservation,
        Err(wait_ms) => return paced_out(&attempt.provider, wait_ms),
    };
    if reservation.wait_ms() > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(reservation.wait_ms())).await;
    }
    reservation.keep();

    // From here the provider is called, and bills. The call and its ledger row
    // run in a task of their own: hyper drops this handler when the client
    // hangs up (a worker killed at its time limit, Esc in an agent, a client
    // timeout shorter than the gateway's), and the row used to be written by
    // the handler, so a billed call went unrecorded. Inside that task the row
    // is written on the blocking thread that makes the call, which a stopping
    // runtime still finishes. The caller's hold goes with the call, so it
    // lasts as long as the call does, not as long as the client.
    let call = tokio::spawn(async move {
        if streaming {
            stream_upstream(state, attempt, forwarded, cache_key, held).await
        } else {
            let answer = buffered_upstream(state, attempt, forwarded, cache_key, false).await;
            // Its row (and the starved retry's) is written by now.
            drop(held);
            answer.into_response()
        }
    });
    match call.await {
        Ok(response) => response,
        Err(error) => error_response(
            StatusCode::BAD_GATEWAY,
            &format!("upstream task failed: {error}"),
        ),
    }
}

/// The longest a call waits for a paced slot before it is turned away.
///
/// A client of the gateway waits at least as long as the gateway waits for
/// the provider, or its calls would fail on every slow answer, but it is not
/// known to wait for that twice: a call queued longer could start while its
/// client still waits and be answered, and billed, after the client gave up.
/// So the queue holds a call for at most the upstream timeout (and never
/// longer than a `Retry-After` pause, [`crate::ratelimit::MAX_PAUSE_MS`]),
/// and a call whose slot is further off is answered 429 with a `Retry-After`
/// at once. The in-tree worker waits up to 300 s for a turn, which covers a
/// full queue and a full upstream wait at the default 120 s timeout, and resends
/// a turn the queue turned away once its `Retry-After` has passed
/// ([`crate::worker`]): it failed the whole attempt on that 429 before, so a
/// burst of more callers than the queue holds lost attempts that had waited
/// and been served when the queue had no cap.
fn queue_cap_ms(state: &GatewayState) -> u64 {
    crate::ratelimit::MAX_PAUSE_MS.min(state.timeout_secs.max(1).saturating_mul(1_000))
}

/// The answer to a call whose paced slot is further off than
/// [`queue_cap_ms`]: 429 in OpenAI's error shape, with the wait as
/// `Retry-After`, so a client backs off as it would from the provider itself.
/// Nothing went upstream, so no row is written.
fn paced_out(provider: &str, wait_ms: u64) -> Response {
    paced_answer(provider, wait_ms).into_response()
}

/// [`paced_out`] as an [`Answer`], for a call the gateway makes again on its
/// own ([`own_slot`]) and finds no slot for.
fn paced_answer(provider: &str, wait_ms: u64) -> Answer {
    let seconds = wait_ms.div_ceil(1_000);
    let body = json!({
        "error": {
            "message": format!(
                "{provider} is paced to its configured rate limit and its next free slot is {seconds}s away; retry then"
            ),
            "type": "rate_limit_exceeded",
            "code": "rate_limit_exceeded"
        }
    });
    Answer {
        status: StatusCode::TOO_MANY_REQUESTS,
        content_type: "application/json".into(),
        body: body.to_string().into_bytes(),
        retry_after: Some(seconds),
    }
}

/// A paced slot for a call the gateway makes again on its own (the retry of
/// a starved answer, the resend of a refused `max_tokens`), waited for here.
/// The call runs detached from its client, so the slot is kept at once.
///
/// # Errors
///
/// The wait, when the slot is further off than [`queue_cap_ms`]: the client
/// is most likely gone by then.
async fn own_slot(state: &GatewayState, provider: &str) -> Result<(), u64> {
    let reservation = crate::ratelimit::Pacer::global().hold_within(
        provider,
        now_ms(),
        None,
        queue_cap_ms(state),
    )?;
    let wait = reservation.wait_ms();
    reservation.keep();
    if wait > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
    }
    Ok(())
}

/// A finished buffered exchange, as the gateway answers it.
///
/// Kept as bytes rather than a [`Response`] so a streaming client can be
/// sent the same answer as a stream ([`Answer::into_stream`]).
struct Answer {
    status: StatusCode,
    content_type: String,
    body: Vec<u8>,
    /// Seconds for the `Retry-After` of a call turned away ([`paced_answer`]).
    retry_after: Option<u64>,
}

impl Answer {
    fn error(status: StatusCode, message: &str) -> Self {
        Self {
            status,
            content_type: "application/json".into(),
            body: json!({ "error": { "message": message } })
                .to_string()
                .into_bytes(),
            retry_after: None,
        }
    }

    fn into_response(self) -> Response {
        let mut response = Response::new(axum::body::Body::from(self.body));
        *response.status_mut() = self.status;
        if let Ok(value) = HeaderValue::from_str(&self.content_type) {
            response.headers_mut().insert(header::CONTENT_TYPE, value);
        }
        if let Some(seconds) = self.retry_after {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        response
    }

    /// The answer as the OpenAI stream a streaming client reads; an error is
    /// answered as it is, which is what an OpenAI client expects of a stream
    /// that never started.
    fn into_stream(self, model: &str) -> Response {
        if !self.status.is_success() {
            return self.into_response();
        }
        let Ok(answer) = serde_json::from_slice::<Value>(&self.body) else {
            return self.into_response();
        };
        let id = answer
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("chatcmpl-susi")
            .to_string();
        sse_response(stream_of(&answer, &id, model))
    }
}

/// An OpenAI `chat.completion` answer as the `chat.completion.chunk` events a
/// streaming client reads: the whole message in one delta (content, reasoning
/// and tool calls, each call with the `index` a stream carries), the answer's
/// own finish reason, the usage chunk, and `[DONE]`.
///
/// A cached answer replayed to a streaming request used to be one content
/// chunk ending in `stop`: a cached tool call reached the agent as an empty
/// final answer, and a cut-off answer read as complete.
fn stream_of(answer: &Value, id: &str, model: &str) -> String {
    let choice = answer.pointer("/choices/0");
    let message = choice.and_then(|choice| choice.get("message"));
    let field = |name: &str| {
        message
            .and_then(|message| message.get(name))
            .filter(|value| !value.is_null())
    };
    let mut delta = json!({
        "role": "assistant",
        "content": field("content").cloned().unwrap_or(Value::Null)
    });
    if let Some(reasoning) = field("reasoning_content") {
        delta["reasoning_content"] = reasoning.clone();
    }
    if let Some(calls) = field("tool_calls")
        .and_then(Value::as_array)
        .filter(|calls| !calls.is_empty())
    {
        let indexed: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(index, call)| {
                let mut call = call.clone();
                if let Some(object) = call.as_object_mut() {
                    object.insert("index".into(), json!(index));
                }
                call
            })
            .collect();
        delta["tool_calls"] = Value::Array(indexed);
    }
    let finish = choice
        .and_then(|choice| choice.get("finish_reason"))
        .filter(|finish| !finish.is_null())
        .cloned()
        .unwrap_or_else(|| json!("stop"));
    let created = answer
        .get("created")
        .and_then(Value::as_u64)
        .filter(|created| *created > 0)
        .unwrap_or_else(|| now_ms() / 1000);
    let chunk = |choices: Value| {
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": choices
        })
    };
    let mut events = vec![
        chunk(json!([{"index": 0, "delta": delta, "finish_reason": null}])),
        chunk(json!([{"index": 0, "delta": {}, "finish_reason": finish}])),
    ];
    // Every stream the gateway forwards carries the usage chunk (it asks the
    // provider for one), so a synthesized stream does too.
    if let Some(usage) = answer.get("usage").filter(|usage| usage.is_object()) {
        let mut last = chunk(json!([]));
        last["usage"] = usage.clone();
        events.push(last);
    }
    let mut body = String::new();
    for event in events {
        body.push_str("data: ");
        body.push_str(&event.to_string());
        body.push_str("\n\n");
    }
    body.push_str("data: [DONE]\n\n");
    body
}

fn sse_response(body: String) -> Response {
    let mut response = Response::new(axum::body::Body::from(body));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    response
}

/// `body` without `stream` and `stream_options`, for a streamed request that
/// is made buffered.
fn without_stream(body: &str) -> String {
    let Ok(mut request) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };
    if let Some(object) = request.as_object_mut() {
        object.remove("stream");
        object.remove("stream_options");
    }
    serde_json::to_string(&request).unwrap_or_else(|_| body.to_string())
}

fn with_usage_in_stream(request: &Value, original: &str) -> String {
    let mut request = request.clone();
    if let Value::Object(map) = &mut request {
        map.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    serde_json::to_string(&request).unwrap_or_else(|_| original.to_string())
}

/// What a buffered exchange came to.
struct Settled {
    answer: Answer,
    /// The answer spent the output limit on hidden thinking and holds
    /// nothing usable ([`starved`]); it is on the ledger but not cached.
    starved: bool,
    /// The provider refused the call's `max_tokens` for
    /// `max_completion_tokens` ([`token_limit_resend`]): the body to send
    /// instead. The refusal is not on the ledger and is not the answer.
    resend: Option<String>,
}

impl Settled {
    /// An answer the gateway neither retries nor sends again.
    fn as_is(answer: Answer) -> Self {
        Self {
            answer,
            starved: false,
            resend: None,
        }
    }
}

/// The body to send instead of `sent` when the provider answered it `status`
/// with `answer`, refusing its `max_tokens` and asking for
/// `max_completion_tokens`: the same call with the same limit under that
/// name ([`susi_adapters_llm::inference_wire::token_param_retry`]). `None`
/// for any other answer, and for a body without `max_tokens`, so a call is
/// sent again this way at most once.
///
/// A capped account's call goes out with its priced limit as `max_tokens`
/// ([`crate::ingress`]), as does every call under `--max-output-tokens`, and
/// OpenAI's reasoning models refused each one: HTTP 400
/// `unsupported_parameter`, for a request the caller may not have given a
/// limit at all.
fn token_limit_resend(sent: &[u8], status: u16, answer: &[u8]) -> Option<String> {
    if status != 400 {
        return None;
    }
    let request = serde_json::from_slice::<Value>(sent).ok()?;
    let error = String::from_utf8_lossy(answer);
    let resend = susi_adapters_llm::inference_wire::token_param_retry(&request, &error)?;
    serde_json::to_string(&resend).ok()
}

async fn buffered_upstream(
    state: Arc<GatewayState>,
    attempt: Attempt,
    body: String,
    cache_key: String,
    retried: bool,
) -> Answer {
    let retryable = (!retried).then(|| body.clone());
    let started_unix_ms = now_ms();
    let route = match crate::router::resolve_route(
        &attempt.model,
        Some(&state.upstream_chat_url),
        state.api_key.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => return Answer::error(StatusCode::BAD_GATEWAY, &format!("resolve route: {e}")),
    };

    let is_anthropic = route.is_anthropic;

    let outgoing_body = if is_anthropic {
        let Ok(mut parsed) = serde_json::from_str::<Value>(&body) else {
            return Answer::error(StatusCode::BAD_REQUEST, "invalid json request body");
        };
        if let Some(obj) = parsed.as_object_mut() {
            obj.insert("model".to_string(), Value::String(route.model_id.clone()));
        }
        match susi_adapters_llm::inference_wire::openai_to_anthropic_messages(&parsed) {
            Ok(v) => serde_json::to_vec(&v).unwrap_or_else(|_| body.into_bytes()),
            Err(e) => {
                return Answer::error(
                    StatusCode::BAD_REQUEST,
                    &format!("anthropic translation: {e}"),
                )
            }
        }
    } else if route.model_id != attempt.model {
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

    // The call and its settlement run together on a blocking thread. This
    // future is detached from the client, but when the gateway stops the
    // runtime drops a detached task at its next await, and finishes only the
    // blocking threads it has started: a client hung up, Ctrl-C then reached
    // the gateway, and the row of a call the provider had billed, settled out
    // here, was never written.
    let settling = {
        let state = Arc::clone(&state);
        let exchange = Exchange {
            tag: attempt.clone(),
            started_unix_ms,
            is_anthropic,
            cache_key: cache_key.clone(),
        };
        tokio::task::spawn_blocking(move || {
            let headers_ref: Vec<(&str, &str)> = route_headers
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect();
            let call = susi_http_transport::http_call_with_body(
                "POST",
                &target_url,
                &headers_ref,
                Some(&outgoing_body),
                state.timeout_secs,
                0,
            );
            settle_buffered(&state, &exchange, &outgoing_body, call)
        })
    };
    let settled = match settling.await {
        Ok(settled) => settled,
        Err(error) => {
            return Answer::error(
                StatusCode::BAD_GATEWAY,
                &format!("upstream task failed: {error}"),
            );
        }
    };
    // The provider refused `max_tokens` and asked for `max_completion_tokens`:
    // the same call goes again with the limit under that name. The resend
    // is a call of its own, so it takes a paced slot of its own, as the
    // starved retry does; whatever the caller holds is kept through it, and
    // the resend may still be retried once if it comes back starved. A slot
    // further off than the cap is answered as the first call's would have
    // been: 429 with a `Retry-After`, not a refusal of a request the caller
    // may never have put a `max_tokens` in.
    if let Some(body) = settled.resend {
        return match own_slot(&state, &attempt.provider).await {
            Ok(()) => Box::pin(buffered_upstream(state, attempt, body, cache_key, retried)).await,
            Err(wait_ms) => paced_answer(&attempt.provider, wait_ms),
        };
    }
    // The starved call stays on the ledger: its hidden thinking was billed.
    // The caller gets one more call with room to answer.
    if settled.starved {
        if let Some(grown) =
            retryable.and_then(|body| with_headroom(&body, state.max_output_tokens))
        {
            // The retry is a call of its own and waits for a slot of its
            // own: on the first call's slot alone, a starved answer put two
            // calls in the provider's minute and drew the 429s the pacer is
            // there to prevent. A slot further off than the cap is one the
            // client gives up before; it gets the answer the provider gave
            // instead.
            if own_slot(&state, &attempt.provider).await.is_ok() {
                return Box::pin(buffered_upstream(state, attempt, grown, cache_key, true)).await;
            }
        }
    }
    settled.answer
}

/// What settling a buffered call needs besides its answer.
struct Exchange {
    tag: Attempt,
    started_unix_ms: u64,
    /// The call went to the Messages API, so its answer is translated back.
    is_anthropic: bool,
    cache_key: String,
}

/// Read a buffered call's answer, put it on the ledger (and in the cache),
/// and shape it for the caller. Runs on the blocking thread that made the
/// call, so a stopping gateway still finishes it ([`buffered_upstream`]).
fn settle_buffered(
    state: &GatewayState,
    exchange: &Exchange,
    sent: &[u8],
    call: Result<susi_http_transport::HttpCall, String>,
) -> Settled {
    let tag = &exchange.tag;
    let started_unix_ms = exchange.started_unix_ms;
    let call = match call {
        Ok(call) => call,
        Err(error) => {
            let message = format!("upstream call failed: {error}");
            record_failure(
                &state.ledger_path,
                tag,
                started_unix_ms,
                transport_failure(&message),
            );
            return Settled::as_is(Answer::error(StatusCode::BAD_GATEWAY, &message));
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
            return Settled::as_is(Answer::error(
                StatusCode::BAD_GATEWAY,
                &format!("upstream body: {error}"),
            ));
        }
    };
    // Refused for naming its limit `max_tokens`: sent again, before anything
    // is booked or answered ([`buffered_upstream`]). The refusal billed
    // nothing, so it writes no row. The Messages API reads only `max_tokens`.
    if !exchange.is_anthropic {
        if let Some(body) = token_limit_resend(sent, status, &response_bytes) {
            return Settled {
                resend: Some(body),
                ..Settled::as_is(Answer::error(
                    StatusCode::BAD_REQUEST,
                    "the provider refused max_tokens",
                ))
            };
        }
    }
    let ended_unix_ms = now_ms();
    let class = provider_failure(status, &response_bytes);
    if let Some(class) = class {
        record_failure(&state.ledger_path, tag, started_unix_ms, class);
    }

    // A provider's error body can echo the key it was sent; it goes back to
    // the caller with every key-shaped run redacted.
    let response_bytes = if (200..300).contains(&status) {
        response_bytes
    } else {
        crate::secrets::redact(&String::from_utf8_lossy(&response_bytes)).into_bytes()
    };
    // The account cannot pay: a temporary refusal, answered as one.
    if class == Some(FailureClass::NoCredit) {
        return Settled::as_is(unfunded(tag, status, &response_bytes));
    }
    let response_bytes = if (200..300).contains(&status) && exchange.is_anthropic {
        if let Ok(anth_val) = serde_json::from_slice::<Value>(&response_bytes) {
            match susi_adapters_llm::inference_wire::anthropic_to_openai_chat(&anth_val, &tag.model)
            {
                Ok(openai_val) => serde_json::to_vec(&openai_val).unwrap_or(response_bytes),
                Err(_) => response_bytes,
            }
        } else {
            response_bytes
        }
    } else {
        response_bytes
    };

    let mut starved_answer = false;
    if (200..300).contains(&status) {
        let Ok(value) = serde_json::from_slice::<Value>(&response_bytes) else {
            return Settled::as_is(Answer::error(
                StatusCode::BAD_GATEWAY,
                "upstream answer is not JSON, so it cannot be priced",
            ));
        };
        let usage = match token_usage_from_json(&value) {
            Ok(usage) => usage,
            Err(error) => {
                return Settled::as_is(Answer::error(
                    StatusCode::BAD_GATEWAY,
                    &format!("unmetered provider answer: {error}"),
                ));
            }
        };
        crate::ratelimit::Pacer::global().record_tokens(
            &tag.provider,
            ended_unix_ms,
            usage.input.saturating_add(usage.output),
        );
        // The provider's own figure for the call, where it states one, is
        // the bill; the catalogue's price is what it was admitted at.
        let entry = LedgerEntry::settle(Dispatch {
            mission: tag.mission.clone(),
            task: tag.task.clone(),
            attempt: tag.attempt,
            model: tag.model.clone(),
            provider: tag.provider.clone(),
            usage,
            price: tag.price,
            started_unix_ms,
            ended_unix_ms,
            outcome: Outcome::Completed,
        })
        .at_provider_cost(crate::usage::provider_cost_from_json(&value));
        // A starved answer is not this request's answer: cached, it was
        // replayed to every later identical call at no cost and no content.
        starved_answer = starved(&value);
        if !starved_answer {
            if let Some(ref cache) = state.cache {
                cache.set(&exchange.cache_key, &value);
            }
        }
        let entry = if cached { entry.as_cached() } else { entry };
        if let Err(error) = append_entry(&state.ledger_path, &entry) {
            return Settled::as_is(Answer::error(
                StatusCode::BAD_GATEWAY,
                &format!("ledger append failed: {error}"),
            ));
        }
    }

    Settled {
        answer: Answer {
            status: StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY),
            content_type,
            body: response_bytes,
            retry_after: None,
        },
        starved: starved_answer,
        resend: None,
    }
}

/// What the blocking thread that makes a streamed call tells the request.
enum Upstream {
    /// The provider answered with this status and content type (and, for a
    /// refusal the gateway answers as a 429, the `Retry-After` seconds); the
    /// body follows on the chunk channel.
    Answered(u16, String, Option<u64>),
    /// The call got no answer.
    Failed(String),
    /// The provider refused the call's `max_tokens` for
    /// `max_completion_tokens` ([`token_limit_resend`]): the body to send
    /// instead, and the caller's hold, kept through the resend.
    Resend(String, Held),
}

/// Forward a streamed call; `held` is dropped once its row is written, or
/// once it is clear none will be ([`complete_holding`]).
async fn stream_upstream(
    state: Arc<GatewayState>,
    attempt: Attempt,
    body: String,
    cache_key: String,
    held: Held,
) -> Response {
    let route = match crate::router::resolve_route(
        &attempt.model,
        Some(&state.upstream_chat_url),
        state.api_key.as_deref(),
    ) {
        Ok(r) => r,
        Err(e) => return error_response(StatusCode::BAD_GATEWAY, &format!("resolve route: {e}")),
    };
    if route.is_anthropic {
        // The Messages API refuses an OpenAI chat body (`stream_options`,
        // `role: system` in `messages`: a 400 on every streamed Claude call)
        // and streams its own event protocol, which an OpenAI client cannot
        // read and the meter could not price. The call is made buffered,
        // translated both ways and metered there, and its answer goes back
        // as an OpenAI stream.
        let model = attempt.model.clone();
        let answer =
            buffered_upstream(state, attempt, without_stream(&body), cache_key, false).await;
        drop(held);
        return answer.into_stream(&model);
    }
    let tag = attempt.clone();
    let again = attempt.clone();
    let Attempt {
        mission,
        task,
        attempt,
        model,
        price,
        ..
    } = attempt;
    let started_unix_ms = now_ms();

    let (chunk_tx, mut chunk_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    let (meta_tx, meta_rx) = tokio::sync::oneshot::channel::<Upstream>();
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
                // The hold ends with the row, before the caller hears.
                drop(held);
                let _ = meta_tx.send(Upstream::Failed(message));
                return;
            }
        };
        let status = call.status;
        pause_on_retry_after(&provider, status, call.header("retry-after"));
        // A susi gateway upstream of this one marks an answer its cache
        // served: the provider was not called, so the row must not charge
        // for it. The buffered path read the mark and this one did not, and
        // the replay carries a usage chunk, so a cache hit streamed through
        // two gateways was booked at full price on the front one.
        let cached = call.header("x-susi-cache-key").is_some();
        let content_type = call
            .header("content-type")
            .unwrap_or("text/event-stream")
            .to_string();
        if !(200..300).contains(&status) {
            let bytes = call.into_bytes(MAX_RESPONSE_BYTES).unwrap_or_default();
            // Refused for naming its limit `max_tokens`: sent again before
            // the caller hears a status, with the hold kept through it. The
            // refusal billed nothing, so it writes no row.
            if let Some(body) = token_limit_resend(outgoing_body.as_bytes(), status, &bytes) {
                let _ = meta_tx.send(Upstream::Resend(body, held));
                return;
            }
            let class = provider_failure(status, &bytes);
            if let Some(class) = class {
                record_failure(&ledger_path, &tag, started_unix_ms, class);
            }
            drop(held);
            let bytes = crate::secrets::redact(&String::from_utf8_lossy(&bytes)).into_bytes();
            // The account cannot pay: a temporary refusal, answered as one.
            let (status, content_type, retry_after, bytes) =
                if class == Some(FailureClass::NoCredit) {
                    let answer = unfunded(&tag, status, &bytes);
                    (
                        answer.status.as_u16(),
                        answer.content_type,
                        answer.retry_after,
                        answer.body,
                    )
                } else {
                    (status, content_type, None, bytes)
                };
            if meta_tx
                .send(Upstream::Answered(status, content_type, retry_after))
                .is_ok()
            {
                let _ = chunk_tx.blocking_send(bytes);
            }
            return;
        }
        // `None` once the client has gone. The call was made either way, so it
        // is still read to its end and settled: the provider billed it.
        let mut client = meta_tx
            .send(Upstream::Answered(status, content_type, None))
            .is_ok()
            .then_some(chunk_tx);

        let mut reader = std::io::BufReader::new(call.into_reader());
        let mut usage = None;
        let mut stated_cost = None;
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
                        stated_cost = crate::usage::provider_cost_from_json(&value);
                    }
                }
            }
            // A client that hangs up stops the forwarding, not the reading:
            // the usage comes in the last chunk, and returning here left a
            // billed stream without its row.
            if let Some(sender) = &client {
                if sender.blocking_send(line).is_err() {
                    client = None;
                }
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
                })
                .at_provider_cost(stated_cost);
                let entry = if cached { entry.as_cached() } else { entry };
                if let Err(error) = append_entry(&ledger_path, &entry) {
                    eprintln!("harness-gateway: ledger append failed: {error}");
                }
            }
            None => eprintln!(
                "harness-gateway: stream for {model} ({mission}/{task} attempt {attempt}) ended without usage; not recorded"
            ),
        }
        // Released after the row and before the stream ends for the caller,
        // so a caller that sends its next call the moment this one ends finds
        // this one settled.
        drop(held);
        drop(client);
    });

    match meta_rx.await {
        Ok(Upstream::Answered(status, content_type, retry_after)) => {
            let stream = futures::stream::poll_fn(move |cx| chunk_rx.poll_recv(cx))
                .map(Ok::<Vec<u8>, std::io::Error>);
            let mut response = Response::new(axum::body::Body::from_stream(stream));
            *response.status_mut() =
                StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            if let Ok(value) = HeaderValue::from_str(&content_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            if let Some(seconds) = retry_after {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
            }
            response
        }
        Ok(Upstream::Failed(message)) => error_response(StatusCode::BAD_GATEWAY, &message),
        // The resend is a call of its own and takes a paced slot of its own,
        // as the buffered path's does ([`buffered_upstream`]).
        Ok(Upstream::Resend(body, held)) => match own_slot(&state, &again.provider).await {
            Ok(()) => Box::pin(stream_upstream(state, again, body, cache_key, held)).await,
            Err(wait_ms) => paced_out(&again.provider, wait_ms),
        },
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

/// The answer to a call the provider refused because its account cannot pay
/// ([`FailureClass::NoCredit`]): 429 `insufficient_quota`, as OpenAI words an
/// empty account, with the provider's cooldown
/// ([`crate::policy::COOLDOWN_MS`]) as `Retry-After` and the provider's own
/// status and (redacted) words in the message, so the reason reaches the
/// failure record.
///
/// Anthropic reports an empty account as a plain 400 ("credit balance is too
/// low"), and the gateway passed that status on: the worker read it as a
/// request error, the ladder as a semantic failure, and a live mission spent
/// turn 1 of every later attempt on a provider the ledger already knew could
/// not pay. A balance is a temporary condition of the provider, not a fault
/// in the request, so it is answered as a rate limit is: the caller backs
/// off or goes elsewhere. The `insufficient_quota` code tells a caller that
/// waiting out the `Retry-After` changes nothing ([`crate::worker`]).
fn unfunded(attempt: &Attempt, status: u16, redacted: &[u8]) -> Answer {
    let words = serde_json::from_slice::<Value>(redacted)
        .ok()
        .and_then(|body| {
            body.pointer("/error/message")
                .or_else(|| body.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(redacted).into_owned());
    let words: String = words.chars().take(200).collect();
    let seconds = crate::policy::COOLDOWN_MS.div_ceil(1_000);
    let body = json!({
        "error": {
            "message": format!(
                "{} cannot pay for {} right now ({} answered HTTP {status}: {}); retry in {seconds}s or use another model",
                attempt.provider, attempt.model, attempt.provider, words.trim()
            ),
            "type": "insufficient_quota",
            "code": "insufficient_quota"
        }
    });
    Answer {
        status: StatusCode::TOO_MANY_REQUESTS,
        content_type: "application/json".into(),
        body: body.to_string().into_bytes(),
        retry_after: Some(seconds),
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
    Answer::error(status, message).into_response()
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

/// Output tokens added to a starved request's limit for its one retry.
const STARVED_HEADROOM: u64 = 2_048;

/// An answer that hit its output limit with nothing a caller can use: no
/// text and no tool call. A reasoning model spends `max_tokens` on hidden
/// thinking first. Live, `gemini-flash` with `max_tokens: 40` thought for 37
/// tokens and answered `finish_reason: length` with no content.
fn starved(answer: &Value) -> bool {
    let Some(choice) = answer.get("choices").and_then(|choices| choices.get(0)) else {
        return false;
    };
    let message = choice.get("message");
    let no_text = message
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .is_none_or(|text| text.trim().is_empty());
    let no_tool_call = message
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty);
    choice.get("finish_reason").and_then(Value::as_str) == Some("length") && no_text && no_tool_call
}

/// `body` with [`STARVED_HEADROOM`] more output tokens on every limit it
/// sets, within the gateway's `cap`; `None` when no limit can grow.
fn with_headroom(body: &str, cap: Option<u32>) -> Option<String> {
    let mut request = serde_json::from_str::<Value>(body).ok()?;
    let object = request.as_object_mut()?;
    let mut grew = false;
    for key in ["max_tokens", "max_completion_tokens"] {
        let Some(asked) = object.get(key).and_then(Value::as_u64) else {
            continue;
        };
        let mut grown = asked.saturating_add(STARVED_HEADROOM);
        if let Some(cap) = cap {
            grown = grown.min(u64::from(cap));
        }
        if grown > asked {
            object.insert(key.to_string(), Value::from(grown));
            grew = true;
        }
    }
    grew.then(|| serde_json::to_string(&request).ok()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::read_entries;
    use crate::money::MicroUsd;
    use axum::response::IntoResponse;
    use axum::Json;
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

    /// Serve `app` for one test. Every answer closes its connection: the
    /// transport pools connections per timeout for the whole test process,
    /// and one kept open to a server a finished test left behind was handed
    /// to a later test's call once a new server took that port, which then
    /// failed with os error 10053 (at times a third of runs). Every request
    /// is read in full first, since a connection closed with part of a
    /// request unread is reset on Windows (os error 10054 at the client).
    async fn spawn(app: Router) -> SocketAddr {
        let app = app
            .layer(axum::middleware::map_response(
                |mut response: Response| async move {
                    response
                        .headers_mut()
                        .insert(header::CONNECTION, HeaderValue::from_static("close"));
                    response
                },
            ))
            .layer(axum::middleware::map_request(
                |request: axum::extract::Request| async move {
                    let (parts, body) = request.into_parts();
                    let bytes = axum::body::to_bytes(body, usize::MAX)
                        .await
                        .unwrap_or_default();
                    axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes))
                },
            ));
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

    /// A reasoning model that spends a small `max_tokens` on hidden thinking
    /// and answers nothing, as `gemini-flash` did live.
    async fn thinker(Json(request): Json<Value>) -> Json<Value> {
        let limit = request["max_tokens"].as_u64().unwrap_or(0);
        let (content, finish, output) = if limit < 100 {
            (Value::Null, "length", 0)
        } else {
            (json!("ready"), "stop", 1)
        };
        Json(json!({
            "id": format!("cmpl-{limit}"),
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": content},
                "finish_reason": finish
            }],
            "usage": {"prompt_tokens": 8, "completion_tokens": output}
        }))
    }

    /// The starved answer's retry is a second call in the provider's minute,
    /// so it takes a paced slot of its own instead of riding on the first.
    #[tokio::test]
    async fn a_starved_retry_takes_a_paced_slot_of_its_own() {
        let provider = "pacer-starved-test";
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(thinker))).await;
        let dir = tempfile::tempdir().unwrap();
        let mut state = Arc::try_unwrap(gateway_state(upstream, dir.path().join("ledger.jsonl")))
            .unwrap_or_else(|_| unreachable!());
        state.max_output_tokens = Some(8_000);
        // A model the registry does not know runs on the gateway's provider.
        state.provider = provider.into();
        let gateway = spawn(router(Arc::new(state))).await;

        let request = r#"{"model":"starved-probe-model","max_tokens":40,"messages":[]}"#;
        let (status, body) = post_json(gateway, request, &[]).await;

        assert_eq!(status, 200, "{body}");
        assert_eq!(
            crate::ratelimit::Pacer::global().reserved(provider),
            2,
            "one slot per upstream call"
        );
    }

    /// A reasoning model answers a small `max_tokens` with hidden thinking
    /// and no content, as `gemini-flash` did live. The caller gets the answer
    /// a retry with room produces; both calls are on the ledger; the cache
    /// keeps the real answer, never the empty one.
    #[tokio::test]
    async fn a_starved_answer_is_retried_with_room_and_never_cached() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(thinker))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let cache = Arc::new(ResponseCache::new(None));
        let mut state = Arc::try_unwrap(gateway_state(upstream, ledger_path.clone()))
            .unwrap_or_else(|_| unreachable!());
        state.max_output_tokens = Some(8_000);
        state.cache = Some(Arc::clone(&cache));
        let gateway = spawn(router(Arc::new(state))).await;
        let request = r#"{"model":"deepseek-flash","max_tokens":40,"messages":[{"role":"user","content":"Reply: ready"}]}"#;

        let (status, body) = post_json(gateway, request, &[]).await;

        assert_eq!(status, 200);
        let answer: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(answer["choices"][0]["message"]["content"], "ready");
        assert_eq!(answer["id"], "cmpl-2088", "retried with 2048 more tokens");
        let entries = read_entries(&ledger_path).unwrap();
        assert_eq!(entries.len(), 2, "the starved call was billed too");
        let key = ResponseCache::compute_key(&serde_json::from_str(request).unwrap());
        assert_eq!(cache.get(&key).unwrap()["id"], "cmpl-2088");
    }

    /// A provider that refuses `max_tokens` as OpenAI's reasoning models do
    /// (400 `unsupported_parameter`), answers every other call, buffered or
    /// streamed as asked, and keeps every request it got.
    async fn refuses_max_tokens() -> (SocketAddr, Arc<std::sync::Mutex<Vec<Value>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = {
            let seen = Arc::clone(&seen);
            Router::new().route(
                "/v1/chat/completions",
                post(move |body: String| {
                    let seen = Arc::clone(&seen);
                    async move {
                        let request = serde_json::from_str::<Value>(&body).unwrap();
                        let refused = request.get("max_tokens").is_some();
                        let streaming = request["stream"] == true;
                        seen.lock().unwrap().push(request);
                        if refused {
                            let error = json!({"error": {
                                "message": "Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.",
                                "type": "invalid_request_error",
                                "param": "max_tokens",
                                "code": "unsupported_parameter"
                            }});
                            return (StatusCode::BAD_REQUEST, Json(error)).into_response();
                        }
                        if streaming {
                            streamed().await.into_response()
                        } else {
                            complete().await.into_response()
                        }
                    }
                }),
            )
        };
        (spawn(app).await, seen)
    }

    /// The ingress sends a capped call's limit as `max_tokens`, as the
    /// gateway's own cap does, and OpenAI's reasoning models refused it with
    /// a 400 that went back to the caller. The call is sent again with the
    /// same limit as `max_completion_tokens`, before the caller hears: it
    /// takes a paced slot of its own, the refusal writes no row, and what the
    /// caller holds is kept until the resend's row is written.
    #[tokio::test]
    async fn a_call_whose_max_tokens_is_refused_is_sent_again_as_max_completion_tokens() {
        for (streaming, provider) in [
            (false, "pacer-token-limit-test"),
            (true, "pacer-token-limit-stream-test"),
        ] {
            let (upstream, seen) = refuses_max_tokens().await;
            let dir = tempfile::tempdir().unwrap();
            let ledger_path = dir.path().join("ledger.jsonl");
            let mut state = Arc::try_unwrap(gateway_state(upstream, ledger_path.clone()))
                .unwrap_or_else(|_| unreachable!());
            // A model the registry does not know runs on the gateway's provider.
            state.provider = provider.into();
            let state = Arc::new(state);
            let at_drop = Arc::new(std::sync::Mutex::new(None));
            let held = {
                let (ledger, seen) = (ledger_path.clone(), Arc::clone(&at_drop));
                move |headers: HeaderMap, body: String| {
                    let guard = RowsAtDrop { ledger, seen };
                    complete_holding(state, headers, body, Some(Box::new(guard)))
                }
            };
            let gateway = spawn(Router::new().route("/v1/chat/completions", post(held))).await;
            let request = json!({
                "model": "token-limit-probe-model",
                "max_tokens": 321,
                "stream": streaming,
                "messages": [{"role": "user", "content": "ping"}]
            });

            let (status, answer) = post_json(gateway, &request.to_string(), &[]).await;

            assert_eq!(status, 200, "{answer}");
            if streaming {
                assert!(answer.contains("data: [DONE]"), "{answer}");
            } else {
                assert!(answer.contains("cmpl-1"), "{answer}");
            }
            let rows = rows_eventually(&ledger_path, 1).await;
            assert_eq!(rows.len(), 1, "the refusal billed nothing: {rows:?}");
            assert_eq!(rows[0].outcome, Outcome::Completed);
            let seen = seen.lock().unwrap().clone();
            assert_eq!(seen.len(), 2, "{seen:?}");
            assert_eq!(seen[0]["max_tokens"], 321);
            assert_eq!(seen[1]["max_completion_tokens"], 321, "{}", seen[1]);
            assert!(seen[1].get("max_tokens").is_none(), "{}", seen[1]);
            assert_eq!(
                crate::ratelimit::Pacer::global().reserved(provider),
                2,
                "one slot per upstream call"
            );
            let mut dropped = None;
            for _ in 0..100 {
                dropped = *at_drop.lock().unwrap();
                if dropped.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert_eq!(dropped, Some(1), "held until the resend's row: {request}");
        }
    }

    /// A provider whose account is empty, as Anthropic reports it: a plain
    /// 400 with "credit balance is too low". It also refuses an over-long
    /// prompt with a 400, as every provider does.
    async fn unfunded_provider() -> SocketAddr {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|body: String| async move {
                let request = serde_json::from_str::<Value>(&body).unwrap();
                let too_long = request["messages"][0]["content"] == "too long";
                let error = if too_long {
                    json!({"error": {
                        "type": "invalid_request_error",
                        "message": "prompt is too long: 250000 tokens > 200000 maximum"
                    }})
                } else {
                    json!({"error": {
                        "type": "invalid_request_error",
                        "message": "Your credit balance is too low to access the Anthropic API. Please go to Plans & Billing to upgrade or purchase credits."
                    }})
                };
                (StatusCode::BAD_REQUEST, Json(error)).into_response()
            }),
        );
        spawn(app).await
    }

    /// Anthropic answers an empty account with a plain 400, and the gateway
    /// passed that status on: the worker read it as a request error and the
    /// ladder as a semantic failure, with nothing of why. It is answered 429
    /// `insufficient_quota` with the provider's cooldown as `Retry-After`,
    /// buffered or streamed, the provider's words in the message, and a
    /// `NoCredit` row on the ledger. A 400 about the request itself still
    /// goes back as it came.
    #[tokio::test]
    async fn an_unfunded_providers_refusal_is_answered_429_with_the_cooldown_as_retry_after() {
        for (streaming, provider) in [
            (false, "unfunded-test-provider"),
            (true, "unfunded-stream-test-provider"),
        ] {
            let upstream = unfunded_provider().await;
            let dir = tempfile::tempdir().unwrap();
            let ledger_path = dir.path().join("ledger.jsonl");
            let mut state = Arc::try_unwrap(gateway_state(upstream, ledger_path.clone()))
                .unwrap_or_else(|_| unreachable!());
            state.provider = provider.into();
            let gateway = spawn(router(Arc::new(state))).await;
            let request = json!({
                "model": "unfunded-probe-model",
                "stream": streaming,
                "messages": [{"role": "user", "content": "ping"}]
            });

            let url = format!("http://{gateway}/v1/chat/completions");
            let body = request.to_string();
            let (status, retry_after, answer) = tokio::task::spawn_blocking(move || {
                let call = susi_http_transport::http_call_with_body(
                    "POST",
                    &url,
                    &[],
                    Some(body.as_bytes()),
                    10,
                    0,
                )
                .unwrap();
                let status = call.status;
                let retry_after = call.header("retry-after").map(str::to_string);
                (
                    status,
                    retry_after,
                    call.into_utf8(MAX_RESPONSE_BYTES).unwrap(),
                )
            })
            .await
            .unwrap();

            assert_eq!(status, 429, "streaming={streaming}: {answer}");
            assert_eq!(
                retry_after.as_deref(),
                Some(
                    crate::policy::COOLDOWN_MS
                        .div_ceil(1_000)
                        .to_string()
                        .as_str()
                ),
                "streaming={streaming}: the cooldown is the Retry-After"
            );
            let answer: Value = serde_json::from_str(&answer).unwrap();
            assert_eq!(answer["error"]["code"], "insufficient_quota", "{answer}");
            let message = answer["error"]["message"].as_str().unwrap();
            assert!(message.contains(provider), "{message}");
            assert!(message.contains("unfunded-probe-model"), "{message}");
            assert!(message.contains("HTTP 400"), "{message}");
            assert!(message.contains("credit balance is too low"), "{message}");
            let rows = rows_eventually(&ledger_path, 1).await;
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert_eq!(
                rows[0].outcome,
                Outcome::ModelFailed {
                    class: FailureClass::NoCredit
                }
            );
            assert_eq!(rows[0].provider, provider);

            // A 400 about the request is the request's fault: it goes back as
            // the provider sent it, and no provider failure is recorded.
            let request = json!({
                "model": "unfunded-probe-model",
                "stream": streaming,
                "messages": [{"role": "user", "content": "too long"}]
            });
            let (status, answer) = post_json(gateway, &request.to_string(), &[]).await;
            assert_eq!(status, 400, "streaming={streaming}: {answer}");
            assert!(answer.contains("prompt is too long"), "{answer}");
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            assert_eq!(read_entries(&ledger_path).unwrap().len(), 1, "no new row");
        }
    }

    #[test]
    fn only_an_answer_with_nothing_usable_is_starved() {
        let answer = |message: Value, finish: &str| json!({"choices": [{"message": message, "finish_reason": finish}]});
        assert!(starved(&answer(json!({"content": null}), "length")));
        assert!(starved(&answer(json!({"content": "  "}), "length")));
        assert!(!starved(&answer(json!({"content": "partial"}), "length")));
        assert!(!starved(&answer(json!({"content": null}), "stop")));
        let tool_call = json!({"content": null, "tool_calls": [{"id": "t"}]});
        assert!(!starved(&answer(tool_call, "length")));
        // Headroom stays inside the operator's cap and never shrinks a limit.
        assert_eq!(
            with_headroom(r#"{"max_tokens":40}"#, Some(1_000)).as_deref(),
            Some(r#"{"max_tokens":1000}"#)
        );
        assert_eq!(with_headroom(r#"{"max_tokens":8000}"#, Some(8_000)), None);
        assert_eq!(with_headroom(r#"{"model":"m"}"#, Some(8_000)), None);
    }

    /// A `stream` that is not a boolean was read as buffered here while a
    /// lenient upstream streamed: the SSE answer could not be priced, the
    /// call was billed, and no row was written. It is refused before any
    /// provider is called.
    #[tokio::test]
    async fn a_stream_flag_that_is_not_a_boolean_is_refused_before_the_call() {
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;
        for stream in [r#""true""#, "1"] {
            let body = format!(r#"{{"model":"deepseek-flash","stream":{stream},"messages":[]}}"#);
            let (status, answer) = post_json(gateway, &body, &[]).await;
            assert_eq!(status, 400, "{answer}");
            assert!(
                answer.contains("`stream` must be true or false"),
                "{answer}"
            );
        }
        assert!(
            read_entries(&ledger_path).unwrap_or_default().is_empty(),
            "nothing reached the provider"
        );
        let (status, _) = post_json(
            gateway,
            r#"{"model":"deepseek-flash","stream":false,"messages":[]}"#,
            &[],
        )
        .await;
        assert_eq!(status, 200, "a boolean stream flag is served as before");
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

        assert_eq!(status, 200, "{body}");
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

    /// The JSON events of an SSE body, without the closing `[DONE]`.
    fn sse_events(body: &str) -> Vec<Value> {
        body.lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|data| *data != "[DONE]")
            .map(|data| serde_json::from_str(data).unwrap())
            .collect()
    }

    const READ_FILE_TOOL: &str =
        r#"[{"type":"function","function":{"name":"read_file","parameters":{"type":"object"}}}]"#;

    /// A tool-calling turn answered buffered, then asked again streamed: the
    /// replay must still carry the tool call and its finish reason. It used
    /// to be one empty content chunk ending in `stop`, which an agent loop
    /// reads as a final empty answer.
    #[tokio::test]
    async fn a_cached_tool_call_replayed_to_a_stream_keeps_the_call_and_its_finish() {
        async fn tool_call() -> Json<Value> {
            Json(json!({
                "id": "cmpl-tool",
                "object": "chat.completion",
                "choices": [{
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": "call_1",
                            "type": "function",
                            "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"}
                        }]
                    },
                    "finish_reason": "tool_calls"
                }],
                "usage": {"prompt_tokens": 12, "completion_tokens": 7}
            }))
        }
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(tool_call))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let mut state = Arc::try_unwrap(gateway_state(upstream, ledger_path.clone()))
            .unwrap_or_else(|_| unreachable!());
        state.cache = Some(Arc::new(ResponseCache::new(None)));
        let gateway = spawn(router(Arc::new(state))).await;
        let buffered = format!(
            r#"{{"model":"deepseek-flash","messages":[{{"role":"user","content":"read a.rs"}}],"tools":{READ_FILE_TOOL}}}"#
        );
        let streamed = buffered.replacen('{', r#"{"stream":true,"#, 1);

        let (status, _) = post_json(gateway, &buffered, &[]).await;
        assert_eq!(status, 200);
        let (status, body) = post_json(gateway, &streamed, &[]).await;

        assert_eq!(status, 200, "{body}");
        assert!(body.trim_end().ends_with("data: [DONE]"), "{body}");
        let events = sse_events(&body);
        let call = events
            .iter()
            .find_map(|event| event["choices"][0]["delta"]["tool_calls"].get(0))
            .expect("the replay carries the tool call");
        assert_eq!(call["index"], 0);
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["function"]["name"], "read_file");
        assert_eq!(call["function"]["arguments"], "{\"path\":\"a.rs\"}");
        let finish: Vec<&Value> = events
            .iter()
            .map(|event| &event["choices"][0]["finish_reason"])
            .filter(|finish| !finish.is_null())
            .collect();
        assert_eq!(finish, [&json!("tool_calls")]);
        assert!(events
            .iter()
            .any(|event| event["usage"]["prompt_tokens"] == 12));
        let rows = read_entries(&ledger_path).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows[1].cached);
    }

    /// An OpenAI client streaming to a Claude model (most agents stream by
    /// default): the Messages API gets a Messages request, never the OpenAI
    /// body (it answered 400 to `stream_options` and `role: system`), and the
    /// client gets an OpenAI stream with the tool call, its finish reason and
    /// the usage, which is metered into the ledger.
    #[tokio::test]
    async fn a_streamed_call_to_an_anthropic_model_is_translated_and_metered() {
        let app = Router::new().route(
            "/v1/messages",
            post(|body: String| async move {
                let request: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(request["model"], "claude-sonnet-5-5");
                assert!(request.get("stream_options").is_none(), "{request}");
                assert_ne!(request["stream"], json!(true), "{request}");
                assert_eq!(request["system"][0]["text"], "You are a test assistant.");
                assert!(request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|message| message["role"] != "system"));
                assert_eq!(request["tools"][0]["name"], "read_file");
                Json(json!({
                    "id": "msg_stream_1",
                    "type": "message",
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": "Reading it."},
                        {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.rs"}}
                    ],
                    "stop_reason": "tool_use",
                    "usage": {"input_tokens": 20, "output_tokens": 10, "cache_read_input_tokens": 5}
                }))
            }),
        );
        let upstream = spawn(app).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let state = Arc::new(GatewayState {
            catalogue: catalogue(),
            ledger_path: ledger_path.clone(),
            upstream_chat_url: format!("http://{upstream}/v1/messages"),
            provider: "anthropic".into(),
            api_key: Some("test-anthropic-key".into()),
            timeout_secs: 10,
            max_output_tokens: None,
            cache: None,
        });
        let gateway = spawn(router(state)).await;
        let body = format!(
            r#"{{"model":"claude-sonnet","stream":true,"messages":[{{"role":"system","content":"You are a test assistant."}},{{"role":"user","content":"read a.rs"}}],"tools":{READ_FILE_TOOL}}}"#
        );

        let (status, answer) = post_json(gateway, &body, &[("x-susi-mission", "m-a")]).await;

        assert_eq!(status, 200, "{answer}");
        assert!(answer.trim_end().ends_with("data: [DONE]"), "{answer}");
        let events = sse_events(&answer);
        assert!(events
            .iter()
            .all(|event| event["object"] == "chat.completion.chunk"));
        let text: String = events
            .iter()
            .filter_map(|event| event["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(text, "Reading it.");
        let call = events
            .iter()
            .find_map(|event| event["choices"][0]["delta"]["tool_calls"].get(0))
            .expect("the stream carries the tool call");
        assert_eq!(call["index"], 0);
        assert_eq!(call["id"], "toolu_1");
        assert_eq!(call["function"]["name"], "read_file");
        assert_eq!(
            serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap()).unwrap(),
            json!({"path": "a.rs"})
        );
        assert!(events
            .iter()
            .any(|event| event["choices"][0]["finish_reason"] == "tool_calls"));
        let usage = events
            .iter()
            .find(|event| event["usage"].is_object())
            .expect("the stream carries the usage");
        assert_eq!(usage["usage"]["prompt_tokens"], 25);
        assert_eq!(usage["usage"]["completion_tokens"], 10);
        let rows = read_entries(&ledger_path).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].mission, "m-a");
        assert_eq!(rows[0].provider, "anthropic");
        assert_eq!(rows[0].usage.input, 25);
        assert_eq!(rows[0].usage.cache_hit, 5);
        assert_eq!(rows[0].usage.output, 10);
        assert!(rows[0].cost.micros() > 0);
    }

    /// Send `body` over a raw socket and hang up: at once (after a moment for
    /// the gateway to start the call) when `wait_for` is empty, otherwise as
    /// soon as the answer shows `wait_for`. This is a worker killed at its
    /// time limit, or Esc in an agent, as the gateway sees it.
    async fn post_and_hang_up(address: SocketAddr, body: &str, wait_for: &str) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: {address}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(request.as_bytes()).await.unwrap();
        if wait_for.is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        } else {
            let mut seen = Vec::new();
            let mut buffer = [0u8; 1024];
            while !String::from_utf8_lossy(&seen).contains(wait_for) {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0, "the gateway closed before `{wait_for}`");
                seen.extend_from_slice(&buffer[..read]);
            }
        }
        drop(socket);
    }

    /// The ledger's rows once it holds `rows` of them, or what it holds after
    /// five seconds.
    async fn rows_eventually(path: &Path, rows: usize) -> Vec<LedgerEntry> {
        for _ in 0..100 {
            let (entries, _) = crate::ledger::read_entries_lenient(path);
            if entries.len() >= rows {
                return entries;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        crate::ledger::read_entries_lenient(path).0
    }

    /// The provider answers after the client has gone. It billed the call, so
    /// the row must be written anyway: hyper drops the handler of a closed
    /// connection, and the settlement used to live in that handler.
    #[tokio::test]
    async fn a_buffered_call_whose_client_hangs_up_still_writes_its_row() {
        async fn slow() -> Json<Value> {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            complete().await
        }
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(slow))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        post_and_hang_up(gateway, r#"{"model":"deepseek-flash","messages":[]}"#, "").await;

        let rows = rows_eventually(&ledger_path, 1).await;
        assert_eq!(rows.len(), 1, "the billed call has its row");
        assert_eq!(rows[0].cost.micros(), 217_200);
        assert_eq!(rows[0].outcome, Outcome::Completed);
    }

    /// A streaming client that hangs up mid-answer: the gateway stops
    /// forwarding but reads on to the provider's usage chunk, so the row
    /// carries what the provider billed.
    #[tokio::test]
    async fn a_stream_whose_client_hangs_up_mid_answer_still_writes_its_row() {
        async fn slow_stream() -> impl IntoResponse {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(4);
            tokio::spawn(async move {
                let first = "data: {\"id\":\"cmpl-5\",\"choices\":[{\"delta\":{\"content\":\"PARTIAL\"}}]}\n\n";
                let _ = tx.send(Ok(first.to_string())).await;
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                let rest = concat!(
                    "data: {\"id\":\"cmpl-5\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,",
                    "\"completion_tokens\":2,\"prompt_cache_hit_tokens\":4}}\n\n",
                    "data: [DONE]\n\n"
                );
                let _ = tx.send(Ok(rest.to_string())).await;
            });
            let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                axum::body::Body::from_stream(stream),
            )
        }
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(slow_stream))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        post_and_hang_up(
            gateway,
            r#"{"model":"deepseek-flash","stream":true,"messages":[]}"#,
            "PARTIAL",
        )
        .await;

        let rows = rows_eventually(&ledger_path, 1).await;
        assert_eq!(rows.len(), 1, "the billed stream has its row");
        assert_eq!(rows[0].usage.input, 10);
        assert_eq!(rows[0].usage.output, 2);
    }

    /// Notes, when it is dropped, how many rows the ledger held then.
    struct RowsAtDrop {
        ledger: PathBuf,
        seen: Arc<std::sync::Mutex<Option<usize>>>,
    }

    impl Drop for RowsAtDrop {
        fn drop(&mut self) {
            let (rows, _) = crate::ledger::read_entries_lenient(&self.ledger);
            *self.seen.lock().unwrap() = Some(rows.len());
        }
    }

    /// What a caller holds for a call (the ingress's reservation against an
    /// account cap) lasts until the call's row is written, also when the
    /// client hangs up while the provider is still answering. Dropped with
    /// the request, it was released before the provider had answered, while
    /// the call went on and billed.
    #[tokio::test]
    async fn a_callers_hold_outlasts_a_client_that_hangs_up_until_the_row_is_written() {
        async fn slow() -> Json<Value> {
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            complete().await
        }
        async fn slow_stream() -> impl IntoResponse {
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(4);
            tokio::spawn(async move {
                let first = "data: {\"id\":\"cmpl-6\",\"choices\":[{\"delta\":{\"content\":\"PARTIAL\"}}]}\n\n";
                let _ = tx.send(Ok(first.to_string())).await;
                tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                let rest = concat!(
                    "data: {\"id\":\"cmpl-6\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,",
                    "\"completion_tokens\":2}}\n\n",
                    "data: [DONE]\n\n"
                );
                let _ = tx.send(Ok(rest.to_string())).await;
            });
            let stream = futures::stream::poll_fn(move |cx| rx.poll_recv(cx));
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/event-stream")],
                axum::body::Body::from_stream(stream),
            )
        }
        let cases = [
            (
                Router::new().route("/v1/chat/completions", post(slow)),
                r#"{"model":"deepseek-flash","messages":[]}"#,
                "",
            ),
            (
                Router::new().route("/v1/chat/completions", post(slow_stream)),
                r#"{"model":"deepseek-flash","stream":true,"messages":[]}"#,
                "PARTIAL",
            ),
        ];
        for (provider, body, wait_for) in cases {
            let upstream = spawn(provider).await;
            let dir = tempfile::tempdir().unwrap();
            let ledger_path = dir.path().join("ledger.jsonl");
            let state = gateway_state(upstream, ledger_path.clone());
            let seen = Arc::new(std::sync::Mutex::new(None));
            let held = {
                let (ledger, seen) = (ledger_path.clone(), Arc::clone(&seen));
                move |headers: HeaderMap, body: String| {
                    let guard = RowsAtDrop { ledger, seen };
                    complete_holding(state, headers, body, Some(Box::new(guard)))
                }
            };
            let gateway = spawn(Router::new().route("/v1/chat/completions", post(held))).await;

            post_and_hang_up(gateway, body, wait_for).await;

            let mut dropped = None;
            for _ in 0..100 {
                dropped = *seen.lock().unwrap();
                if dropped.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            assert_eq!(dropped, Some(1), "held until the row was written: {body}");
        }
    }

    /// One susi gateway in front of another: the back one serves a streamed
    /// call from its cache, books it at zero and marks the answer. The front
    /// one read the mark on a buffered answer only, and the replayed stream
    /// carries a usage chunk, so the front booked the hit at full price.
    #[tokio::test]
    async fn a_cache_hit_streamed_through_a_second_gateway_costs_nothing_there_either() {
        let provider = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let dir = tempfile::tempdir().unwrap();
        let (back_ledger, front_ledger) = (
            dir.path().join("back.jsonl"),
            dir.path().join("front.jsonl"),
        );
        let mut back = Arc::try_unwrap(gateway_state(provider, back_ledger.clone()))
            .unwrap_or_else(|_| unreachable!());
        back.cache = Some(Arc::new(ResponseCache::new(None)));
        let back = spawn(router(Arc::new(back))).await;
        let front = spawn(router(gateway_state(back, front_ledger.clone()))).await;
        let buffered =
            r#"{"model":"deepseek-flash","messages":[{"role":"user","content":"ping"}]}"#;
        let streamed = buffered.replacen('{', r#"{"stream":true,"#, 1);

        // A buffered call through the back gateway warms its cache.
        let (status, _) = post_json(back, buffered, &[]).await;
        assert_eq!(status, 200);
        let (status, answer) = post_json(front, &streamed, &[]).await;

        assert_eq!(status, 200, "{answer}");
        assert!(answer.contains("data: [DONE]"), "{answer}");
        let back_rows = read_entries(&back_ledger).unwrap();
        assert_eq!(back_rows.len(), 2);
        assert!(back_rows[1].cached);
        let front_rows = read_entries(&front_ledger).unwrap();
        assert_eq!(front_rows.len(), 1);
        assert!(front_rows[0].cached, "the front saw the back's mark");
        assert_eq!(front_rows[0].cost, MicroUsd::ZERO);
        assert!(
            front_rows[0].usage.input > 0,
            "the tokens are still recorded"
        );
    }

    /// A call waiting for a paced slot whose client gives up gives the slot
    /// back. Kept, abandoned slots filled the minute while every live call
    /// queued behind them past its own client's timeout.
    #[tokio::test]
    async fn a_call_abandoned_while_it_waits_for_its_slot_gives_the_slot_back() {
        let provider = "pacer-abandon-test";
        let one_a_minute = Some(crate::ratelimit::Limits {
            rpm: Some(1),
            tpm: None,
        });
        let pacer = crate::ratelimit::Pacer::global();
        assert_eq!(pacer.reserve(provider, now_ms(), one_a_minute), 0);
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let mut state = Arc::try_unwrap(gateway_state(upstream, ledger_path.clone()))
            .unwrap_or_else(|_| unreachable!());
        // A model the registry does not know runs on the gateway's provider.
        state.provider = provider.into();
        // The minute's wait is inside the queue's cap, so the call queues.
        state.timeout_secs = 120;
        let gateway = spawn(router(Arc::new(state))).await;

        post_and_hang_up(
            gateway,
            r#"{"model":"pacer-abandon-model","messages":[]}"#,
            "",
        )
        .await;

        let mut held = pacer.reserved(provider);
        for _ in 0..40 {
            if held == 1 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            held = pacer.reserved(provider);
        }
        assert_eq!(held, 1, "only the call that ran still holds a slot");
        assert!(!ledger_path.exists(), "the abandoned call never ran");
    }

    /// A call whose paced slot is further off than the gateway waits is
    /// answered 429 at once, with the wait as `Retry-After`: it holds no
    /// slot, calls nothing upstream and writes no row. Queued, such calls ran
    /// the queue past every client's timeout under sustained overload, and
    /// the provider served nothing while the load lasted.
    #[tokio::test]
    async fn a_call_whose_paced_slot_is_past_the_cap_is_answered_429_with_retry_after() {
        let provider = "pacer-cap-test";
        let one_a_minute = Some(crate::ratelimit::Limits {
            rpm: Some(1),
            tpm: None,
        });
        let pacer = crate::ratelimit::Pacer::global();
        assert_eq!(pacer.reserve(provider, now_ms(), one_a_minute), 0);
        let upstream_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = Arc::clone(&upstream_calls);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                complete()
            }),
        );
        let upstream = spawn(app).await;
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let mut state = Arc::try_unwrap(gateway_state(upstream, ledger_path.clone()))
            .unwrap_or_else(|_| unreachable!());
        // A model the registry does not know runs on the gateway's provider;
        // the gateway waits 10 s for it, and its next slot is a minute away.
        state.provider = provider.into();
        let gateway = spawn(router(Arc::new(state))).await;

        let url = format!("http://{gateway}/v1/chat/completions");
        let (status, retry_after, body) = tokio::task::spawn_blocking(move || {
            let call = susi_http_transport::http_call_with_body(
                "POST",
                &url,
                &[],
                Some(br#"{"model":"pacer-cap-model","messages":[]}"#),
                10,
                0,
            )
            .unwrap();
            let retry_after = call.header("retry-after").map(str::to_string);
            let status = call.status;
            (
                status,
                retry_after,
                call.into_utf8(MAX_RESPONSE_BYTES).unwrap(),
            )
        })
        .await
        .unwrap();

        assert_eq!(status, 429, "{body}");
        let seconds: u64 = retry_after.expect("a Retry-After").parse().unwrap();
        assert!((50..=60).contains(&seconds), "Retry-After {seconds}");
        let error: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(error["error"]["code"], "rate_limit_exceeded");
        assert_eq!(pacer.reserved(provider), 1, "the call took no slot");
        assert_eq!(
            upstream_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "nothing went upstream"
        );
        assert!(!ledger_path.exists(), "and no row was written");
    }

    /// A client hangs up, then the gateway stops before the provider has
    /// answered (Ctrl-C reaching the agents and the gateway in one console).
    /// The runtime drops a detached task at its next await when it stops, but
    /// finishes a blocking thread it has started: settled in the task, the
    /// row of a call the provider billed was lost.
    #[test]
    fn a_buffered_call_is_settled_when_the_gateway_stops_after_its_client_hung_up() {
        let asked = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::clone(&asked);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move || {
                seen.store(true, std::sync::atomic::Ordering::SeqCst);
                async {
                    tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                    complete().await
                }
            }),
        );
        // The provider is not the gateway: it answers after the gateway stops.
        let provider = tokio::runtime::Runtime::new().unwrap();
        let upstream = provider.block_on(spawn(app));
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let gateway_runtime = tokio::runtime::Runtime::new().unwrap();
        let gateway =
            gateway_runtime.block_on(spawn(router(gateway_state(upstream, ledger_path.clone()))));

        gateway_runtime.block_on(post_and_hang_up(
            gateway,
            r#"{"model":"deepseek-flash","messages":[]}"#,
            "",
        ));
        for _ in 0..100 {
            if asked.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            asked.load(std::sync::atomic::Ordering::SeqCst),
            "the call went upstream"
        );
        drop(gateway_runtime);

        let (rows, _) = crate::ledger::read_entries_lenient(&ledger_path);
        assert_eq!(rows.len(), 1, "the billed call has its row");
        assert_eq!(rows[0].cost.micros(), 217_200);
        assert_eq!(rows[0].outcome, Outcome::Completed);
    }
}
