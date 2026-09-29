//! Outbound A2A client: delegates a task to any A2A-compliant agent found via
//! its agent card and follows it with `tasks/get` until terminal.
//!
//! Both wire dialects are handled. The 0.3 line uses `kind`-tagged parts and
//! lower-case kebab states (`input-required`); newer servers (including SUSI's
//! own `ra2a`-based server) use `TASK_STATE_*` / `ROLE_*` enums and untagged
//! parts. The dialect is picked from the card's `protocolVersion`, and
//! responses are parsed tolerantly.
use super::{Adapter, AgentManager, RunRecord, RunStatus};
use serde_json::{json, Value};
use std::time::Duration;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

const MAX_BODY: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// A2A 0.x: `kind` discriminators, kebab-case states, `user` role.
    Legacy,
    /// A2A 1.x proto-JSON: `TASK_STATE_*`, `ROLE_USER`, untagged parts.
    Proto,
}

struct Remote {
    rpc_url: String,
    token: Option<String>,
    dialect: Dialect,
}

fn card_dialect(card: &Value) -> Dialect {
    match card.get("protocolVersion").and_then(Value::as_str) {
        Some(v) if v.starts_with("0.") => Dialect::Legacy,
        _ => Dialect::Proto,
    }
}

/// JSON-RPC endpoint the card advertises, or `base` when it names none.
fn card_rpc_url(card: &Value, base: &str) -> String {
    card.get("url")
        .and_then(Value::as_str)
        .or_else(|| {
            card.pointer("/supportedInterfaces/0/url")
                .and_then(Value::as_str)
        })
        .unwrap_or(base)
        .to_string()
}

fn build_send(dialect: Dialect, prompt: &str, message_id: &str) -> Value {
    let (role, part) = match dialect {
        Dialect::Legacy => ("user", json!({"kind": "text", "text": prompt})),
        Dialect::Proto => ("ROLE_USER", json!({"text": prompt})),
    };
    json!({"message": {"messageId": message_id, "role": role, "parts": [part]}})
}

/// Stable text of a task/message: agent artifacts first, else the status message.
fn extract_text(task: &Value) -> String {
    fn parts_text(parts: Option<&Value>) -> String {
        parts
            .and_then(Value::as_array)
            .map(|p| {
                p.iter()
                    .filter_map(|x| x.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default()
    }
    let from_artifacts: Vec<String> = task
        .get("artifacts")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|x| parts_text(x.get("parts"))).collect())
        .unwrap_or_default();
    let joined = from_artifacts
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if joined.is_empty() {
        parts_text(
            task.pointer("/status/message/parts")
                .or_else(|| task.get("parts")),
        )
    } else {
        joined
    }
}

fn map_state(state: &str) -> RunStatus {
    let s = state
        .trim_start_matches("TASK_STATE_")
        .to_ascii_lowercase()
        .replace('_', "-");
    match s.as_str() {
        "submitted" | "working" => RunStatus::Running,
        "input-required" | "auth-required" => RunStatus::Waiting,
        "completed" => RunStatus::Succeeded,
        "failed" => RunStatus::Failed,
        "canceled" | "cancelled" => RunStatus::Cancelled,
        // rejected is the agent declining; never certify it as success.
        "rejected" => RunStatus::Stopped,
        _ => RunStatus::Unknown,
    }
}

/// Unwrap a `message/send` or `tasks/get` result to the task object.
fn task_of(result: &Value) -> &Value {
    result.get("task").unwrap_or(result)
}

fn get_json(url: &str, token: Option<&str>) -> Result<Value> {
    let auth = token.map(|t| format!("Bearer {t}"));
    let mut headers = vec![("Accept", "application/json")];
    if let Some(a) = &auth {
        headers.push(("Authorization", a.as_str()));
    }
    let call = susi_http_transport::http_call("GET", url, &headers, 15, 3)
        .map_err(susi_error::EaiError::network)?;
    if call.status != 200 {
        bail!("HTTP {} from {url}", call.status);
    }
    let bytes = call.into_bytes(MAX_BODY).context("agent card body")?;
    Ok(serde_json::from_slice(&bytes)?)
}

impl Remote {
    fn resolve(adapter: &Adapter) -> Result<Self> {
        let Adapter::A2a { url, token_env } = adapter else {
            bail!("not an A2A adapter");
        };
        if !crate::susi_core::mac_policy::egress_permitted(url) {
            bail!("[PRIVACY] network egress to {url} blocked by the privacy posture (run `susi privacy consent --egress`)");
        }
        let token = token_env
            .as_deref()
            .map(|e| {
                crate::susi_config::env_or_cloud_env(e).with_context(|| format!("missing {e}"))
            })
            .transpose()?
            .filter(|t| !t.trim().is_empty());
        let base = url.trim_end_matches('/');
        let card = get_json(
            &format!("{base}/.well-known/agent-card.json"),
            token.as_deref(),
        )
        .or_else(|_| get_json(&format!("{base}/.well-known/agent.json"), token.as_deref()))
        .context("could not fetch the A2A agent card")?;
        Ok(Self {
            rpc_url: card_rpc_url(&card, base),
            dialect: card_dialect(&card),
            token,
        })
    }

    fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let body = serde_json::to_vec(
            &json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
        )?;
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if let Some(t) = &self.token {
            headers.push(("Authorization".into(), format!("Bearer {t}")));
        }
        let (status, text) =
            susi_http_transport::http_post_utf8(&self.rpc_url, &headers, &body, 60, MAX_BODY)
                .map_err(|e| {
                    susi_error::EaiError::internal(format!(
                        "A2A request failed; outcome may be unknown: {e}"
                    ))
                })?;
        if !(200..300).contains(&status) {
            bail!("A2A agent returned HTTP {status}");
        }
        let v: Value = serde_json::from_str(&text).context("invalid A2A JSON response")?;
        if let Some(err) = v.get("error") {
            bail!(
                "A2A error: {}",
                err.get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            );
        }
        v.get("result")
            .cloned()
            .context("A2A response missing result")
    }
}

fn remote_id(run: &RunRecord) -> Result<&str> {
    run.remote_id
        .as_deref()
        .context("no remote task ID; check the remote agent before resubmitting")
}

fn apply(manager: &AgentManager, run: &mut RunRecord, result: &Value) -> Result<()> {
    let task = task_of(result);
    let state = task
        .pointer("/status/state")
        .and_then(Value::as_str)
        .context("A2A response missing task status")?;
    run.status = map_state(state);
    std::fs::write(
        manager.run_dir(&run.id)?.join("stdout.log"),
        extract_text(task),
    )?;
    Ok(())
}

pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let remote = Remote::resolve(&run.adapter)?;
    let message_id = format!("susi-{}", run.id);
    let created = remote.rpc(
        "message/send",
        build_send(remote.dialect, &run.prompt, &message_id),
    )?;
    let task = task_of(&created);
    run.remote_id = Some(
        task.get("id")
            .and_then(Value::as_str)
            .context("A2A response missing task id (message-only reply?)")?
            .to_string(),
    );
    manager.save(run)?;
    apply(manager, run, &created)?;
    manager.save(run)?;
    while run.status == RunStatus::Running {
        if manager.cancel_requested(&run.id)? {
            cancel(run)?;
            run.status = RunStatus::Cancelled;
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
        refresh(manager, run)?;
        manager.save(run)?;
    }
    Ok(())
}

pub(super) fn refresh(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let remote = Remote::resolve(&run.adapter)?;
    let got = remote.rpc("tasks/get", json!({"id": remote_id(run)?}))?;
    apply(manager, run, &got)
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    Remote::resolve(&run.adapter)?.rpc("tasks/cancel", json!({"id": remote_id(run)?}))?;
    Ok(())
}

pub(super) fn send(run: &RunRecord, message: &str) -> Result<()> {
    let remote = Remote::resolve(&run.adapter)?;
    let mut params = build_send(remote.dialect, message, &format!("susi-{}-follow", run.id));
    params["message"]["taskId"] = json!(remote_id(run)?);
    remote.rpc("message/send", params)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dialect_follows_card_protocol_version() {
        assert_eq!(
            card_dialect(&json!({"protocolVersion": "0.3.0"})),
            Dialect::Legacy
        );
        assert_eq!(
            card_dialect(&json!({"protocolVersion": "1.0"})),
            Dialect::Proto
        );
        assert_eq!(card_dialect(&json!({})), Dialect::Proto);
    }

    #[test]
    fn send_payload_matches_each_dialect() {
        let legacy = build_send(Dialect::Legacy, "hi", "m");
        assert_eq!(legacy["message"]["role"], "user");
        assert_eq!(
            legacy["message"]["parts"][0],
            json!({"kind":"text","text":"hi"})
        );
        let proto = build_send(Dialect::Proto, "hi", "m");
        assert_eq!(proto["message"]["role"], "ROLE_USER");
        assert_eq!(proto["message"]["parts"][0], json!({"text":"hi"}));
    }

    #[test]
    fn rpc_url_prefers_card_then_interfaces_then_base() {
        assert_eq!(
            card_rpc_url(&json!({"url":"http://a/rpc"}), "http://b"),
            "http://a/rpc"
        );
        assert_eq!(
            card_rpc_url(
                &json!({"supportedInterfaces":[{"url":"http://c"}]}),
                "http://b"
            ),
            "http://c"
        );
        assert_eq!(card_rpc_url(&json!({}), "http://b"), "http://b");
    }

    #[test]
    fn states_map_in_both_spellings_and_never_fake_success() {
        for (s, want) in [
            ("completed", RunStatus::Succeeded),
            ("TASK_STATE_COMPLETED", RunStatus::Succeeded),
            ("input-required", RunStatus::Waiting),
            ("TASK_STATE_INPUT_REQUIRED", RunStatus::Waiting),
            ("working", RunStatus::Running),
            ("canceled", RunStatus::Cancelled),
            ("rejected", RunStatus::Stopped),
            ("failed", RunStatus::Failed),
            ("weird", RunStatus::Unknown),
        ] {
            assert_eq!(map_state(s), want, "{s}");
        }
    }

    #[test]
    fn text_comes_from_artifacts_then_status_message() {
        let with_art = json!({"artifacts":[{"parts":[{"text":"a"},{"kind":"text","text":"b"}]}],
            "status":{"message":{"parts":[{"text":"ignored"}]}}});
        assert_eq!(extract_text(&with_art), "ab");
        let status_only = json!({"status":{"message":{"parts":[{"text":"done"}]}}});
        assert_eq!(extract_text(&status_only), "done");
    }

    #[test]
    fn task_unwraps_the_v1_envelope() {
        let wrapped = json!({"task": {"id": "t"}});
        assert_eq!(task_of(&wrapped)["id"], "t");
        let bare = json!({"id": "t"});
        assert_eq!(task_of(&bare)["id"], "t");
    }
}

#[cfg(test)]
mod loopback {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Minimal HTTP/1.1 A2A server: card, `message/send` (working), then
    /// `tasks/get` (completed with an artifact). Returns its base URL.
    fn fake_agent() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let card = json!({"protocolVersion": "0.3.0", "url": format!("{base}/rpc")}).to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let mut raw = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = s.read(&mut buf).unwrap_or(0);
                    raw.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&raw).to_string();
                    let Some(split) = text.find("\r\n\r\n") else {
                        if n == 0 {
                            break;
                        } else {
                            continue;
                        }
                    };
                    let want = text[..split]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if raw.len() >= split + 4 + want || n == 0 {
                        break;
                    }
                }
                let req = String::from_utf8_lossy(&raw).to_string();
                let body = if req.starts_with("GET /.well-known/agent-card.json") {
                    card.clone()
                } else if req.contains("\"tasks/get\"") {
                    json!({"jsonrpc":"2.0","id":1,"result":{"id":"t1","status":{"state":"completed"},
                        "artifacts":[{"parts":[{"kind":"text","text":"answer"}]}]}}).to_string()
                } else if req.contains("\"message/send\"") {
                    assert!(
                        req.contains("\"kind\":\"text\""),
                        "legacy dialect expected: {req}"
                    );
                    json!({"jsonrpc":"2.0","id":1,"result":{"id":"t1","status":{"state":"working"}}}).to_string()
                } else {
                    String::new()
                };
                let status = if body.is_empty() {
                    "404 Not Found"
                } else {
                    "200 OK"
                };
                let _ = write!(
                    s,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        base
    }

    #[test]
    fn delegates_a_task_and_collects_the_remote_answer() {
        let root =
            std::env::temp_dir().join(format!("susi-a2a-{}", super::super::unique_id().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        let manager = AgentManager::with_config(
            &root,
            super::super::CatalogKind::Execution,
            root.join("cfg"),
        )
        .unwrap();
        let adapter = Adapter::A2a {
            url: fake_agent(),
            token_env: None,
        };
        let run = manager
            .prepare_adapter("a2a", "summarise", adapter)
            .unwrap();
        let done = manager.execute(&run.id).unwrap();
        assert_eq!(done.status, RunStatus::Succeeded, "{:?}", done.error);
        assert_eq!(done.remote_id.as_deref(), Some("t1"));
        let out =
            std::fs::read_to_string(manager.run_dir(&run.id).unwrap().join("stdout.log")).unwrap();
        assert_eq!(out, "answer");
        let _ = std::fs::remove_dir_all(&root);
    }
}
