//! Shared outbound HTTP client for every SUSI plane that talks to a remote
//! JSON endpoint (MCP, peer A2A, live search, eval/benchmark, HF download,
//! cloud agent APIs, webhooks).
//!
//! Vendor SDKs and provider-specific wire shapes do **not** live here:
//! OpenAI / Anthropic / Gemini bodies stay in `susi-adapters-llm`; Candle
//! stays in `susi-vendor-candle`. This module is the one timeout-bounded
//! `ureq` agent so core/config/orchestration crates do not each declare an
//! HTTP client crate.

use std::io::Read;
use std::sync::OnceLock;
use std::time::Duration;

/// Shared ureq Agent with connect/read/write timeouts. `ureq::get` /
/// `ureq::post` free functions use a default agent with NO timeouts at all
/// — a stalled remote (or one that completes the handshake but then goes
/// silent mid-response, e.g. during SSE body streaming) blocks the calling
/// thread forever. Agent-level timeout_read/timeout_write bound every
/// socket read and write, including streaming body reads after the initial
/// response headers arrive, which a per-request `.timeout()` alone would
/// not cover.
static HTTP_AGENT: OnceLock<ureq::Agent> = OnceLock::new();

/// Process-wide timeout-bounded HTTP agent.
pub fn http_agent() -> ureq::Agent {
    HTTP_AGENT
        .get_or_init(|| {
            let config = ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(10)))
                .timeout_recv_body(Some(Duration::from_secs(20)))
                .timeout_send_body(Some(Duration::from_secs(20)))
                .build();
            ureq::Agent::new_with_config(config)
        })
        .clone()
}

/// One completed request: status, headers, and a `Read` body. Callers
/// never name `ureq`. HTTP error statuses are returned, not turned into
/// `Err` — resume downloads need 416 / 206.
pub struct HttpCall {
    /// HTTP status code.
    pub status: u16,
    headers: Vec<(String, String)>,
    body: ureq::Body,
}

impl HttpCall {
    /// First header value matching `name` (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Consume the body as a `Read`.
    pub fn into_reader(self) -> impl Read {
        self.body.into_reader()
    }

    /// Consume the body into a bounded byte buffer, rejecting payloads over
    /// `max`. One extra byte is read to detect overflow.
    pub fn into_bytes(self, max: u64) -> Result<Vec<u8>, std::io::Error> {
        let mut bytes = Vec::new();
        self.into_reader()
            .take(max.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response body exceeds configured limit",
            ));
        }
        Ok(bytes)
    }
}

fn into_call(resp: ureq::http::Response<ureq::Body>) -> HttpCall {
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    HttpCall {
        status,
        headers,
        body: resp.into_body(),
    }
}

/// GET or HEAD with explicit timeouts and redirect cap. `max_redirects = 0`
/// is required to observe Hugging Face `X-Linked-ETag` on the 302.
pub fn http_call(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    timeout_secs: u64,
    max_redirects: u32,
) -> Result<HttpCall, String> {
    http_call_with_body(method, url, headers, None, timeout_secs, max_redirects)
}

/// GET/HEAD/DELETE/POST/PUT/PATCH. `body` is sent for methods that carry
/// one; GET/HEAD ignore it. HTTP error statuses are returned, not `Err`.
#[allow(clippy::too_many_arguments)] // six independent request facts every caller already has; a params struct would only move the count to all ten call sites
pub fn http_call_with_body(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    timeout_secs: u64,
    max_redirects: u32,
) -> Result<HttpCall, String> {
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15).min(timeout)))
        .timeout_recv_body(Some(timeout))
        .timeout_send_body(Some(timeout))
        .max_redirects(max_redirects)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    match method {
        "HEAD" | "GET" | "DELETE" => {
            let mut req = match method {
                "HEAD" => agent.head(url),
                "DELETE" => agent.delete(url),
                _ => agent.get(url),
            };
            for (name, value) in headers {
                req = req.header(*name, *value);
            }
            let resp = req
                .config()
                .http_status_as_error(false)
                .build()
                .call()
                .map_err(|e| e.to_string())?;
            Ok(into_call(resp))
        }
        "POST" | "PUT" | "PATCH" => {
            let mut req = match method {
                "PUT" => agent.put(url),
                "PATCH" => agent.patch(url),
                _ => agent.post(url),
            };
            for (name, value) in headers {
                req = req.header(*name, *value);
            }
            let payload = body.unwrap_or(&[]);
            let resp = req
                .config()
                .http_status_as_error(false)
                .build()
                .send(payload)
                .map_err(|e| e.to_string())?;
            Ok(into_call(resp))
        }
        other => Err(format!("unsupported HTTP method {other}")),
    }
}
