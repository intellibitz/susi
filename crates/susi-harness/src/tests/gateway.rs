use super::*;
use crate::http::{IntoResponse, Json};
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

/// Serve `app` for one test. Every answer closes its connection: the
/// transport pools connections per timeout for the whole test process,
/// and one kept open to a server a finished test left behind was handed
/// to a later test's call once a new server took that port, which then
/// failed with os error 10053 (at times a third of runs). The server reads
/// every request in full before it answers, since a connection closed with
/// part of a request unread is reset on Windows (os error 10054).
async fn spawn(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(crate::http::serve(
        listener,
        app.closing(),
        crate::http::Peer::Withhold,
        std::future::pending(),
    ));
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
async fn thinker(request: Request) -> Json<Value> {
    let request: Value = serde_json::from_slice(&request.body).unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
            post(move |request: Request| {
                let body = String::from_utf8(request.body).unwrap();
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
        let dir = susi_paths::tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let mut state = Arc::try_unwrap(gateway_state(upstream, ledger_path.clone()))
            .unwrap_or_else(|_| unreachable!());
        // A model the registry does not know runs on the gateway's provider.
        state.provider = provider.into();
        let state = Arc::new(state);
        let at_drop = Arc::new(std::sync::Mutex::new(None));
        let held = {
            let (ledger, seen) = (ledger_path.clone(), Arc::clone(&at_drop));
            move |request: Request| {
                let guard = RowsAtDrop {
                    ledger: ledger.clone(),
                    seen: Arc::clone(&seen),
                };
                let body = String::from_utf8(request.body).unwrap();
                complete_holding(
                    Arc::clone(&state),
                    request.headers,
                    body,
                    Some(Box::new(guard)),
                )
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
        post(|request: Request| async move {
            let body = String::from_utf8(request.body).unwrap();
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
        let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| complete()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| complete()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| streamed()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| complete()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| complete()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| failing()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger.jsonl");
    let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

    let (status, _) = post_json(gateway, r#"{"model":"deepseek-flash","messages":[]}"#, &[]).await;
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
    let wait = crate::ratelimit::Pacer::global().reserve("pause-test-provider", now_ms(), limits);
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
        spawn(Router::new().route("/v1/chat/completions", post(|_| without_usage()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger.jsonl");
    let gateway = spawn(router(gateway_state(upstream, ledger_path.clone()))).await;

    let (status, _) = post_json(gateway, r#"{"model":"deepseek-flash","messages":[]}"#, &[]).await;
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
    let temp = susi_paths::tempfile::tempdir().unwrap();
    let ledger = temp.path().join("ledger.jsonl");

    let app = Router::new().route(
        "/v1/chat/completions",
        post(|_| async {
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
    let temp = susi_paths::tempfile::tempdir().unwrap();
    let ledger = temp.path().join("ledger.jsonl");

    // Mock Anthropic upstream server expecting Anthropic Messages format
    let app = Router::new().route(
        "/v1/messages",
        post(|request: Request| async move {
            let body = String::from_utf8(request.body).unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| tool_call()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
        post(|request: Request| async move {
            let body = String::from_utf8(request.body).unwrap();
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| slow()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        tokio::spawn(async move {
            let first =
                "data: {\"id\":\"cmpl-5\",\"choices\":[{\"delta\":{\"content\":\"PARTIAL\"}}]}\n\n";
            let _ = tx.send(first.as_bytes().to_vec()).await;
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let rest = concat!(
                "data: {\"id\":\"cmpl-5\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,",
                "\"completion_tokens\":2,\"prompt_cache_hit_tokens\":4}}\n\n",
                "data: [DONE]\n\n"
            );
            let _ = tx.send(rest.as_bytes().to_vec()).await;
        });
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            Response::new(Body::Stream(rx)),
        )
    }
    let upstream =
        spawn(Router::new().route("/v1/chat/completions", post(|_| slow_stream()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
        let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
        tokio::spawn(async move {
            let first =
                "data: {\"id\":\"cmpl-6\",\"choices\":[{\"delta\":{\"content\":\"PARTIAL\"}}]}\n\n";
            let _ = tx.send(first.as_bytes().to_vec()).await;
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            let rest = concat!(
                "data: {\"id\":\"cmpl-6\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,",
                "\"completion_tokens\":2}}\n\n",
                "data: [DONE]\n\n"
            );
            let _ = tx.send(rest.as_bytes().to_vec()).await;
        });
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/event-stream")],
            Response::new(Body::Stream(rx)),
        )
    }
    let cases = [
        (
            Router::new().route("/v1/chat/completions", post(|_| slow())),
            r#"{"model":"deepseek-flash","messages":[]}"#,
            "",
        ),
        (
            Router::new().route("/v1/chat/completions", post(|_| slow_stream())),
            r#"{"model":"deepseek-flash","stream":true,"messages":[]}"#,
            "PARTIAL",
        ),
    ];
    for (provider, body, wait_for) in cases {
        let upstream = spawn(provider).await;
        let dir = susi_paths::tempfile::tempdir().unwrap();
        let ledger_path = dir.path().join("ledger.jsonl");
        let state = gateway_state(upstream, ledger_path.clone());
        let seen = Arc::new(std::sync::Mutex::new(None));
        let held = {
            let (ledger, seen) = (ledger_path.clone(), Arc::clone(&seen));
            move |request: Request| {
                let guard = RowsAtDrop {
                    ledger: ledger.clone(),
                    seen: Arc::clone(&seen),
                };
                let body = String::from_utf8(request.body).unwrap();
                complete_holding(
                    Arc::clone(&state),
                    request.headers,
                    body,
                    Some(Box::new(guard)),
                )
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
    let provider = spawn(Router::new().route("/v1/chat/completions", post(|_| complete()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let (back_ledger, front_ledger) = (
        dir.path().join("back.jsonl"),
        dir.path().join("front.jsonl"),
    );
    let mut back = Arc::try_unwrap(gateway_state(provider, back_ledger.clone()))
        .unwrap_or_else(|_| unreachable!());
    back.cache = Some(Arc::new(ResponseCache::new(None)));
    let back = spawn(router(Arc::new(back))).await;
    let front = spawn(router(gateway_state(back, front_ledger.clone()))).await;
    let buffered = r#"{"model":"deepseek-flash","messages":[{"role":"user","content":"ping"}]}"#;
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
    let upstream = spawn(Router::new().route("/v1/chat/completions", post(|_| complete()))).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
        post(move |_| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            complete()
        }),
    );
    let upstream = spawn(app).await;
    let dir = susi_paths::tempfile::tempdir().unwrap();
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
        post(move |_| {
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
    let dir = susi_paths::tempfile::tempdir().unwrap();
    let ledger_path = dir.path().join("ledger.jsonl");
    let gateway_runtime = tokio::runtime::Runtime::new().unwrap();
    let gateway =
        gateway_runtime.block_on(spawn(router(gateway_state(upstream, ledger_path.clone()))));

    gateway_runtime.block_on(post_and_hang_up(
        gateway,
        r#"{"model":"deepseek-flash","messages":[]}"#,
        "",
    ));
    // Up to 30 s: the call reaches the provider in milliseconds on an
    // idle machine, but the finish gate runs this beside 1 600 other
    // tests and a compile, where 5 s was not always enough (three
    // tries red, the gate red with them). The bound only needs to be
    // longer than the slowest scheduling; nothing here depends on it.
    for _ in 0..600 {
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
