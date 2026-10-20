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
    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return error_response(StatusCode::BAD_REQUEST, "request body is not JSON");
    };
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

    let mission = header_tag(&headers, "x-susi-mission").unwrap_or_else(|| "unscoped".into());
    let task = header_tag(&headers, "x-susi-task").unwrap_or_else(|| "unscoped".into());
    let attempt = header_tag(&headers, "x-susi-attempt")
        .and_then(|value| value.parse::<u32>().ok())
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

    if streaming {
        stream_upstream(state, attempt, forwarded).await
    } else {
        buffered_upstream(state, attempt, forwarded).await
    }
}

fn with_usage_in_stream(request: &Value, original: &str) -> String {
    let mut request = request.clone();
    if let Value::Object(map) = &mut request {
        map.insert("stream_options".into(), json!({ "include_usage": true }));
    }
    serde_json::to_string(&request).unwrap_or_else(|_| original.to_string())
}

async fn buffered_upstream(state: Arc<GatewayState>, attempt: Attempt, body: String) -> Response {
    let Attempt {
        mission,
        task,
        attempt,
        model,
        price,
    } = attempt;
    let started_unix_ms = now_ms();
    let url = state.upstream_chat_url.clone();
    let api_key = state.api_key.clone();
    let timeout_secs = state.timeout_secs;
    let upstream = tokio::task::spawn_blocking(move || {
        let authorization = format!("Bearer {api_key}");
        let forwarded = [
            ("authorization", authorization.as_str()),
            ("content-type", "application/json"),
        ];
        susi_http_transport::http_call_with_body(
            "POST",
            &url,
            &forwarded,
            Some(body.as_bytes()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::read_entries;
    use std::net::SocketAddr;

    const CATALOGUE: &str = r#"{
        "schema": "susi/harness/prices/v1",
        "models": {
            "deepseek-flash": {"input_miss": 150000, "input_hit": 3000, "output": 600000}
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
}
