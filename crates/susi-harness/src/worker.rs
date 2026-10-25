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

        let request_body = json!({
            "model": config.model,
            "messages": messages,
            "tools": declarations,
            "temperature": 0.0,
            "max_tokens": 4096
        });

        let payload_bytes = serde_json::to_vec(&request_body)
            .map_err(|e| EaiError::internal(format!("serialize request: {e}")))?;

        let headers = [
            ("authorization", auth_header.as_str()),
            ("content-type", "application/json"),
            ("x-susi-mission", config.mission.as_str()),
            ("x-susi-task", config.task.as_str()),
            ("x-susi-attempt", attempt_str.as_str()),
        ];

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

        if call.status != 200 {
            return Ok(RunOutcome::Failed {
                message: format!("gateway answered HTTP {} on turn {turn}", call.status),
            });
        }

        let buf = call
            .into_bytes(16 * 1024 * 1024)
            .map_err(|e| EaiError::internal(format!("unreadable answer on turn {turn}: {e}")))?;

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
