//! The native in-process autonomous coding worker.
//!
//! A Rust multi-turn loop that talks to the metered gateway and dispatches
//! workspace operations through the tool registry and its admitted implementations.
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
use crate::smart::{
    classify_non_tool_turn, extract_reasoning_trace, worker_system_prompt, SmartAdvisor,
    SmartParams, TurnDisposition,
};

/// The optional gateway bearer belongs only to its configured origin. Resolve
/// the named grant for each outbound request so a retry sees rotation or denial.
pub(crate) fn gateway_auth(gateway: &str, run_key: &str) -> EaiResult<String> {
    if run_key.contains(['\r', '\n']) {
        return Err(EaiError::config("invalid gateway run identity"));
    }
    let name = std::env::var_os("SUSI_HARNESS_GATEWAY_KEY_ENV");
    let origin = std::env::var_os("SUSI_HARNESS_GATEWAY_ORIGIN");
    if name.is_none() && origin.is_none() {
        return Ok(format!("Bearer {run_key}"));
    }
    let origin = origin
        .as_deref()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .trim_end_matches('/');
    let local = origin
        .strip_prefix("http://")
        .and_then(|s| s.parse::<std::net::SocketAddr>().ok())
        .is_some_and(|addr| addr.ip().is_loopback());
    if gateway.trim_end_matches('/') != origin
        || (!local && crate::workload::origin(origin).is_none())
    {
        return Err(EaiError::config(
            "gateway account bearer requires its configured HTTPS or numeric loopback origin",
        ));
    }
    let key = name
        .as_deref()
        .and_then(|n| n.to_str())
        .and_then(crate::secrets::lookup)
        .ok_or_else(|| EaiError::config("configured gateway account credential unavailable"))?;
    if !key.expose().bytes().all(|b| b.is_ascii_graphic()) || key.expose().is_empty() {
        return Err(EaiError::config("invalid gateway account bearer"));
    }
    Ok(format!("Bearer {}", key.expose()))
}

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
    let attempt_str = config.attempt.to_string();

    let tools = crate::tools::Registry::builtin();
    let declarations = tools.declarations();
    let mut messages = vec![
        json!({
            "role": "system",
            "content": worker_system_prompt()
        }),
        json!({
            "role": "user",
            "content": config.prompt
        }),
    ];

    let mut turn = 0;
    let mut empty_streak = 0usize;
    let mut continuation_streak = 0u32;
    let mut reasoning_turns = 0u32;
    let mut accumulated_text = String::new();
    let mut advisor = SmartAdvisor::new();

    while turn < max_turns {
        turn += 1;
        if Instant::now() >= deadline {
            return Ok(RunOutcome::TimedOut);
        }

        // Synthesize phase-aware full-budget reasoning and cache-affinity
        // parameters (`max_tokens`, `reasoning_effort = "high"`, `thinking`,
        // and `x-susi-cache-scope`). No `temperature` is sent because Kimi and
        // Anthropic Messages reject non-default sampling parameters.
        let phase = advisor.recommend_phase(turn - 1, continuation_streak);
        let smart_params = SmartParams::for_turn(
            &config.model,
            &config.mission,
            &config.task,
            config.attempt,
            phase,
        );
        let mut request_body = json!({
            "model": config.model,
            "messages": messages,
            "tools": declarations,
        });
        smart_params.apply_to_request(&mut request_body);

        let payload_bytes = serde_json::to_vec(&request_body)
            .map_err(|e| EaiError::internal(format!("serialize request: {e}")))?;

        let mut resent = 0;
        let (status, buf) = loop {
            let auth_header = gateway_auth(&config.gateway_url, &run_key)?;
            let headers = [
                ("authorization", auth_header.as_str()),
                ("content-type", "application/json"),
                ("x-susi-mission", config.mission.as_str()),
                ("x-susi-task", config.task.as_str()),
                ("x-susi-attempt", attempt_str.as_str()),
                ("x-susi-cache-scope", smart_params.cache_scope.as_str()),
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
        if !extract_reasoning_trace(&message).is_empty() {
            reasoning_turns = reasoning_turns.saturating_add(1);
        }
        append_transcript(
            &config.state_dir,
            &json!({
                "turn": turn,
                "max_tokens": smart_params.max_tokens,
                "message": message
            }),
        );
        let tool_calls = message.get("tool_calls").and_then(Value::as_array);

        if let Some(calls) = tool_calls {
            if !calls.is_empty() {
                // Append assistant message with tool calls to conversation history
                messages.push(message.clone());
                empty_streak = 0;
                continuation_streak = 0;
                accumulated_text.clear();

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

                    let mut output = tools.execute(&config.workspace, func_name, func_args, &[]);
                    if let Some(nudge) = advisor.observe_tool_call(func_name, func_args, &output) {
                        output.push_str(&nudge);
                    }
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": call_id,
                        "content": output
                    }));
                }
                continue;
            }
        }

        match classify_non_tool_turn(choice, continuation_streak) {
            TurnDisposition::ContinueThinking {
                reasoning,
                partial_text,
                continuation_prompt,
            } => {
                continuation_streak = continuation_streak.saturating_add(1);
                if !partial_text.is_empty() {
                    accumulated_text.push_str(&partial_text);
                }
                let mut assistant_turn = json!({
                    "role": "assistant",
                    "content": partial_text
                });
                if !reasoning.is_empty() {
                    assistant_turn["reasoning_content"] = Value::from(reasoning);
                }
                messages.push(assistant_turn);
                messages.push(json!({
                    "role": "user",
                    "content": continuation_prompt
                }));
                continue;
            }
            TurnDisposition::Conclude {
                final_text: turn_text,
                ..
            } => {
                let final_text = if accumulated_text.is_empty() {
                    turn_text
                } else {
                    format!("{accumulated_text}{turn_text}")
                };
                let result_doc = json!({
                    "ok": true,
                    "final": final_text,
                    "turns": turn,
                    "reasoning_turns": reasoning_turns,
                    "tool_errors": advisor.total_tool_errors(),
                    "cache_scope": smart_params.cache_scope
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
            TurnDisposition::EmptyTurn => {
                // A reply with neither tool calls, length-truncated thinking,
                // nor prose is not a conclusion: nudge it and keep going
                // (bounded by max_turns and 3 empty turns).
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
            }
        }
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
    // Never spin on zero (`Retry-After: 0` waits 1 s); honor sub-second waits >= 100 ms.
    let wait_ms = retry_after_ms.map(|ms| if ms == 0 { 1_000 } else { ms.max(100) })?;
    let wait = std::time::Duration::from_millis(wait_ms);
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
#[path = "tests/worker.rs"]
mod tests;
