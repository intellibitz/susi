use super::*;

#[test]
fn worker_declares_the_registry_tools() {
    let declarations = crate::tools::Registry::builtin().declarations();
    assert!(declarations.as_array().is_some());
    assert_eq!(declarations.as_array().unwrap().len(), 6);
}

#[tokio::test]
async fn multi_turn_worker_executes_tool_call_and_concludes() {
    use crate::http::post;
    use crate::http::Json as AxumJson;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let turn_count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&turn_count);

    let app = crate::http::Router::new().route(
            "/v1/chat/completions",
            post(move |request: crate::http::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                let cnt = count_clone.fetch_add(1, Ordering::SeqCst);
                async move {
                    if cnt == 0 {
                        AxumJson(json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "tool_calls": [{
                                        "id": "call_42",
                                        "type": "function",
                                        "function": {
                                            "name": "write_to_file",
                                            "arguments": "{\"path\": \"output.txt\", \"content\": \"native worker works\"}"
                                        }
                                    }]
                                }
                            }]
                        }))
                    } else {
                        // verify tool call result was passed back in messages
                        let messages = body.get("messages").and_then(Value::as_array).unwrap();
                        assert!(messages
                            .iter()
                            .any(|m| m.get("role").and_then(Value::as_str) == Some("tool")));

                        AxumJson(json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "content": "All edits completed and verified."
                                }
                            }]
                        }))
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = crate::http::serve(
            listener,
            app,
            crate::http::Peer::Withhold,
            std::future::pending(),
        )
        .await;
    });

    let temp_ws = std::env::temp_dir().join(format!("susi-worker-ws-{}", std::process::id()));
    let temp_state = std::env::temp_dir().join(format!("susi-worker-st-{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_ws);
    let _ = fs::create_dir_all(&temp_state);

    let config = ExecutorConfig {
        gateway_url: format!("http://{addr}"),
        model: "test-model".into(),
        workspace: temp_ws.clone(),
        state_dir: temp_state.clone(),
        mission: "m-test".into(),
        task: "t-test".into(),
        attempt: 1,
        timeout: std::time::Duration::from_secs(30),
        prompt: "Create output.txt".into(),
    };

    let outcome = tokio::task::spawn_blocking(move || run_agent_loop(&config, 10))
        .await
        .unwrap()
        .unwrap();

    match outcome {
        RunOutcome::Succeeded { final_text, .. } => {
            assert_eq!(final_text, "All edits completed and verified.");
        }
        other => panic!("expected succeeded, got {other:?}"),
    }

    // Verify that the tool actually wrote the file in the workspace
    let written = fs::read_to_string(temp_ws.join("output.txt")).unwrap();
    assert_eq!(written, "native worker works");

    // Verify result.json was persisted in state_dir
    assert!(temp_state.join("result.json").exists());

    let _ = fs::remove_dir_all(&temp_ws);
    let _ = fs::remove_dir_all(&temp_state);
}
#[tokio::test]
async fn empty_reply_is_nudged_by_the_worker() {
    use crate::http::post;
    use crate::http::Json as AxumJson;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let turns = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&turns);
    let app = crate::http::Router::new().route(
            "/v1/chat/completions",
            post(move |_| {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                async move {
                    match n {
                        0 => AxumJson(json!({
                            "choices": [{ "message": { "role": "assistant", "content": "" } }]
                        })),
                        1 => AxumJson(json!({
                            "choices": [{
                                "message": {
                                    "role": "assistant",
                                    "tool_calls": [{
                                        "id": "call_1",
                                        "type": "function",
                                        "function": {
                                            "name": "write_to_file",
                                            "arguments": "{\"path\": \"nudged.txt\", \"content\": \"nudged\"}"
                                        }
                                    }]
                                }
                            }]
                        })),
                        _ => AxumJson(json!({
                            "choices": [{ "message": { "role": "assistant", "content": "Wrote nudged.txt" } }]
                        })),
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = crate::http::serve(
            listener,
            app,
            crate::http::Peer::Withhold,
            std::future::pending(),
        )
        .await;
    });

    let temp_ws = std::env::temp_dir().join(format!("susi-worker-nudge-ws-{}", std::process::id()));
    let temp_state =
        std::env::temp_dir().join(format!("susi-worker-nudge-st-{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_ws);
    let _ = fs::create_dir_all(&temp_state);

    let config = ExecutorConfig {
        gateway_url: format!("http://{addr}"),
        model: "test-model".into(),
        workspace: temp_ws.clone(),
        state_dir: temp_state.clone(),
        mission: "m-nudge".into(),
        task: "t-nudge".into(),
        attempt: 1,
        timeout: std::time::Duration::from_secs(30),
        prompt: "Create nudged.txt".into(),
    };

    let outcome = tokio::task::spawn_blocking(move || run_agent_loop(&config, 10))
        .await
        .unwrap()
        .unwrap();

    match outcome {
        RunOutcome::Succeeded { final_text, .. } => {
            assert_eq!(final_text, "Wrote nudged.txt");
        }
        other => panic!("expected succeeded after a nudge, got {other:?}"),
    }
    assert!(
        turns.load(Ordering::SeqCst) >= 3,
        "the empty reply must not end the loop"
    );
    assert_eq!(
        fs::read_to_string(temp_ws.join("nudged.txt")).unwrap(),
        "nudged"
    );

    let _ = fs::remove_dir_all(&temp_ws);
    let _ = fs::remove_dir_all(&temp_state);
}

/// A stub gateway whose paced queue turns the first `paced` calls away
/// with 429 and `Retry-After: retry_after`, then concludes. Returns its
/// address and how many calls it took.
async fn paced_gateway(
    paced: usize,
    retry_after: &'static str,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    refusing_gateway(
        paced,
        retry_after,
        r#"{"error":{"message":"paced","code":"rate_limit_exceeded"}}"#,
    )
    .await
}

/// [`paced_gateway`] with the refusal's body as `refusal`.
async fn refusing_gateway(
    paced: usize,
    retry_after: &'static str,
    refusal: &'static str,
) -> (
    std::net::SocketAddr,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use crate::http::IntoResponse;
    use crate::http::{header, StatusCode};
    use std::sync::atomic::Ordering;
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&calls);
    let app = crate::http::Router::new().route(
        "/v1/chat/completions",
        // The request body is read: a server that closes its connection
        // with part of a request unread can reset it on Windows, and the
        // client then sees os error 10053 instead of the answer.
        crate::http::post(move |_| {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            // `connection: close`: the transport pools connections per
            // timeout for the whole test process, and one left open to
            // this server, gone when the test ends, failed a later
            // test's call once a new server took its port.
            async move {
                if n < paced {
                    return (
                        StatusCode::TOO_MANY_REQUESTS,
                        [
                            (header::RETRY_AFTER, retry_after),
                            (header::CONNECTION, "close"),
                        ],
                        refusal,
                    )
                        .into_response();
                }
                (
                    [(header::CONNECTION, "close")],
                    crate::http::Json(json!({
                        "choices": [{ "message": { "role": "assistant", "content": "Done." } }]
                    })),
                )
                    .into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = crate::http::serve(
            listener,
            app,
            crate::http::Peer::Withhold,
            std::future::pending(),
        )
        .await;
    });
    (addr, calls)
}

fn paced_config(addr: std::net::SocketAddr, name: &str, timeout_secs: u64) -> ExecutorConfig {
    let dir = |kind: &str| {
        std::env::temp_dir().join(format!("susi-worker-{name}-{kind}-{}", std::process::id()))
    };
    let (workspace, state_dir) = (dir("ws"), dir("st"));
    let _ = fs::create_dir_all(&workspace);
    let _ = fs::create_dir_all(&state_dir);
    ExecutorConfig {
        gateway_url: format!("http://{addr}"),
        model: "test-model".into(),
        workspace,
        state_dir,
        mission: "m-paced".into(),
        task: "t-paced".into(),
        attempt: 1,
        timeout: std::time::Duration::from_secs(timeout_secs),
        prompt: "x".into(),
    }
}

/// The gateway answers a call whose paced slot is further off than its
/// queue holds with 429 and a `Retry-After`. The worker failed the whole
/// attempt on it; it now sends the turn again once that time has passed.
#[tokio::test]
async fn a_turn_the_paced_queue_turned_away_is_sent_again_after_its_retry_after() {
    let (addr, calls) = paced_gateway(1, "0.15").await;
    // A call timeout no other test uses keeps the transport's pooled
    // connections of other tests' servers off this test's calls.
    let config = paced_config(addr, "paced", 31);
    let (workspace, state_dir) = (config.workspace.clone(), config.state_dir.clone());

    let started = Instant::now();
    let outcome = tokio::task::spawn_blocking(move || run_agent_loop(&config, 5))
        .await
        .unwrap()
        .unwrap();

    match outcome {
        RunOutcome::Succeeded { final_text, .. } => assert_eq!(final_text, "Done."),
        other => panic!("the paced turn was not sent again: {other:?}"),
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(120),
        "it waited for the Retry-After"
    );
    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&state_dir);
}

/// A wait that would run past the attempt's deadline is not slept: the
/// attempt fails at once, as a 429 the ladder reads as transient.
#[tokio::test]
async fn a_paced_wait_past_the_deadline_fails_the_attempt_at_once() {
    let (addr, calls) = paced_gateway(usize::MAX, "120").await;
    let config = paced_config(addr, "deadline", 6);
    let (workspace, state_dir) = (config.workspace.clone(), config.state_dir.clone());

    let started = Instant::now();
    let outcome = tokio::task::spawn_blocking(move || run_agent_loop(&config, 5))
        .await
        .unwrap()
        .unwrap();

    match outcome {
        RunOutcome::Failed { message } => assert!(message.contains("429"), "{message}"),
        other => panic!("expected the 429 to fail the attempt, got {other:?}"),
    }
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(started.elapsed() < std::time::Duration::from_secs(6));
    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&state_dir);
}

/// The gateway answers a provider whose account cannot pay with 429
/// `insufficient_quota` and the cooldown as `Retry-After`. That wait
/// changes nothing, so it is not slept: the attempt fails at once, as a
/// 429 the ladder reads as transient, with the gateway's words in the
/// message so the failure record says why.
#[tokio::test]
async fn an_unfunded_answer_fails_the_attempt_at_once_with_its_reason() {
    let (addr, calls) = refusing_gateway(
            usize::MAX,
            "1",
            r#"{"error":{"message":"anthropic cannot pay for claude-sonnet right now (anthropic answered HTTP 400: Your credit balance is too low)","type":"insufficient_quota","code":"insufficient_quota"}}"#,
        )
        .await;
    let config = paced_config(addr, "unfunded", 32);
    let (workspace, state_dir) = (config.workspace.clone(), config.state_dir.clone());

    let started = Instant::now();
    let outcome = tokio::task::spawn_blocking(move || run_agent_loop(&config, 5))
        .await
        .unwrap()
        .unwrap();

    match outcome {
        RunOutcome::Failed { message } => {
            assert!(message.contains("HTTP 429"), "{message}");
            assert!(message.contains("credit balance is too low"), "{message}");
            assert_eq!(
                crate::policy::FailureKind::classify(&message),
                crate::policy::FailureKind::Transient,
                "{message}"
            );
        }
        other => panic!("expected the refusal to fail the attempt, got {other:?}"),
    }
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "not sent again"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(900),
        "the Retry-After was not waited out"
    );
    let _ = fs::remove_dir_all(&workspace);
    let _ = fs::remove_dir_all(&state_dir);
}

#[test]
fn a_gateway_errors_words_are_read_from_its_body() {
    assert_eq!(
        error_message(br#"{"error":{"message":"  paced  ","code":"rate_limit_exceeded"}}"#),
        Some("paced".to_string())
    );
    assert_eq!(
        error_message(
            b"plain text first line
second"
        ),
        Some("plain text first line".to_string())
    );
    assert_eq!(error_message(b""), None);
    assert_eq!(error_message(b"   "), None);
    assert!(has_error_code(
        br#"{"error":{"code":"insufficient_quota"}}"#,
        "insufficient_quota"
    ));
    assert!(!has_error_code(
        br#"{"error":{"code":"rate_limit_exceeded"}}"#,
        "insufficient_quota"
    ));
    assert!(!has_error_code(b"not json", "insufficient_quota"));
}

#[tokio::test]
async fn repeated_empty_replies_fail_fast_by_the_worker() {
    use crate::http::post;
    use crate::http::Json as AxumJson;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let app = crate::http::Router::new().route(
        "/v1/chat/completions",
        post(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            async move {
                AxumJson(json!({
                    "choices": [{ "message": { "role": "assistant", "content": "" } }]
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = crate::http::serve(
            listener,
            app,
            crate::http::Peer::Withhold,
            std::future::pending(),
        )
        .await;
    });

    let temp_ws = std::env::temp_dir().join(format!("susi-worker-loop-ws-{}", std::process::id()));
    let temp_state =
        std::env::temp_dir().join(format!("susi-worker-loop-st-{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_ws);
    let _ = fs::create_dir_all(&temp_state);

    let config = ExecutorConfig {
        gateway_url: format!("http://{addr}"),
        model: "test-model".into(),
        workspace: temp_ws.clone(),
        state_dir: temp_state.clone(),
        mission: "m-loop".into(),
        task: "t-loop".into(),
        attempt: 1,
        timeout: std::time::Duration::from_secs(30),
        prompt: "x".into(),
    };

    let outcome = tokio::task::spawn_blocking(move || run_agent_loop(&config, 25))
        .await
        .unwrap()
        .unwrap();

    match outcome {
        RunOutcome::Failed { message } => assert!(message.contains("no action"), "{message}"),
        other => panic!("expected a fast failure, got {other:?}"),
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "three empty replies must end the attempt"
    );
    assert!(
        temp_state.join("transcript.jsonl").exists(),
        "the attempt must leave a transcript"
    );
    let _ = fs::remove_dir_all(&temp_ws);
    let _ = fs::remove_dir_all(&temp_state);
}

#[tokio::test]
async fn infinite_thinking_continues_across_length_turns_with_full_budget_reasoning() {
    use crate::http::post;
    use crate::http::Json as AxumJson;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let app = crate::http::Router::new().route(
        "/v1/chat/completions",
        post(move |request: crate::http::Request| {
            let headers = request.headers.clone();
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            let n = counter.fetch_add(1, Ordering::SeqCst);
            async move {
                // Verify Full Budget Reasoning ("high", never "low") and Full Cache header
                assert_eq!(body["reasoning_effort"], "high");
                assert!(body["max_tokens"].as_u64().unwrap() >= 24_576);
                assert_eq!(
                    headers
                        .get("x-susi-cache-scope")
                        .and_then(|v| v.to_str().ok()),
                    Some("susi-cache:m-inf:t-inf:a1")
                );

                match n {
                    // Three consecutive length-truncated reasoning turns (which would
                    // have tripped the 3-empty-turn check without infinite thinking)
                    0..=2 => AxumJson(json!({
                        "choices": [{
                            "finish_reason": "length",
                            "message": {
                                "role": "assistant",
                                "content": "",
                                "reasoning_content": format!("Deep reasoning phase {n}...")
                            }
                        }]
                    })),
                    // Turn 3: partial prose truncated mid-sentence
                    3 => AxumJson(json!({
                        "choices": [{
                            "finish_reason": "length",
                            "message": {
                                "role": "assistant",
                                "content": "Completed deep analysis: ",
                                "reasoning_content": "Finalizing synthesis."
                            }
                        }]
                    })),
                    // Turn 4: seamless continuation of the final answer
                    _ => AxumJson(json!({
                        "choices": [{
                            "finish_reason": "stop",
                            "message": {
                                "role": "assistant",
                                "content": "all invariants verified."
                            }
                        }]
                    })),
                }
            }
        }),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = crate::http::serve(
            listener,
            app,
            crate::http::Peer::Withhold,
            std::future::pending(),
        )
        .await;
    });

    let temp_ws = std::env::temp_dir().join(format!("susi-worker-inf-ws-{}", std::process::id()));
    let temp_state =
        std::env::temp_dir().join(format!("susi-worker-inf-st-{}", std::process::id()));
    let _ = fs::create_dir_all(&temp_ws);
    let _ = fs::create_dir_all(&temp_state);

    let config = ExecutorConfig {
        gateway_url: format!("http://{addr}"),
        model: "gemini-pro".into(),
        workspace: temp_ws.clone(),
        state_dir: temp_state.clone(),
        mission: "m-inf".into(),
        task: "t-inf".into(),
        attempt: 1,
        timeout: std::time::Duration::from_secs(30),
        prompt: "Solve complex invariant".into(),
    };

    let outcome = tokio::task::spawn_blocking(move || run_agent_loop(&config, 15))
        .await
        .unwrap()
        .unwrap();

    match outcome {
        RunOutcome::Succeeded { final_text, .. } => {
            assert_eq!(
                final_text,
                "Completed deep analysis: all invariants verified."
            );
        }
        other => panic!("expected infinite thinking to succeed, got {other:?}"),
    }
    assert_eq!(calls.load(Ordering::SeqCst), 5);
    let result_doc: Value =
        serde_json::from_str(&fs::read_to_string(temp_state.join("result.json")).unwrap()).unwrap();
    assert_eq!(result_doc["reasoning_turns"], 4);

    let _ = fs::remove_dir_all(&temp_ws);
    let _ = fs::remove_dir_all(&temp_state);
}
