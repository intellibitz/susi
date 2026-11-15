//! Concrete MCP/meta tool handlers (`CoreTools`).

use std::sync::OnceLock;

use crate::susi_error::{EaiError, EaiResult};

/// Plane-bus topic a plug serves for headless page capture: `{"url"}` in,
/// `{"url","html","png_bytes"}` out (`browser_automate`).
#[cfg(feature = "tools-rich")]
pub const TOPIC_BROWSER_CAPTURE: &str = "desktop.browser.capture";

pub struct CoreTools;

impl CoreTools {
    /// Shared long-lived tokio runtime for tool functions that must bridge into
    /// async APIs (Docker/Qdrant clients). Avoids constructing/tearing down a
    /// fresh multi-thread runtime on every call (Mandate 28: Async Defaults).
    pub(crate) fn shared_runtime() -> EaiResult<&'static tokio::runtime::Runtime> {
        static RT: OnceLock<std::io::Result<tokio::runtime::Runtime>> = OnceLock::new();
        RT.get_or_init(tokio::runtime::Runtime::new)
            .as_ref()
            .map_err(|e| EaiError::process(format!("Failed to start shared tokio runtime: {}", e)))
    }
}

mod agents;
mod cluster;
mod files_os;
mod knowledge;
mod models;
mod substrate;

pub(crate) use cluster::remote_result_is_error;
#[cfg(test)]
pub(crate) use files_os::drain_exec_pipe;
