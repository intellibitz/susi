//! OpenHands app-server / Cloud REST client (`/api/v1/app-conversations`).
//!
//! Complements the one-shot headless CLI executor with long-running sessions
//! that report status and can be paused. Endpoints and status values follow
//! the published V1 API (create → poll start-task until `READY` → poll the
//! conversation's `execution_status`). Follow-up messages and stop are not
//! documented for that API, so `send` is refused rather than guessed; cancel
//! pauses the conversation's sandbox, which is the documented control.
use super::{Adapter, AgentManager, RunRecord, RunStatus};
use serde_json::{json, Value};
use std::time::Duration;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

const MAX_BODY: u64 = 8 * 1024 * 1024;
const TASK_PREFIX: &str = "task:";

struct Server {
    base: String,
    key: Option<String>,
}

impl Server {
    fn new(adapter: &Adapter) -> Result<Self> {
        let Adapter::OpenHandsServer {
            base_url,
            api_key_env,
        } = adapter
        else {
            bail!("not an OpenHands server adapter");
        };
        if !crate::susi_core::mac_policy::egress_permitted(base_url) {
            bail!("[PRIVACY] network egress to {base_url} blocked by the privacy posture (run `susi privacy consent --egress`)");
        }
        let key = api_key_env
            .as_deref()
            .map(|e| {
                crate::susi_config::env_or_cloud_env(e).with_context(|| format!("missing {e}"))
            })
            .transpose()?
            .filter(|k| !k.trim().is_empty());
        Ok(Self {
            base: base_url.trim_end_matches('/').to_string(),
            key,
        })
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
        if let Some(k) = &self.key {
            headers.push(("Authorization".into(), format!("Bearer {k}")));
        }
        let refs: Vec<(&str, &str)> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let bytes = body.map(serde_json::to_vec).transpose()?;
        let call = susi_http_transport::http_call_with_body(
            method,
            &format!("{}{path}", self.base),
            &refs,
            bytes.as_deref(),
            30,
            0,
        )
        .map_err(|e| {
            susi_error::EaiError::internal(format!(
                "OpenHands request failed; outcome may be unknown: {e}"
            ))
        })?;
        // Provider bodies can echo prompts or credentials; report only the status.
        if !(200..300).contains(&call.status) {
            bail!("OpenHands returned HTTP {}", call.status);
        }
        let payload = call
            .into_bytes(MAX_BODY)
            .context("OpenHands response body")?;
        if payload.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&payload).context("invalid OpenHands JSON response")
    }
}

fn create_body(prompt: &str) -> Value {
    json!({"initial_message": {"content": [{"type": "text", "text": prompt}]}})
}

/// `GET …?ids=X` returns a list; tolerate a bare object too.
fn first(v: &Value) -> &Value {
    v.as_array().and_then(|a| a.first()).unwrap_or(v)
}

fn map_execution(status: &str) -> RunStatus {
    match status {
        "running" | "idle" => RunStatus::Running,
        "paused" | "waiting_for_confirmation" => RunStatus::Waiting,
        "finished" => RunStatus::Succeeded,
        "error" | "stuck" => RunStatus::Failed,
        _ => RunStatus::Unknown,
    }
}

/// Advance the run one step: resolve the start task, then read execution status.
fn refresh_with(server: &Server, manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let remote = run
        .remote_id
        .clone()
        .context("no remote id; check OpenHands before resubmitting")?;
    if let Some(task_id) = remote.strip_prefix(TASK_PREFIX) {
        let t = server.call(
            "GET",
            &format!("/api/v1/app-conversations/start-tasks?ids={task_id}"),
            None,
        )?;
        let t = first(&t);
        match t.get("status").and_then(Value::as_str) {
            Some("READY") => {
                let conv = t
                    .get("app_conversation_id")
                    .and_then(Value::as_str)
                    .context("READY start task missing app_conversation_id")?;
                run.remote_id = Some(conv.to_string());
                run.status = RunStatus::Running;
            }
            Some("ERROR") => {
                run.status = RunStatus::Failed;
                run.error = Some("OpenHands failed to start the conversation".into());
            }
            _ => run.status = RunStatus::Running,
        }
        return Ok(());
    }
    let c = server.call(
        "GET",
        &format!("/api/v1/app-conversations?ids={remote}"),
        None,
    )?;
    let c = first(&c);
    let exec = c
        .get("execution_status")
        .and_then(Value::as_str)
        .context("response missing execution_status")?;
    run.status = map_execution(exec);
    crate::susi_config::atomic_write_json_pretty(
        &manager.run_dir(&run.id)?.join("provider.json"),
        c,
    )?;
    Ok(())
}

pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let server = Server::new(&run.adapter)?;
    let created = server.call(
        "POST",
        "/api/v1/app-conversations",
        Some(&create_body(&run.prompt)),
    )?;
    let id = created
        .get("id")
        .and_then(Value::as_str)
        .context("create response missing task id")?;
    run.remote_id = Some(
        match created.get("app_conversation_id").and_then(Value::as_str) {
            Some(conv) => conv.to_string(),
            None => format!("{TASK_PREFIX}{id}"),
        },
    );
    manager.save(run)?;
    run.status = RunStatus::Running;
    while run.status == RunStatus::Running {
        if manager.cancel_requested(&run.id)? {
            cancel(run)?;
            run.status = RunStatus::Cancelled;
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(3));
        refresh_with(&server, manager, run)?;
        manager.save(run)?;
    }
    Ok(())
}

pub(super) fn refresh(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    refresh_with(&Server::new(&run.adapter)?, manager, run)
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    let server = Server::new(&run.adapter)?;
    let remote = run.remote_id.as_deref().context("no remote id to cancel")?;
    if remote.starts_with(TASK_PREFIX) {
        bail!("conversation is still starting; retry cancel once it is READY");
    }
    let c = server.call(
        "GET",
        &format!("/api/v1/app-conversations?ids={remote}"),
        None,
    )?;
    let sandbox = first(&c)
        .get("sandbox_id")
        .and_then(Value::as_str)
        .context("conversation has no sandbox_id to pause")?;
    server.call(
        "POST",
        &format!("/api/v1/sandboxes/{sandbox}/pause"),
        Some(&json!({})),
    )?;
    Ok(())
}

pub(super) fn send(_run: &RunRecord, _message: &str) -> Result<()> {
    bail!("the OpenHands V1 REST API documents no follow-up message endpoint; start a new task instead")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn execution_status_mapping_never_fakes_success() {
        for (s, want) in [
            ("finished", RunStatus::Succeeded),
            ("running", RunStatus::Running),
            ("paused", RunStatus::Waiting),
            ("waiting_for_confirmation", RunStatus::Waiting),
            ("error", RunStatus::Failed),
            ("stuck", RunStatus::Failed),
            ("???", RunStatus::Unknown),
        ] {
            assert_eq!(map_execution(s), want, "{s}");
        }
    }

    #[test]
    fn create_body_uses_the_documented_initial_message_shape() {
        assert_eq!(
            create_body("go"),
            json!({"initial_message": {"content": [{"type": "text", "text": "go"}]}})
        );
    }

    /// Fake app-server: create → start-task (READY) → conversation (finished).
    fn fake_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let mut buf = [0u8; 8192];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                let body = if line.starts_with("POST /api/v1/app-conversations ") {
                    assert!(req.contains("Bearer k-test"));
                    json!({"id": "st1", "status": "WORKING"}).to_string()
                } else if line.contains("/start-tasks?ids=st1") {
                    json!([{"id": "st1", "status": "READY", "app_conversation_id": "c1"}])
                        .to_string()
                } else if line.contains("/app-conversations?ids=c1") {
                    json!([{"id": "c1", "sandbox_id": "sb", "execution_status": "finished"}])
                        .to_string()
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
    fn runs_a_conversation_to_finished_over_the_v1_api() {
        std::env::set_var("SUSI_TEST_OH_KEY", "k-test");
        let root =
            std::env::temp_dir().join(format!("susi-oh-{}", super::super::unique_id().unwrap()));
        std::fs::create_dir_all(&root).unwrap();
        let manager = AgentManager::with_config(
            &root,
            super::super::CatalogKind::Execution,
            root.join("cfg"),
        )
        .unwrap();
        let adapter = Adapter::OpenHandsServer {
            base_url: fake_server(),
            api_key_env: Some("SUSI_TEST_OH_KEY".into()),
        };
        let run = manager
            .prepare_adapter("openhands-server", "fix it", adapter)
            .unwrap();
        let done = manager.execute(&run.id).unwrap();
        assert_eq!(done.status, RunStatus::Succeeded, "{:?}", done.error);
        assert_eq!(done.remote_id.as_deref(), Some("c1"));
        assert!(send(&done, "more").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
