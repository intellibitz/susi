//! Shared outbound HTTP client for every SUSI plane that talks to a remote
//! JSON endpoint (MCP, peer A2A, live search, eval/benchmark, HF download).
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

/// One completed GET/HEAD: status, headers, and a `Read` body. Callers
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
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15).min(timeout)))
        .timeout_recv_body(Some(timeout))
        .timeout_send_body(Some(timeout))
        .max_redirects(max_redirects)
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let mut req = match method {
        "HEAD" => agent.head(url),
        "GET" => agent.get(url),
        other => return Err(format!("unsupported HTTP method {other}")),
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
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
        .collect();
    Ok(HttpCall {
        status,
        headers,
        body: resp.into_body(),
    })
}
