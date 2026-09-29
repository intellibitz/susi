//! Native Devin v3 session and Manus v2 task lifecycles.
use super::{Adapter, AgentManager, RunRecord, RunStatus};
use serde_json::{json, Value};
use std::time::Duration;
use susi_error::{eai_bail as bail, EaiResult as Result, ResultExt as Context};

struct Cloud {
    /// Full API root including version prefix, e.g. `https://api.devin.ai/v3`.
    base: String,
    key: String,
    manus: bool,
    /// Devin org id (`org-…`); empty for Manus.
    org_id: String,
}

impl Cloud {
    #[allow(clippy::wildcard_enum_match_arm)] // only cloud adapters reach here; every other Adapter kind bails
    fn new(adapter: &Adapter) -> Result<Self> {
        let (base, env, manus, org_env) = match adapter {
            Adapter::Devin {
                api_key_env,
                org_id_env,
            } => (
                "https://api.devin.ai/v3".to_string(),
                api_key_env.as_str(),
                false,
                Some(org_id_env.as_str()),
            ),
            Adapter::Manus { api_key_env } => (
                "https://api.manus.ai/v2".to_string(),
                api_key_env.as_str(),
                true,
                None,
            ),
            _ => bail!("not a cloud adapter"),
        };
        // Cloud agents receive the user's prompt and repository context.
        if !crate::susi_core::mac_policy::egress_permitted(&base) {
            bail!("[PRIVACY] network egress to {base} blocked by the privacy posture (run `susi privacy consent --egress`)");
        }
        let key =
            crate::susi_config::env_or_cloud_env(env).with_context(|| format!("missing {env}"))?;
        if key.trim().is_empty() {
            bail!("missing {env}");
        }
        let org_id = if let Some(org_env) = org_env {
            let org = crate::susi_config::env_or_cloud_env(org_env).with_context(|| {
                format!("missing {org_env} (Devin API v3 requires an organization id)")
            })?;
            let org = org.trim().to_string();
            if org.is_empty() {
                bail!("missing {org_env}");
            }
            if !org.starts_with("org-") {
                bail!("{org_env} must look like org-… (got a non-org value)");
            }
            org
        } else {
            String::new()
        };
        Ok(Self {
            base,
            key,
            manus,
            org_id,
        })
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("Content-Type".into(), "application/json".into())];
        if self.manus {
            headers.push(("x-manus-api-key".into(), self.key.clone()));
        } else {
            headers.push(("Authorization".into(), format!("Bearer {}", self.key)));
        }
        headers
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let url = format!("{}{path}", self.base);
        let owned = self.headers();
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let bytes = match body {
            Some(value) => Some(serde_json::to_vec(value).context("cloud request JSON")?),
            None => None,
        };
        let call =
            susi_http_transport::http_call_with_body(method, &url, &refs, bytes.as_deref(), 30, 0)
                .map_err(|e| {
                    susi_error::EaiError::internal(format!(
                        "cloud request failed; outcome may be unknown: {e}"
                    ))
                })?;
        let status = call.status;
        let payload = call
            .into_bytes(8 * 1024 * 1024)
            .context("cloud response body")?;
        decode_cloud_body(status, &payload)
    }

    fn get_query(&self, path: &str, params: &[(&str, &str)]) -> Result<Value> {
        if params.is_empty() {
            return self.call("GET", path, None);
        }
        let query = params
            .iter()
            .map(|(k, v)| {
                format!(
                    "{}={}",
                    susi_paths::percent_encode_query(k),
                    susi_paths::percent_encode_query(v)
                )
            })
            .collect::<Vec<_>>()
            .join("&");
        self.call("GET", &format!("{path}?{query}"), None)
    }

    fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.call("POST", path, Some(&body))
    }

    fn remote_path(&self, run: &RunRecord) -> Result<String> {
        let id = remote_id(run)?;
        if self.manus {
            Ok(format!("/sessions/{id}"))
        } else {
            Ok(format!("/organizations/{}/sessions/{id}", self.org_id))
        }
    }

    fn sessions_collection(&self) -> String {
        format!("/organizations/{}/sessions", self.org_id)
    }
}

fn decode_cloud_body(status: u16, bytes: &[u8]) -> Result<Value> {
    // Do not include provider error bodies, which can echo credentials or prompts.
    if !(200..300).contains(&status) {
        bail!("cloud API returned HTTP {status}");
    }
    if bytes.len() > 8 * 1024 * 1024 {
        bail!("cloud response exceeds 8 MiB");
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    let value: Value = serde_json::from_slice(bytes).context("invalid cloud JSON response")?;
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
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::execute(manager, run);
    }
    let cloud = Cloud::new(&run.adapter)?;
    let created = if cloud.manus {
        cloud.post("/task.create", json!({"message": {"content": run.prompt}}))?
    } else {
        cloud.post(&cloud.sessions_collection(), json!({"prompt": run.prompt}))?
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
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::refresh(manager, run);
    }
    refresh_with(&Cloud::new(&run.adapter)?, manager, run)
}

fn refresh_with(cloud: &Cloud, manager: &AgentManager, run: &mut RunRecord) -> Result<()> {
    let id = remote_id(run)?;
    let value = if cloud.manus {
        cloud.get_query("/task.detail", &[("task_id", id)])?
    } else {
        cloud.call("GET", &cloud.remote_path(run)?, None)?
    };
    let status = if cloud.manus {
        value.pointer("/task/status")
    } else {
        // Prefer coarse `status`; fall back to `status_detail` for finer states.
        value
            .get("status")
            .or_else(|| value.get("status_detail"))
            .or_else(|| value.get("status_enum"))
    }
    .and_then(Value::as_str)
    .context("provider response missing recognized status field")?;
    run.status = map_status(cloud.manus, status);
    let dir = manager.run_dir(&run.id)?;
    crate::susi_config::atomic_write_json_pretty(&dir.join("provider.json"), &value)?;
    // Preserve all pages of Manus outputs, including artifact URLs, as JSONL.
    // Replace the snapshot only after a complete, successful fetch.
    if cloud.manus {
        let mut cursor = String::new();
        let mut seen = std::collections::HashSet::new();
        let mut pages = Vec::new();
        loop {
            let page = cloud.get_query(
                "/task.listMessages",
                &[
                    ("task_id", remote_id(run)?),
                    ("order", "asc"),
                    ("cursor", &cursor),
                ],
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
        crate::susi_config::atomic_write_json_pretty(&dir.join("stdout.log"), &pages)?;
    } else {
        // Snapshot messages when available; keep the session object as a fallback.
        let messages_path = format!("{}/messages", cloud.remote_path(run)?);
        if let Ok(messages) = cloud.call("GET", &messages_path, None) {
            crate::susi_config::atomic_write_json_pretty(&dir.join("stdout.log"), &messages)?;
        } else {
            crate::susi_config::atomic_write_json_pretty(&dir.join("stdout.log"), &value)?;
        }
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
        // Devin v3 coarse status + status_detail values.
        match status {
            "new" | "claimed" | "running" | "resuming" | "working" => RunStatus::Running,
            "waiting_for_user" | "waiting_for_approval" | "blocked" => RunStatus::Waiting,
            "exit" | "finished" => RunStatus::Succeeded,
            "error" => RunStatus::Failed,
            "suspended" | "expired" => RunStatus::Stopped,
            _ => RunStatus::Unknown,
        }
    }
}

pub(super) fn cancel(run: &RunRecord) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::cancel(run);
    }
    let cloud = Cloud::new(&run.adapter)?;
    if cloud.manus {
        cloud.post("/task.stop", json!({"task_id": remote_id(run)?}))?;
    } else {
        // v3: prefer an explicit stop; fall back to DELETE if the stop route is absent.
        let stop = format!("{}/stop", cloud.remote_path(run)?);
        match cloud.post(&stop, json!({})) {
            Ok(_) => {}
            Err(_) => {
                cloud.call("DELETE", &cloud.remote_path(run)?, None)?;
            }
        }
    }
    Ok(())
}

pub(super) fn send(run: &RunRecord, message: &str) -> Result<()> {
    if matches!(run.adapter, Adapter::A2a { .. }) {
        return super::a2a::send(run, message);
    }
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
        assert_eq!(map_status(false, "exit"), RunStatus::Succeeded);
        assert_eq!(map_status(false, "waiting_for_user"), RunStatus::Waiting);
        assert_eq!(map_status(false, "running"), RunStatus::Running);
        assert_eq!(map_status(false, "invented"), RunStatus::Unknown);
    }
    #[test]
    fn response_rejects_http_and_application_errors() {
        for (status, body, success) in [
            (200_u16, "{\"ok\":true}", true),
            (200, "{\"ok\":false}", false),
            (401, "secret", false),
            (200, "not JSON", false),
        ] {
            let result = decode_cloud_body(status, body.as_bytes());
            assert_eq!(result.is_ok(), success);
            if let Err(e) = result {
                assert!(!e.to_string().contains("secret"));
            }
        }
    }
}
