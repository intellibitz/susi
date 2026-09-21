//! Native Devin v1 session and Manus v2 task lifecycles.
use super::{Adapter, AgentManager, RunRecord, RunStatus};
use anyhow::{bail, Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

struct Cloud {
    client: Client,
    base: &'static str,
    key: String,
    manus: bool,
}
impl Cloud {
    fn new(adapter: &Adapter) -> Result<Self> {
        let (base, env, manus) = match adapter {
            Adapter::Devin { api_key_env } => ("https://api.devin.ai/v1", api_key_env, false),
            Adapter::Manus { api_key_env } => ("https://api.manus.ai/v2", api_key_env, true),
            _ => bail!("not a cloud adapter"),
        };
        let key = std::env::var(env).with_context(|| format!("missing {env}"))?;
        if key.trim().is_empty() {
            bail!("missing {env}");
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            base,
            key,
            manus,
        })
    }
    fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        let req = self.client.request(method, format!("{}{path}", self.base));
        if self.manus {
            req.header("x-manus-api-key", &self.key)
        } else {
            req.bearer_auth(&self.key)
        }
    }
    fn post(&self, path: &str, body: Value) -> Result<Value> {
        response(self.request(reqwest::Method::POST, path).json(&body))
    }
    fn remote_path(&self, run: &RunRecord) -> Result<String> {
        let id = remote_id(run)?;
        Ok(format!("/sessions/{id}"))
    }
}

fn response(req: RequestBuilder) -> Result<Value> {
    let res = req
        .send()
        .context("cloud request failed; outcome may be unknown")?;
    let status = res.status();
    // Do not include provider error bodies, which can echo credentials or prompts.
    if !status.is_success() {
        bail!("cloud API returned HTTP {status}");
    }
    let mut bytes = Vec::new();
    res.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 8 * 1024 * 1024 {
        bail!("cloud response exceeds 8 MiB");
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let value: Value = serde_json::from_slice(&bytes).context("invalid cloud JSON response")?;
    if value.get("ok") == Some(&Value::Bool(false)) {
        bail!("cloud API rejected request");
    }
    Ok(value)
}
fn remote_id(run: &RunRecord) -> Result<&str> {
    let id = run
        .remote_id
        .as_deref()
        .context("no remote task ID; check provider dashboard before resubmitting")?;
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("invalid remote task ID");
    }
    Ok(id)
}

pub(super) fn execute(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let cloud = Cloud::new(&run.adapter)?;
    let created = if cloud.manus {
        cloud.post("/task.create", json!({"message": {"content": run.prompt}}))?
    } else {
        cloud.post("/sessions", json!({"prompt": run.prompt}))?
    };
    run.remote_id = Some(
        created
            .get(if cloud.manus { "task_id" } else { "session_id" })
            .and_then(Value::as_str)
            .context("create response missing remote ID")?
            .into(),
    );
    run.remote_url = created
        .get(if cloud.manus { "task_url" } else { "url" })
        .and_then(Value::as_str)
        .map(str::to_owned);
    // Save immediately. Any subsequent transport error retains an ID for recovery.
    manager.save(run)?;
    loop {
        if manager.cancel_requested(&run.id)? {
            cancel(run)?;
            run.status = RunStatus::Cancelled;
            return Ok(());
        }
        refresh_with(&cloud, manager, run)?;
        manager.save(run)?;
        if run.status != RunStatus::Running {
            return Ok(());
        }
        for _ in 0..20 {
            if manager.cancel_requested(&run.id)? {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
}

pub(super) fn refresh(manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    refresh_with(&Cloud::new(&run.adapter)?, manager, run)
}
fn refresh_with(cloud: &Cloud, manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let id = remote_id(run)?;
    let value = if cloud.manus {
        response(
            cloud
                .request(reqwest::Method::GET, "/task.detail")
                .query(&[("task_id", id)]),
        )?
    } else {
        response(cloud.request(reqwest::Method::GET, &cloud.remote_path(run)?))?
    };
    let status = if cloud.manus {
        value.pointer("/task/status")
    } else {
        value.get("status_enum")
    }
    .and_then(Value::as_str)
    .context("provider response missing recognized status field")?;
    run.status = map_status(cloud.manus, status);
    let dir = manager.run_dir(&run.id)?;
    super::atomic_json(&dir.join("provider.json"), &value)?;
    // Preserve all pages of Manus outputs, including artifact URLs, as JSONL.
    // Replace the snapshot only after a complete, successful fetch.
    if cloud.manus {
        let mut cursor = String::new();
        let mut seen = std::collections::HashSet::new();
        let mut pages = Vec::new();
        loop {
            let page = response(
                cloud
                    .request(reqwest::Method::GET, "/task.listMessages")
                    .query(&[
                        ("task_id", remote_id(run)?),
                        ("order", "asc"),
                        ("cursor", &cursor),
                    ]),
            )?;
            let more = page
                .get("has_more")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let next = page
                .get("next_cursor")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            pages.push(page);
            if !more {
                break;
            }
            if next.is_empty() || !seen.insert(next.clone()) || pages.len() >= 1000 {
                bail!("invalid or excessive Manus pagination; output snapshot not replaced");
            }
            cursor = next;
        }
        super::atomic_json(&dir.join("stdout.log"), &pages)?;
    } else {
        super::atomic_json(&dir.join("stdout.log"), &value)?;
    }
    Ok(())
}

fn map_status(manus: bool, status: &str) -> RunStatus {
    if manus {
        // Manus v2: "stopped" is the terminal completed state (no separate "finished").
        // Cancellation sets Cancelled before refresh; dashboard-stop is treated as completed.
        match status {
            "running" => RunStatus::Running,
            "waiting" => RunStatus::Waiting,
            "stopped" => RunStatus::Succeeded,
            "error" => RunStatus::Failed,
            _ => RunStatus::Unknown,
        }
    } else {
        match status {
            "working" | "resumed" | "resume_requested" | "resume_requested_frontend" => {
                RunStatus::Running
            }
            "blocked" | "suspend_requested" | "suspend_requested_frontend" => RunStatus::Waiting,
            "finished" => RunStatus::Succeeded,
            "expired" => RunStatus::Stopped,
            _ => RunStatus::Unknown,
        }
    }
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    let cloud = Cloud::new(&run.adapter)?;
    if cloud.manus {
        cloud.post("/task.stop", json!({"task_id": remote_id(run)?}))?;
    } else {
        response(cloud.request(reqwest::Method::DELETE, &cloud.remote_path(run)?))?;
    }
    Ok(())
}

pub(super) fn send(run: &RunRecord, message: &str) -> Result<()> {
    let cloud = Cloud::new(&run.adapter)?;
    if cloud.manus {
        cloud.post(
            "/task.sendMessage",
            json!({"task_id": remote_id(run)?, "message": {"content": message}}),
        )?;
    } else {
        cloud.post(
            &format!("{}/messages", cloud.remote_path(run)?),
            json!({"message": message}),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cloud_statuses_do_not_fabricate_success() {
        assert_eq!(map_status(true, "stopped"), RunStatus::Succeeded);
        assert_eq!(map_status(true, "waiting"), RunStatus::Waiting);
        assert_eq!(map_status(true, "error"), RunStatus::Failed);
        assert_eq!(map_status(false, "finished"), RunStatus::Succeeded);
        assert_eq!(map_status(false, "blocked"), RunStatus::Waiting);
        assert_eq!(map_status(false, "invented"), RunStatus::Unknown);
    }
    #[test]
    fn response_rejects_http_and_application_errors() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        for (status, body, success) in [
            ("200 OK", "{\"ok\":true}", true),
            ("200 OK", "{\"ok\":false}", false),
            ("401 Unauthorized", "secret", false),
            ("200 OK", "not JSON", false),
        ] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                stream.read(&mut buf).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let result = response(Client::new().get(format!("http://{address}")));
            assert_eq!(result.is_ok(), success);
            if let Err(e) = result {
                assert!(!e.to_string().contains("secret"));
            }
            worker.join().unwrap();
        }
    }
}
