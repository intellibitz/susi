//! The metering gateway.
//!
//! Workers point their OpenAI-compatible base URL here. The gateway forwards a
//! **non-streaming** chat completion, reads the provider's own `usage` (cache
//! hits included), prices it from the catalogue, and appends a ledger row
//! before returning the answer — so a call that reached a provider is never
//! spend without a record.
//!
//! Streaming is refused in this phase: an unmetered stream is exactly the
//! spend the harness exists to make visible.

use crate::catalog::PriceCatalogue;
use crate::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
use crate::usage::token_usage_from_json;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
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
    if request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "streaming is not metered yet; send stream=false",
        );
    }
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
            model: model.to_string(),
            provider: state.provider.clone(),
            usage,
            price,
            started_unix_ms,
            ended_unix_ms,
            outcome: Outcome::Completed,
        });
        let ledger_path = state.ledger_path.clone();
        let appended =
            tokio::task::spawn_blocking(move || append_entry(&ledger_path, &entry)).await;
        match appended {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("ledger append failed: {error}"),
                );
            }
            Err(error) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    &format!("ledger task failed: {error}"),
                );
            }
        }
    }

    let mut response = Response::new(axum::body::Body::from(response_bytes));
    *response.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, value);
    }
    response
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
    async fn streaming_is_refused_without_a_row() {
        let dir = tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let upstream = spawn(Router::new().route("/v1/chat/completions", post(complete))).await;
        let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

        let (status, _) = post_json(
            gateway,
            r#"{"model":"deepseek-flash","stream":true,"messages":[]}"#,
            &[],
        )
        .await;
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
