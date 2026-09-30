//! Bare `susi` status + next-step helper (zero-config UX).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BareStatus {
    pub ready: bool,
    pub next_step: String,
}

#[must_use]
pub fn bare_susi_status(daemon_up: bool, has_model: bool, keys_ok: bool) -> BareStatus {
    if !daemon_up {
        return BareStatus {
            ready: false,
            next_step: "start the daemon (susi daemon start)".into(),
        };
    }
    if !keys_ok {
        return BareStatus {
            ready: false,
            next_step: "register a cloud key when you need one (prompted on first use)".into(),
        };
    }
    if !has_model {
        return BareStatus {
            ready: false,
            next_step: "pull or select a local model (susi ecosystem scan)".into(),
        };
    }
    BareStatus {
        ready: true,
        next_step: "run a mission (susi \"your goal\")".into(),
    }
}

#[must_use]
pub fn render_bare(status: &BareStatus) -> String {
    format!(
        "susi status: {}\nnext: {}\n",
        if status.ready { "ready" } else { "not ready" },
        status.next_step
    )
}
