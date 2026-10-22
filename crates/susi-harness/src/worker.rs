//! The native in-process autonomous coding worker.
//!
//! Replaces external Node.js / OpenClaw runtime dependencies with a pure Rust
//! multi-turn agentic loop. Executes surgical file edits, workspace inspection,
//! and hermetic verification directly against the metered gateway.

use serde_json::{json, Value};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use susi_error::{EaiError, EaiResult};

use crate::executor::{ExecutorConfig, RunOutcome};

/// Declared tools exposed to the reasoning model.
pub fn tool_declarations() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "view_file",
                "description": "Read file lines from the workspace. Lines are 1-indexed. Provide optional start_line and end_line to view a line slice.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path to file in workspace" },
                        "start_line": { "type": "integer", "description": "1-based starting line number" },
                        "end_line": { "type": "integer", "description": "1-based ending line number" }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_to_file",
                "description": "Create or overwrite a file in the workspace with content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path to file in workspace" },
                        "content": { "type": "string", "description": "Complete file text content" }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "replace_file_content",
                "description": "Perform an exact surgical string replacement in a workspace file.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path to file in workspace" },
                        "target": { "type": "string", "description": "Exact text substring to match and replace" },
                        "replacement": { "type": "string", "description": "Replacement text" }
                    },
                    "required": ["path", "target", "replacement"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "run_command",
                "description": "Run a shell command inside the workspace directory (e.g. 'cargo check', 'cargo test').",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": { "type": "string", "description": "Command string to execute" }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_dir",
                "description": "List file and directory entries within a workspace directory.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path, or empty for root" }
                    }
                }
            }
        }
    ])
}

/// Safely resolve a path within the workspace root.
pub fn sanitize_path(workspace: &Path, rel: &str) -> Result<PathBuf, String> {
    let clean_rel = Path::new(rel);
    for component in clean_rel.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                return Err("absolute paths are not permitted; provide relative paths".to_string());
            }
            Component::ParentDir => {
                return Err("path traversal ('..') is not permitted".to_string());
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    Ok(workspace.join(clean_rel))
}

/// Execute a declared tool by name with JSON arguments.
pub fn execute_tool(workspace: &Path, name: &str, args_json: &str) -> String {
    let args: Value = match serde_json::from_str(args_json) {
        Ok(v) => v,
        Err(e) => return format!("error parsing tool arguments: {e}"),
    };

    match name {
        "view_file" => tool_view_file(workspace, &args),
        "write_to_file" => tool_write_to_file(workspace, &args),
        "replace_file_content" => tool_replace_file_content(workspace, &args),
        "run_command" => tool_run_command(workspace, &args),
        "list_dir" => tool_list_dir(workspace, &args),
        other => format!("unknown tool '{other}'"),
    }
}

fn tool_view_file(workspace: &Path, args: &Value) -> String {
    let path_str = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return "missing 'path' argument".to_string(),
    };
    let target_path = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !target_path.exists() {
        return format!("file '{}' does not exist", path_str);
    }
    let content = match fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(e) => return format!("failed to read '{}': {e}", path_str),
    };

    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or(1);
    let end_line = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);

    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    if total_lines == 0 {
        return "(empty file)".to_string();
    }

    let start_idx = start_line.saturating_sub(1).min(total_lines);
    let end_idx = end_line.unwrap_or(total_lines).min(total_lines);

    let mut out = String::new();
    for (i, line) in lines[start_idx..end_idx].iter().enumerate() {
        let line_num = start_idx + i + 1;
        out.push_str(&format!("{line_num:>5}: {line}\n"));
    }
    out
}

fn tool_write_to_file(workspace: &Path, args: &Value) -> String {
    let path_str = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return "missing 'path' argument".to_string(),
    };
    let content = match args.get("content").and_then(Value::as_str) {
        Some(c) => c,
        None => return "missing 'content' argument".to_string(),
    };
    let target_path = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if let Some(parent) = target_path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            return format!("failed to create parent directories: {e}");
        }
    }
    match fs::write(&target_path, content) {
        Ok(()) => format!(
            "successfully wrote {} bytes to '{}'",
            content.len(),
            path_str
        ),
        Err(e) => format!("failed to write to '{}': {e}", path_str),
    }
}

fn tool_replace_file_content(workspace: &Path, args: &Value) -> String {
    let path_str = match args.get("path").and_then(Value::as_str) {
        Some(p) => p,
        None => return "missing 'path' argument".to_string(),
    };
    let target = match args.get("target").and_then(Value::as_str) {
        Some(t) => t,
        None => return "missing 'target' argument".to_string(),
    };
    let replacement = match args.get("replacement").and_then(Value::as_str) {
        Some(r) => r,
        None => return "missing 'replacement' argument".to_string(),
    };
    let target_path = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !target_path.exists() {
        return format!("file '{}' does not exist", path_str);
    }
    let content = match fs::read_to_string(&target_path) {
        Ok(c) => c,
        Err(e) => return format!("failed to read '{}': {e}", path_str),
    };

    let count = content.matches(target).count();
    if count == 0 {
        return format!("target substring not found in '{}'", path_str);
    }
    if count > 1 {
        return format!(
            "target matches {count} occurrences in '{}'; make target more specific to match exactly once",
            path_str
        );
    }

    let updated = content.replace(target, replacement);
    match fs::write(&target_path, updated) {
        Ok(()) => format!("successfully replaced target in '{}'", path_str),
        Err(e) => format!("failed to write '{}': {e}", path_str),
    }
}

fn tool_run_command(workspace: &Path, args: &Value) -> String {
    let cmd_str = match args.get("command").and_then(Value::as_str) {
        Some(c) => c,
        None => return "missing 'command' argument".to_string(),
    };

    let mut command = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.arg("-NoProfile").arg("-Command").arg(cmd_str);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd_str);
        c
    };
    command.current_dir(workspace);

    match command.output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let code = out.status.code().unwrap_or(-1);

            let mut combined = String::new();
            if !stdout.is_empty() {
                combined.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str("[stderr]\n");
                combined.push_str(&stderr);
            }
            // Cap output at 32KB
            if combined.len() > 32 * 1024 {
                let tail = &combined[combined.len() - 32 * 1024..];
                format!("exit code: {code}\n[truncated...]\n{tail}")
            } else {
                format!("exit code: {code}\n{combined}")
            }
        }
        Err(e) => format!("failed to spawn command '{cmd_str}': {e}"),
    }
}

fn tool_list_dir(workspace: &Path, args: &Value) -> String {
    let path_str = args
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let target_dir = match sanitize_path(workspace, path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if !target_dir.exists() {
        return format!("directory '{}' does not exist", path_str);
    }
    let read_dir = match fs::read_dir(&target_dir) {
        Ok(rd) => rd,
        Err(e) => return format!("failed to read directory: {e}"),
    };

    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        if is_dir {
            entries.push(format!("{name}/"));
        } else {
            entries.push(name);
        }
    }
    entries.sort();
    if entries.is_empty() {
        "(empty directory)".to_string()
    } else {
        entries.join("\n")
    }
}

/// Run an autonomous multi-turn agentic coding loop.
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

    let tools = tool_declarations();
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
    while turn < max_turns {
        turn += 1;
        if Instant::now() >= deadline {
            return Ok(RunOutcome::TimedOut);
        }

        let request_body = json!({
            "model": config.model,
            "messages": messages,
            "tools": tools,
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
        let tool_calls = message.get("tool_calls").and_then(Value::as_array);

        if let Some(calls) = tool_calls {
            if !calls.is_empty() {
                // Append assistant message with tool calls to conversation history
                messages.push(message.clone());

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

                    let output = execute_tool(&config.workspace, func_name, func_args);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_declarations_valid() {
        let decls = tool_declarations();
        assert!(decls.as_array().is_some());
        assert_eq!(decls.as_array().unwrap().len(), 5);
    }

    #[test]
    fn sanitize_path_blocks_escapes() {
        let ws = Path::new("test_ws");
        assert!(sanitize_path(ws, "../outside").is_err());
        assert!(sanitize_path(ws, "sub/../../outside").is_err());
        assert!(sanitize_path(ws, "safe/nested/file.rs").is_ok());
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
                        assert!(messages.iter().any(|m| m.get("role").and_then(Value::as_str) == Some("tool")));

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
            program: PathBuf::from("native"),
            gateway_url: format!("http://{addr}"),
            model: "test-model".into(),
            workspace: temp_ws.clone(),
            state_dir: temp_state.clone(),
            mission: "m-test".into(),
            task: "t-test".into(),
            attempt: 1,
            timeout: std::time::Duration::from_secs(30),
            stall_after: None,
            ledger_path: None,
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

    #[test]
    fn view_and_write_and_replace_work() {
        let temp = std::env::temp_dir().join(format!("susi-worker-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&temp);

        // write
        let write_res = execute_tool(
            &temp,
            "write_to_file",
            &json!({ "path": "test.txt", "content": "line1\nline2\nline3" }).to_string(),
        );
        assert!(write_res.contains("successfully wrote"));

        // view
        let view_res = execute_tool(
            &temp,
            "view_file",
            &json!({ "path": "test.txt", "start_line": 2, "end_line": 2 }).to_string(),
        );
        assert!(view_res.contains("line2"));
        assert!(!view_res.contains("line1"));

        // replace
        let rep_res = execute_tool(
            &temp,
            "replace_file_content",
            &json!({ "path": "test.txt", "target": "line2", "replacement": "line_two" })
                .to_string(),
        );
        assert!(rep_res.contains("successfully replaced"));

        // verify replaced
        let view_after = execute_tool(
            &temp,
            "view_file",
            &json!({ "path": "test.txt" }).to_string(),
        );
        assert!(view_after.contains("line_two"));

        // list_dir
        let list_res = execute_tool(&temp, "list_dir", "{}");
        assert!(list_res.contains("test.txt"));

        let _ = fs::remove_dir_all(&temp);
    }
}
