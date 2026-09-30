//! Manage Ollama models over its local HTTP API: list (`/api/tags`),
//! show (`/api/show`), pull with progress (`/api/pull`) and remove
//! (`/api/delete`). The transport is injected so the whole client is
//! testable offline; [`http_transport`] wires it to
//! `susi_http_transport::http_call_with_body`. Pulls that would exceed the
//! disk budget are refused before the first request.

use serde::{Deserialize, Serialize};
use susi_error::{EaiError, EaiResult};

pub const DEFAULT_BASE: &str = "http://127.0.0.1:11434";
/// Response bodies from the local daemon are small; cap defensively.
const MAX_BODY: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    /// JSON body for POST/DELETE.
    pub body: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// A model row from `/api/tags`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OllamaModel {
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub modified_at: String,
}

/// Detail from `/api/show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub parameter_size: String,
    #[serde(default)]
    pub quantization_level: String,
    #[serde(default)]
    pub context_length: u64,
}

/// One NDJSON progress line from `/api/pull`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PullProgress {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub completed: u64,
    #[serde(default)]
    pub total: u64,
}

pub struct OllamaClient<'a> {
    base: String,
    transport: &'a dyn Fn(&HttpRequest) -> EaiResult<HttpResponse>,
}

impl<'a> OllamaClient<'a> {
    #[must_use]
    pub fn new(base: &str, transport: &'a dyn Fn(&HttpRequest) -> EaiResult<HttpResponse>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            transport,
        }
    }

    fn call(&self, method: &'static str, path: &str, body: Option<Vec<u8>>) -> EaiResult<Vec<u8>> {
        let resp = (self.transport)(&HttpRequest {
            method,
            url: format!("{}{path}", self.base),
            body,
        })?;
        if !(200..300).contains(&resp.status) {
            return Err(EaiError::network(format!(
                "ollama {method} {path} -> {}: {}",
                resp.status,
                String::from_utf8_lossy(&resp.body)
                    .chars()
                    .take(200)
                    .collect::<String>()
            )));
        }
        Ok(resp.body)
    }

    /// `GET /api/tags` — installed models.
    pub fn list(&self) -> EaiResult<Vec<OllamaModel>> {
        #[derive(Deserialize)]
        struct Tags {
            #[serde(default)]
            models: Vec<OllamaModel>,
        }
        let body = self.call("GET", "/api/tags", None)?;
        let tags: Tags =
            serde_json::from_slice(&body).map_err(|e| EaiError::network(e.to_string()))?;
        Ok(tags.models)
    }

    /// `POST /api/show` — details for one model.
    pub fn show(&self, name: &str) -> EaiResult<ModelInfo> {
        #[derive(Serialize)]
        struct ShowReq<'a> {
            name: &'a str,
        }
        #[derive(Deserialize)]
        struct ShowResp {
            #[serde(default)]
            details: ShowDetails,
            #[serde(default)]
            model_info: serde_json::Value,
        }
        #[derive(Deserialize, Default)]
        struct ShowDetails {
            #[serde(default)]
            family: String,
            #[serde(default)]
            parameter_size: String,
            #[serde(default)]
            quantization_level: String,
        }
        let body =
            serde_json::to_vec(&ShowReq { name }).map_err(|e| EaiError::config(e.to_string()))?;
        let raw = self.call("POST", "/api/show", Some(body))?;
        let resp: ShowResp =
            serde_json::from_slice(&raw).map_err(|e| EaiError::network(e.to_string()))?;
        // `context_length` lives in model_info as a flat "<arch>.context_length" key.
        let ctx = resp
            .model_info
            .as_object()
            .and_then(|m| {
                m.iter()
                    .find(|(k, _)| k.ends_with(".context_length"))
                    .and_then(|(_, v)| v.as_u64())
            })
            .unwrap_or(0);
        Ok(ModelInfo {
            family: resp.details.family,
            parameter_size: resp.details.parameter_size,
            quantization_level: resp.details.quantization_level,
            context_length: ctx,
        })
    }

    /// `POST /api/pull` — stream progress lines through `on_progress`.
    /// `free_bytes`/`budget` guards refuse the pull before contacting the
    /// daemon when `expected_size` is known to exceed them.
    pub fn pull(
        &self,
        name: &str,
        expected_size: Option<u64>,
        budget: Option<&crate::disk_budget::DiskBudget>,
        on_progress: &dyn Fn(&PullProgress),
    ) -> EaiResult<()> {
        if let (Some(size), Some(b)) = (expected_size, budget) {
            let ev = b.plan_eviction(size);
            if !ev.feasible {
                return Err(EaiError::config(format!(
                    "refusing pull of {name}: {size}B exceeds disk budget even after evicting {}B",
                    ev.freed
                )));
            }
        }
        #[derive(Serialize)]
        struct PullReq<'a> {
            name: &'a str,
            stream: bool,
        }
        let body = serde_json::to_vec(&PullReq { name, stream: true })
            .map_err(|e| EaiError::config(e.to_string()))?;
        let raw = self.call("POST", "/api/pull", Some(body))?;
        let mut saw_error = None;
        for line in raw.split(|b| *b == b'\n') {
            if line.is_empty() {
                continue;
            }
            let p: PullProgress =
                serde_json::from_slice(line).map_err(|e| EaiError::network(e.to_string()))?;
            if let Some(err) = p.status.strip_prefix("error:") {
                saw_error = Some(err.trim().to_string());
            }
            // status may be "error" with detail in an `error` field.
            #[derive(Deserialize)]
            struct ErrLine {
                #[serde(default)]
                error: String,
            }
            if let Ok(el) = serde_json::from_slice::<ErrLine>(line) {
                if !el.error.is_empty() {
                    saw_error = Some(el.error);
                }
            }
            if p.status == "error" {
                saw_error.get_or_insert_with(|| "pull failed".to_string());
            }
            on_progress(&p);
        }
        if let Some(e) = saw_error {
            return Err(EaiError::network(format!("ollama pull {name}: {e}")));
        }
        Ok(())
    }

    /// `DELETE /api/delete`.
    pub fn remove(&self, name: &str) -> EaiResult<()> {
        #[derive(Serialize)]
        struct DelReq<'a> {
            name: &'a str,
        }
        let body =
            serde_json::to_vec(&DelReq { name }).map_err(|e| EaiError::config(e.to_string()))?;
        self.call("DELETE", "/api/delete", Some(body))?;
        Ok(())
    }
}

/// Real transport over `susi_http_transport::http_call_with_body`.
///
/// # Errors
/// Propagates transport failures as [`EaiError::network`].
pub fn http_transport(req: &HttpRequest) -> EaiResult<HttpResponse> {
    let call = susi_http_transport::http_call_with_body(
        req.method,
        &req.url,
        &[("Content-Type", "application/json")],
        req.body.as_deref(),
        120,
        0,
    )
    .map_err(EaiError::network)?;
    let status = call.status;
    let body = call
        .into_bytes(MAX_BODY)
        .map_err(|e| EaiError::network(e.to_string()))?;
    Ok(HttpResponse { status, body })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    /// Minimal single-connection-per-request HTTP/1.0 server. `routes`
    /// maps "<METHOD> <path>" to (status, body).
    fn serve(
        routes: &'static [(&'static str, u16, &'static str)],
    ) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || loop {
            let Ok((mut sock, _)) = listener.accept() else {
                return;
            };
            sock.set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .ok();
            let mut buf = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                let n = match sock.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(_) => break,
                };
                buf.extend_from_slice(&chunk[..n]);
                // done when headers + Content-Length body arrived
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
                    let want = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(str::trim)
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= pos + 4 + want {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&buf).to_string();
            let line = text.lines().next().unwrap_or("").to_string();
            let key = line
                .split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ");
            tx.send(text.clone()).ok();
            let (status, body) = routes
                .iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, s, b)| (*s, *b))
                .unwrap_or((404, "{\"error\":\"not found\"}"));
            let resp = format!(
                "HTTP/1.0 {status} X\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).ok();
        });
        (format!("http://127.0.0.1:{port}"), rx)
    }

    fn json_transport(req: &HttpRequest) -> EaiResult<HttpResponse> {
        http_transport(req)
    }

    #[test]
    fn ollama_models_list_parses_tags() {
        let (base, _rx) = serve(&[(
            "GET /api/tags",
            200,
            "{\"models\":[{\"name\":\"qwen2.5:7b\",\"size\":4700000000,\"modified_at\":\"2025-01-01T00:00:00Z\"}]}",
        )]);
        let c = OllamaClient::new(&base, &json_transport);
        let models = c.list().unwrap();
        assert_eq!(models[0].name, "qwen2.5:7b");
        assert_eq!(models[0].size, 4_700_000_000);
    }

    #[test]
    fn ollama_models_show_reads_details_and_context() {
        let (base, rx) = serve(&[(
            "POST /api/show",
            200,
            "{\"details\":{\"family\":\"qwen2\",\"parameter_size\":\"7.6B\",\"quantization_level\":\"Q4_K_M\"},\"model_info\":{\"qwen2.context_length\":131072}}",
        )]);
        let c = OllamaClient::new(&base, &json_transport);
        let info = c.show("qwen2.5:7b").unwrap();
        assert_eq!(info.family, "qwen2");
        assert_eq!(info.context_length, 131_072);
        let sent = rx.recv().unwrap();
        assert!(sent.contains("\"name\":\"qwen2.5:7b\""));
    }

    #[test]
    fn ollama_models_pull_streams_progress() {
        let (base, _rx) = serve(&[(
            "POST /api/pull",
            200,
            "{\"status\":\"pulling manifest\"}\n{\"status\":\"downloading\",\"completed\":50,\"total\":100}\n{\"status\":\"success\"}\n",
        )]);
        let c = OllamaClient::new(&base, &json_transport);
        let seen = std::cell::RefCell::new(Vec::new());
        c.pull("qwen2.5:7b", Some(100), None, &|p| {
            seen.borrow_mut().push(p.status.clone())
        })
        .unwrap();
        assert!(seen.borrow().iter().any(|s| s == "success"));
    }

    #[test]
    fn ollama_models_pull_refuses_over_budget() {
        let (base, _rx) = serve(&[("POST /api/pull", 200, "{\"status\":\"success\"}\n")]);
        let c = OllamaClient::new(&base, &json_transport);
        let budget = crate::disk_budget::DiskBudget::new(1_000);
        let err = c
            .pull("huge:latest", Some(5_000), Some(&budget), &|_| {})
            .unwrap_err();
        assert!(err.to_string().contains("disk budget"));
    }

    #[test]
    fn ollama_models_pull_surfaces_server_error() {
        let (base, _rx) = serve(&[(
            "POST /api/pull",
            200,
            "{\"status\":\"pulling manifest\"}\n{\"error\":\"pull model manifest: file does not exist\"}\n",
        )]);
        let c = OllamaClient::new(&base, &json_transport);
        let err = c.pull("missing:latest", None, None, &|_| {}).unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }

    #[test]
    fn ollama_models_remove_sends_delete() {
        let (base, rx) = serve(&[("DELETE /api/delete", 200, "")]);
        let c = OllamaClient::new(&base, &json_transport);
        c.remove("old:model").unwrap();
        let sent = rx.recv().unwrap();
        assert!(sent.starts_with("DELETE /api/delete"));
        assert!(sent.contains("\"name\":\"old:model\""));
    }

    #[test]
    fn ollama_models_non_2xx_is_network_error() {
        let (base, _rx) = serve(&[("GET /api/tags", 500, "boom")]);
        let c = OllamaClient::new(&base, &json_transport);
        assert!(c.list().is_err());
    }
}
