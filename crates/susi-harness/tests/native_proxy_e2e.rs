#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)]

//! Offline end-to-end proof that the native ingress is a usable proxy:
//! authentication, a priced completion, a cached repeat at zero cost, a
//! per-key cap that refuses before dispatch, and `/v1/models` over the wire.
//!
//! A mock OpenAI-compatible upstream stands in for a provider, so the test is
//! deterministic and spends nothing. Every byte is written under a tempdir.

use axum::http::{header, StatusCode};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use susi_harness::cache::ResponseCache;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::gateway::GatewayState;
use susi_harness::ingress::{self, Account, Ingress};
use susi_harness::ledger::{read_entries, Outcome};
use susi_harness::money::MicroUsd;

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
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

async fn call(
    address: SocketAddr,
    method: &'static str,
    path: &'static str,
    token: Option<&'static str>,
    body: Option<&'static str>,
) -> (u16, String) {
    let url = format!("http://{address}{path}");
    let token = token.map(str::to_string);
    let body = body.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        let mut headers: Vec<(&str, String)> = Vec::new();
        if let Some(token) = token.as_deref() {
            headers.push(("authorization", format!("Bearer {token}")));
        }
        let forwarded: Vec<(&str, &str)> = headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let call = susi_http_transport::http_call_with_body(
            method,
            &url,
            &forwarded,
            body.as_deref().map(str::as_bytes),
            10,
            0,
        )
        .unwrap();
        let status = call.status;
        (status, call.into_utf8(MAX_RESPONSE_BYTES).unwrap())
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn native_proxy_ingress_is_priced_cached_and_capped() {
    let dir = tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger.jsonl");

    let calls = Arc::new(AtomicUsize::new(0));
    let upstream_calls = calls.clone();
    let app = Router::new().route(
        "/v1/chat/completions",
        post(move |_body: String| {
            let upstream_calls = upstream_calls.clone();
            async move {
                upstream_calls.fetch_add(1, Ordering::SeqCst);
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/json")],
                    json!({
                        "id": "cmpl-native-1",
                        "object": "chat.completion",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "native proxy reply"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
                    })
                    .to_string(),
                )
            }
        }),
    );
    let upstream = spawn(app).await;

    let cache = Arc::new(ResponseCache::new(Some(dir.path().to_path_buf())));
    let gateway = Arc::new(GatewayState {
        catalogue: catalogue(),
        ledger_path: ledger_path.clone(),
        upstream_chat_url: format!("http://{upstream}/v1/chat/completions"),
        provider: "deepseek".into(),
        api_key: "upstream-test-key".into(),
        timeout_secs: 10,
        max_output_tokens: None,
        cache: Some(cache),
    });
    let ingress = Arc::new(Ingress::new(vec![
        Account::new("sk-alice", "alice"),
        Account::new("sk-capped", "capped").with_budget(MicroUsd::from_micros(100)),
    ]));
    let proxy = spawn(ingress::router(gateway, ingress)).await;
    let body = r#"{"model":"deepseek-flash","messages":[{"role":"user","content":"ping"}]}"#;

    // (a) No bearer token: refused before anything is forwarded.
    let (status, _) = call(proxy, "POST", "/v1/chat/completions", None, Some(body)).await;
    assert_eq!(status, 401);

    // (b) An unknown token is refused just as clearly.
    let (status, _) = call(
        proxy,
        "POST",
        "/v1/chat/completions",
        Some("sk-unknown"),
        Some(body),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!ledger_path.exists());

    // (c) A valid key gets a completion, priced from the provider's usage.
    let (status, response) = call(
        proxy,
        "POST",
        "/v1/chat/completions",
        Some("sk-alice"),
        Some(body),
    )
    .await;
    assert_eq!(status, 200);
    assert!(response.contains("native proxy reply"), "{response}");
    let entries = read_entries(&ledger_path).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].mission, "alice");
    assert_eq!(entries[0].model, "deepseek-flash");
    assert_eq!(entries[0].provider, "deepseek");
    assert_eq!(entries[0].outcome, Outcome::Completed);
    assert!(!entries[0].cached);
    assert_eq!(entries[0].cost, MicroUsd::from_micros(5));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // (d) The same request hits the cache: one more row, zero cost, no call.
    let (status, response) = call(
        proxy,
        "POST",
        "/v1/chat/completions",
        Some("sk-alice"),
        Some(body),
    )
    .await;
    assert_eq!(status, 200);
    assert!(response.contains("native proxy reply"), "{response}");
    let entries = read_entries(&ledger_path).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries[1].cached);
    assert_eq!(entries[1].cost, MicroUsd::ZERO);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // (e) A cap below the worst-case estimate is refused, and writes no row.
    let (status, response) = call(
        proxy,
        "POST",
        "/v1/chat/completions",
        Some("sk-capped"),
        Some(body),
    )
    .await;
    assert_eq!(status, 402);
    assert!(response.contains("budget"), "{response}");
    assert_eq!(read_entries(&ledger_path).unwrap().len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // (f) The registry is listed with a token and refused without one.
    let (status, body) = call(proxy, "GET", "/v1/models", Some("sk-alice"), None).await;
    assert_eq!(status, 200);
    let payload: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(payload["object"], "list");
    let ids = payload["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|model| model["id"].as_str())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"deepseek-flash"), "{ids:?}");
    assert!(ids.contains(&"coder"), "{ids:?}");

    let (status, _) = call(proxy, "GET", "/v1/models", None, None).await;
    assert_eq!(status, 401);
}
