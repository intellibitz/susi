//! The native in-process autonomous coding worker.
//!
//! A pure Rust multi-turn agentic loop: it executes surgical file edits,
//! workspace inspection, and hermetic verification directly against the metered
//! gateway, in process.
//!
//! The tool surface is the registry in [`crate::tools`], not a match arm here:
//! the worker declares what the registry holds and runs it through the same
//! policy gate every other caller uses.

use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::time::Instant;
use susi_error::{EaiError, EaiResult};

use crate::executor::{ExecutorConfig, RunOutcome};

/// Run an autonomous multi-turn agentic coding loop.
///
/// # Errors
///
/// Returns an error when the state directory cannot be created or a request or
/// answer cannot be encoded.
pub fn run_agent_loop(config: &ExecutorConfig, max_turns: usize) -> EaiResult<RunOutcome> {
    fs::create_dir_all(&config.state_dir).map_err(|error| {
        EaiError::io(format!(
            "create state dir {}: {error}",
            config.state_dir.display()
        ))
    })?;

    let prompt_path = config.state_dir.join("prompt.md");
    let _ = fs::write(&prompt_path, &config.prompt);

    let deadline = Instant::now() + config.timeout;
    let gateway_url = format!(
        "{}/v1/chat/completions",
        config.gateway_url.trim_end_matches('/')
    );
    let run_key = format!("susi:{}:{}:{}", config.mission, config.task, config.attempt);
    let auth_header = format!("Bearer {run_key}");
    let attempt_str = config.attempt.to_string();

    let tools = crate::tools::Registry::builtin();
    let declarations = tools.declarations();
    let mut messages = vec![
        json!({
            "role": "system",
            "content": "You are SUSI's native autonomous coding worker. Complete the assigned task directly in the workspace. Use tools to view files, make surgical edits, and verify changes with cargo check or cargo test. Conclude with a clear summary when verification passes."
        }),
        json!({
            "role": "user",
            "content": config.prompt
        }),
    ];

    let mut turn = 0;
    let mut empty_streak = 0usize;
    while turn < max_turns {
        turn += 1;
        if Instant::now() >= deadline {
            return Ok(RunOutcome::TimedOut);
        }

        let mut request_body = json!({
            "model": config.model,
            "messages": messages,
            "tools": declarations,
            "max_tokens": 8000
        });
        // No `temperature`: Kimi rejects anything but 1 with HTTP 400, and the
        // provider default is the only value every routed model accepts.
        //
        // Gemini 3.x spends its output budget on thinking first: at 4096 it
        // burned the whole allowance and answered with an empty message, which
        // read as a silent turn rather than a failure. Ask for low effort so the
        // answer fits; other providers are untouched.
        if config.model.starts_with("gemini") {
            request_body["reasoning_effort"] = Value::from("low");
        }

        let payload_bytes = serde_json::to_vec(&request_body)
            .map_err(|e| EaiError::internal(format!("serialize request: {e}")))?;

        let headers = [
            ("authorization", auth_header.as_str()),
            ("content-type", "application/json"),
            ("x-susi-mission", config.mission.as_str()),
            ("x-susi-task", config.task.as_str()),
            ("x-susi-attempt", attempt_str.as_str()),
        ];

        let mut resent = 0;
        let (status, buf) = loop {
            let call = match susi_http_transport::http_call_with_body(
                "POST",
                &gateway_url,
                &headers,
                Some(&payload_bytes),
                config.timeout.as_secs().min(300),
                0,
            ) {
                Ok(c) => c,
                Err(e) => {
                    return Ok(RunOutcome::Failed {
                        message: format!("gateway call failed on turn {turn}: {e}"),
                    });
                }
            };
            let status = call.status;
            let retry_after = call
                .header("retry-after")
                .and_then(crate::ratelimit::retry_after_ms);
            let buf = call.into_bytes(16 * 1024 * 1024).map_err(|e| {
                EaiError::internal(format!("unreadable answer on turn {turn}: {e}"))
            })?;
            match paced_wait(status, retry_after, &buf, deadline, resent) {
                Some(wait) => {
                    resent += 1;
                    std::thread::sleep(wait);
                }
                None => break (status, buf),
            }
        };

        if status != 200 {
            // The gateway's words go with the status: "HTTP 429" alone told
            // the ladder a provider was busy when its account was empty, and
            // nothing of why reached the failure record.
            let reason = error_message(&buf);
            return Ok(RunOutcome::Failed {
                message: match reason {
                    Some(reason) => {
                        format!("gateway answered HTTP {status} on turn {turn}: {reason}")
                    }
                    None => format!("gateway answered HTTP {status} on turn {turn}"),
                },
            });
        }

        let reply: Value = serde_json::from_slice(&buf)
            .map_err(|e| EaiError::internal(format!("unparseable answer on turn {turn}: {e}")))?;

        let choice = match reply.pointer("/choices/0") {
            Some(c) => c,
            None => {
                return Ok(RunOutcome::Failed {
                    message: format!("empty choices array from model on turn {turn}"),
                });
            }
        };

        let message = choice.get("message").cloned().unwrap_or_else(|| json!({}));
        append_transcript(
            &config.state_dir,
            &json!({"turn": turn, "message": message}),
        );
        let tool_calls = message.get("tool_calls").and_then(Value::as_array);

        if let Some(calls) = tool_calls {
            if !calls.is_empty() {
                // Append assistant message with tool calls to conversation history
                messages.push(message.clone());
                empty_streak = 0;

                for call_obj in calls {
                    let call_id = call_obj
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("call_default");
                    let func_name = call_obj
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let func_args = call_obj
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");

                    let output = tools.execute(&config.workspace, func_name, func_args, &[]);
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }
                continue;
            }
        }

        // Final text produced
        let final_text = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        // A reply with neither tool calls nor prose is not a conclusion: the
        // model produced no action at all. Nudge it and keep going (bounded by
        // max_turns) instead of reporting success on an empty turn, which
        // commits nothing and hides the failure from the ladder.
        if final_text.trim().is_empty() {
            empty_streak += 1;
            if empty_streak >= 3 {
                return Ok(RunOutcome::Failed {
                    message: format!(
                        "model produced no action for {empty_streak} consecutive turns"
                    ),
                });
            }
            messages.push(json!({"role": "assistant", "content": ""}));
            messages.push(json!({
                "role": "user",
                "content": "Your last reply contained neither a tool call nor a conclusion. Use a tool to make the change, run the acceptance command, then summarize the result."
            }));
            continue;
        }

        let result_doc = json!({
            "ok": true,
            "final": final_text,
            "turns": turn
        });
        let _ = fs::write(
            config.state_dir.join("result.json"),
            serde_json::to_string_pretty(&result_doc).unwrap_or_default(),
        );

        return Ok(RunOutcome::Succeeded {
            final_text,
            reported_cost_usd: None,
        });
    }

    Ok(RunOutcome::Failed {
        message: format!("exceeded max turns ({max_turns}) without concluding"),
    })
}

/// How often one turn is sent again after the gateway's pacing turned it
/// away. The attempt's deadline bounds the waits; this bounds the calls.
const PACED_RESENDS: usize = 8;

/// How long to wait before sending a turn again, when the answer (`status`,
/// its `Retry-After` in milliseconds, `body`) is the gateway's that the
/// provider's paced queue is full: 429 with a `Retry-After` (a provider's
/// own 429 reaches the worker without one). `None` when the answer is to be
/// read as it is, including when the wait would run past the attempt's
/// `deadline`.
///
/// The gateway queues a paced call only as long as its upstream timeout, and
/// answers a call whose slot is further off with that 429 at once. The
/// worker failed the whole attempt on it, losing the turns so far, and the
/// ladder's one same-model retry met the same full queue: with four workers
/// on a provider paced to one call a minute, the fourth task could fail
/// without a single call being served, where it had waited and been served
/// when the queue had no cap.
///
/// A 429 whose code is `insufficient_quota` is the gateway's answer for an
/// account that cannot pay ([`crate::gateway`]): its `Retry-After` is the
/// provider's cooldown, which waiting out changes nothing, so the attempt
/// fails at once and the ladder goes to another model.
fn paced_wait(
    status: u16,
    retry_after_ms: Option<u64>,
    body: &[u8],
    deadline: Instant,
    resent: usize,
) -> Option<std::time::Duration> {
    if status != 429 || resent >= PACED_RESENDS || has_error_code(body, "insufficient_quota") {
        return None;
    }
    let wait_ms = retry_after_ms?;
    // Never sooner than a second: a zero wait would only hammer the queue.
    let wait = std::time::Duration::from_millis(wait_ms.max(1_000));
    Instant::now()
        .checked_add(wait)
        .is_some_and(|resend_at| resend_at < deadline)
        .then_some(wait)
}

/// Whether an OpenAI-shaped error answer carries `code` as its `error.code`.
fn has_error_code(body: &[u8], code: &str) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value.pointer("/error/code")?.as_str().map(|c| c == code))
        .unwrap_or(false)
}

/// The `error.message` of an OpenAI-shaped error answer, or the body's first
/// line when it is not one; at most 240 characters, as the failure record
/// keeps. `None` for an empty body.
fn error_message(body: &[u8]) -> Option<String> {
    let text = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            String::from_utf8_lossy(body)
                .lines()
                .next()
                .unwrap_or_default()
                .to_string()
        });
    let text: String = text.trim().chars().take(240).collect();
    (!text.is_empty()).then_some(text)
}

/// Append one assistant message to the attempt transcript, so a loop is
/// diagnosable from artifacts rather than from a live replay.
fn append_transcript(state_dir: &Path, value: &Value) {
    use std::io::Write as _;
    let path = state_dir.join("transcript.jsonl");
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(&path) {
        if let Ok(mut line) = serde_json::to_string(value) {
            line.push('\n');
            let _ = file.write_all(line.as_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_declares_the_registry_tools() {
        let declarations = crate::tools::Registry::builtin().declarations();
        assert!(declarations.as_array().is_some());
        assert_eq!(declarations.as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn multi_turn_worker_executes_tool_call_and_concludes() {
        use axum::extract::Json as AxumJson;
        use axum::routing::post;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let turn_count = Arc::new(AtomicUsize::new(0));
        let count_clone = Arc::clone(&turn_count);

        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |AxumJson(body): AxumJson<Value>| {
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
            let _ = axum::serve(listener, app).await;
        });

        let temp_ws = std::env::temp_dir().join(format!("susi-worker-ws-{}", std::process::id()));
        let temp_state =
            std::env::temp_dir().join(format!("susi-worker-st-{}", std::process::id()));
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
        use axum::extract::Json as AxumJson;
        use axum::routing::post;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let turns = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&turns);
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |AxumJson(_body): AxumJson<Value>| {
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
            let _ = axum::serve(listener, app).await;
        });

        let temp_ws =
            std::env::temp_dir().join(format!("susi-worker-nudge-ws-{}", std::process::id()));
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
        use axum::http::{header, StatusCode};
        use axum::response::IntoResponse;
        use std::sync::atomic::Ordering;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&calls);
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            // The request body is read: a server that closes its connection
            // with part of a request unread can reset it on Windows, and the
            // client then sees os error 10053 instead of the answer.
            axum::routing::post(move |_body: String| {
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
                        axum::Json(json!({
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
            let _ = axum::serve(listener, app).await;
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
        let (addr, calls) = paced_gateway(1, "1").await;
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
            started.elapsed() >= std::time::Duration::from_millis(900),
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
        use axum::extract::Json as AxumJson;
        use axum::routing::post;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            post(move |AxumJson(_body): AxumJson<Value>| {
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
            let _ = axum::serve(listener, app).await;
        });

        let temp_ws =
            std::env::temp_dir().join(format!("susi-worker-loop-ws-{}", std::process::id()));
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
}
