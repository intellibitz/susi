//! Shared outbound HTTP client for every SUSI plane that talks to a remote
//! JSON endpoint (MCP, peer A2A, live search, eval/benchmark).
//!
//! Vendor SDKs and provider-specific wire shapes do **not** live here:
//! OpenAI / Anthropic / Gemini bodies stay in `susi-adapters-llm`; Candle
//! stays in `susi-vendor-candle`. This module is the one timeout-bounded
//! `ureq` agent so core/config/orchestration crates do not each declare an
//! HTTP client crate.

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
