//! Agent Client Protocol (ACP) client: drives any ACP agent (Gemini CLI
//! `--experimental-acp`, Zed's Claude/Codex adapters, OpenCode `acp`, …) over
//! newline-delimited JSON-RPC 2.0 on stdio.
//!
//! The driver is generic over the byte streams so the protocol is tested with
//! a scripted agent; [`execute`] wires it to a supervised child process.
//! Client capabilities are declared empty (no fs/terminal): the agent works
//! in its own sandbox and any client-method request gets `-32601`. Tool
//! permission requests are denied unless the host opted in.
use super::{AgentManager, RunRecord, RunStatus};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::mpsc;
use std::time::Duration;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

const PROTOCOL_VERSION: u64 = 1;
/// Env opt-in: approve `allow_once` tool permission requests headlessly.
pub const AUTO_APPROVE_ENV: &str = "SUSI_ACP_AUTO_APPROVE";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Deny,
    AllowOnce,
}

impl Permission {
    pub fn from_env() -> Self {
        match crate::susi_config::env_or_cloud_env(AUTO_APPROVE_ENV).as_deref() {
            Ok("1" | "true") => Self::AllowOnce,
            _ => Self::Deny,
        }
    }
}

/// What to ask the agent and how to answer its permission requests.
#[derive(Debug, Clone, Copy)]
pub struct Task<'a> {
    pub cwd: &'a str,
    pub prompt: &'a str,
    pub policy: Permission,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub stop_reason: String,
    pub text: String,
}

impl Outcome {
    pub fn status(&self) -> RunStatus {
        match self.stop_reason.as_str() {
            "end_turn" => RunStatus::Succeeded,
            "cancelled" => RunStatus::Cancelled,
            // max_tokens / max_turn_requests / refusal: the agent stopped without finishing.
            _ => RunStatus::Stopped,
        }
    }
}

fn send(w: &mut impl Write, v: &Value) -> Result<()> {
    let mut line = serde_json::to_vec(v)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

fn permission_reply(params: &Value, policy: Permission) -> Value {
    let wanted = match policy {
        Permission::AllowOnce => "allow_once",
        Permission::Deny => "reject_once",
    };
    let option = params
        .get("options")
        .and_then(Value::as_array)
        .and_then(|o| {
            o.iter()
                .find(|o| o.get("kind").and_then(Value::as_str) == Some(wanted))
        })
        .and_then(|o| o.get("optionId").and_then(Value::as_str));
    match option {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

/// Run one prompt to completion. `cancelled` is polled between messages; on
/// cancel a `session/cancel` notification is sent and the agent's own
/// `cancelled` stop reason is awaited.
pub fn drive<R, W>(
    reader: R,
    mut writer: W,
    task: &Task<'_>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Outcome>
where
    R: Read + Send + 'static,
    W: Write,
{
    let Task {
        cwd,
        prompt,
        policy,
    } = *task;
    let (tx, rx) = mpsc::channel::<std::io::Result<String>>();
    std::thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            if tx.send(line).is_err() {
                return;
            }
        }
    });

    let mut next_id = 0_u64;
    let mut request = |w: &mut W, method: &str, params: Value| -> Result<u64> {
        next_id += 1;
        send(
            w,
            &json!({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params}),
        )?;
        Ok(next_id)
    };

    let init_id = request(
        &mut writer,
        "initialize",
        json!({
            "protocolVersion": PROTOCOL_VERSION,
            "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
            "clientInfo": {"name": "susi", "title": "SUSI", "version": env!("CARGO_PKG_VERSION")},
        }),
    )?;
    let mut session: Option<String> = None;
    let mut new_id = 0;
    let mut prompt_id = 0;
    let mut text = String::new();
    let mut cancel_sent = false;

    loop {
        if !cancel_sent && cancelled() {
            if let Some(s) = &session {
                send(
                    &mut writer,
                    &json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": s}}),
                )?;
                cancel_sent = true;
            } else {
                bail!("cancelled before the ACP session was established");
            }
        }
        let line = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => line.context("read ACP agent output")?,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ACP agent closed its output before finishing the prompt")
            }
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue; // agents may print non-protocol banners; ignore them
        };
        let method = msg.get("method").and_then(Value::as_str);
        let id = msg.get("id").cloned();
        match (method, id) {
            (Some("session/update"), None) => {
                let update = msg.pointer("/params/update");
                if update
                    .and_then(|u| u.get("sessionUpdate"))
                    .and_then(Value::as_str)
                    == Some("agent_message_chunk")
                {
                    if let Some(t) = update
                        .and_then(|u| u.pointer("/content/text"))
                        .and_then(Value::as_str)
                    {
                        text.push_str(t);
                    }
                }
            }
            (Some("session/request_permission"), Some(id)) => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                send(
                    &mut writer,
                    &json!({"jsonrpc": "2.0", "id": id, "result": permission_reply(&params, policy)}),
                )?;
            }
            (Some(other), Some(id)) => send(
                &mut writer,
                &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("client does not implement {other}")}}),
            )?,
            (Some(_), None) => {}
            (None, Some(id)) => {
                if let Some(err) = msg.get("error") {
                    bail!(
                        "ACP agent error: {}",
                        err.get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                    );
                }
                let result = msg.get("result").cloned().unwrap_or(Value::Null);
                if id == json!(init_id) {
                    new_id = request(
                        &mut writer,
                        "session/new",
                        json!({"cwd": cwd, "mcpServers": []}),
                    )?;
                } else if id == json!(new_id) {
                    let sid = result
                        .get("sessionId")
                        .and_then(Value::as_str)
                        .context("session/new response missing sessionId")?
                        .to_string();
                    prompt_id = request(
                        &mut writer,
                        "session/prompt",
                        json!({"sessionId": sid, "prompt": [{"type": "text", "text": prompt}]}),
                    )?;
                    session = Some(sid);
                } else if id == json!(prompt_id) {
                    let stop_reason = result
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .context("session/prompt response missing stopReason")?
                        .to_string();
                    return Ok(Outcome { stop_reason, text });
                }
            }
            (None, None) => {}
        }
    }
}

pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let super::Adapter::Acp { program, args } = &run.adapter else {
        bail!("not an ACP adapter");
    };
    let program =
        super::catalog::resolve_program(program).context("ACP agent executable missing")?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(args)
        .current_dir(&run.workspace)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(manager.run_dir(&run.id)?.join("stderr.log"))?,
        )
        .envs(crate::susi_config::cloud_env_overlay());
    let mut child = super::process::spawn_owned(&mut cmd)?;
    run.pid = Some(child.id());
    manager.save(run)?;
    let stdin = child.take_stdin().context("ACP agent stdin")?;
    let stdout = child.take_stdout().context("ACP agent stdout")?;
    let id = run.id.clone();
    let outcome = drive(
        stdout,
        stdin,
        &Task {
            cwd: &run.workspace.to_string_lossy(),
            prompt: &run.prompt,
            policy: Permission::from_env(),
        },
        &|| manager.cancel_requested(&id).unwrap_or(false),
    );
    drop(child);
    run.pid = None;
    let outcome = outcome?;
    std::fs::write(manager.run_dir(&run.id)?.join("stdout.log"), &outcome.text)?;
    run.status = outcome.status();
    if run.status != RunStatus::Succeeded {
        run.error = Some(format!("ACP stop reason: {}", outcome.stop_reason));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Scripted agent output; the driver's writes are captured and inspected.
    fn script(lines: &[Value]) -> Cursor<Vec<u8>> {
        let mut out = Vec::new();
        for l in lines {
            out.extend(serde_json::to_vec(l).unwrap());
            out.push(b'\n');
        }
        Cursor::new(out)
    }

    fn happy_script(extra: Vec<Value>) -> Cursor<Vec<u8>> {
        let mut lines = vec![
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s1"}}),
        ];
        lines.extend(extra);
        lines.push(json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}));
        script(&lines)
    }

    #[test]
    fn collects_message_chunks_until_end_turn() {
        let chunk = |t: &str| json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":t}}}});
        let mut sink = Vec::new();
        let out = drive(
            happy_script(vec![chunk("hel"), chunk("lo")]),
            &mut sink,
            &Task {
                cwd: "/w",
                prompt: "do it",
                policy: Permission::Deny,
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(
            out,
            Outcome {
                stop_reason: "end_turn".into(),
                text: "hello".into()
            }
        );
        assert_eq!(out.status(), RunStatus::Succeeded);
        let written = String::from_utf8(sink).unwrap();
        assert!(written.contains("\"method\":\"initialize\""));
        assert!(
            written.contains("\"method\":\"session/new\"") && written.contains("\"cwd\":\"/w\"")
        );
        assert!(written.contains("\"sessionId\":\"s1\"") && written.contains("\"text\":\"do it\""));
    }

    #[test]
    fn permission_denied_by_default_and_allowed_on_opt_in() {
        let req = json!({"jsonrpc":"2.0","id":"p1","method":"session/request_permission","params":{"options":[
            {"optionId":"a","name":"Allow","kind":"allow_once"},{"optionId":"r","name":"No","kind":"reject_once"}]}});
        for (policy, want) in [(Permission::Deny, "r"), (Permission::AllowOnce, "a")] {
            let mut sink = Vec::new();
            drive(
                happy_script(vec![req.clone()]),
                &mut sink,
                &Task {
                    cwd: "/w",
                    prompt: "x",
                    policy,
                },
                &|| false,
            )
            .unwrap();
            let written = String::from_utf8(sink).unwrap();
            assert!(
                written.contains(&format!("\"optionId\":\"{want}\"")),
                "{written}"
            );
        }
    }

    #[test]
    fn unimplemented_client_methods_get_method_not_found() {
        let req = json!({"jsonrpc":"2.0","id":9,"method":"fs/read_text_file","params":{}});
        let mut sink = Vec::new();
        drive(
            happy_script(vec![req]),
            &mut sink,
            &Task {
                cwd: "/w",
                prompt: "x",
                policy: Permission::Deny,
            },
            &|| false,
        )
        .unwrap();
        assert!(String::from_utf8(sink).unwrap().contains("-32601"));
    }

    #[test]
    fn non_end_turn_stop_reasons_never_report_success() {
        let script = script(&[
            json!({"jsonrpc":"2.0","id":1,"result":{}}),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s"}}),
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"refusal"}}),
        ]);
        let out = drive(
            script,
            Vec::new(),
            &Task {
                cwd: "/w",
                prompt: "x",
                policy: Permission::Deny,
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(out.status(), RunStatus::Stopped);
    }

    #[test]
    fn agent_error_and_early_close_are_failures() {
        let err =
            script(&[json!({"jsonrpc":"2.0","id":1,"error":{"code":-1,"message":"no auth"}})]);
        let e = drive(
            err,
            Vec::new(),
            &Task {
                cwd: "/w",
                prompt: "x",
                policy: Permission::Deny,
            },
            &|| false,
        )
        .unwrap_err();
        assert!(e.to_string().contains("no auth"));
        let closed = script(&[json!({"jsonrpc":"2.0","id":1,"result":{}})]);
        assert!(drive(
            closed,
            Vec::new(),
            &Task {
                cwd: "/w",
                prompt: "x",
                policy: Permission::Deny
            },
            &|| false
        )
        .is_err());
    }
}
