//! Socket reach for the provider, key and brain commands (zero-UI plan B4
//! group 3): `susi openweight|frontier|openrouter`, `susi models probe`,
//! `susi keys list|prefer|check|models`, `susi brain`, `susi benchmark` and
//! `susi verify-download-agent`.
//!
//! Each tool forwards to the daemon's `daemon.providers` topic, which calls
//! the same `susi_daemon::provider_ops` function the CLI command calls and
//! returns the document the command prints. Key values never cross: key
//! tools report presence only, and setting or removing a key stays local
//! (`tests/socket_parity.rs` exempts `keys set` and `keys remove`).

use serde_json::{json, Value};
use std::path::Path;

use crate::susi_core::plane_bus::daemon;
use crate::susi_error::{EaiError, EaiResult};

use super::forward::plane_tools;
use super::CoreTools;

impl CoreTools {
    plane_tools! {
        daemon::providers;
        pub provider_list => "list" [vendor: Str];
        pub provider_doctor => "doctor" [vendor: Str, model: Str, live: Bool];
        pub provider_setup => "setup" [vendor: Str, model: Str];
        pub provider_configure => "configure" [vendor: Str, model: Str, override: Object] paths [file: read];
        pub provider_reset => "reset" [vendor: Str, model: Str];
        pub provider_pull => "pull" [model: Str];
        pub provider_prefer => "prefer" [vendor: Str, model: Str, clear: Bool];
        pub provider_probe => "probe" [vendor: Str, model: Str, prompt: Str];
        pub provider_models => "openrouter_models" [live: Bool, limit: U64];
        pub keys_status => "keys_status" [];
        pub keys_prefer => "keys_prefer" [vendor: Str, clear: Bool];
        pub keys_check => "keys_check" [vendor: Str];
        pub keys_models => "keys_models" [vendor: Str];
        pub brain_status => "brain_status" [top: U64, class: Str];
        pub brain_budget => "brain_budget" [
            level: Str, free_only: Bool, allow_paid: Bool, max_spend_microusd: U64,
            max_tokens: U64, max_requests: U64, max_parallel: U64, new_window: Bool,
        ];
        pub brain_classify => "brain_classify" [prompt: Str];
        pub brain_explain => "brain_explain" [prompt: Str];
        pub brain_export => "brain_export" [] paths [out: write];
        pub brain_import => "brain_import" [] paths [file: read];
        pub brain_matrix => "brain_matrix" [];
        pub brain_cache => "brain_cache" [];
        pub brain_reset => "brain_reset" [];
        pub brain_bench => "brain_bench" [] paths [out: write];
        pub brain_probe => "brain_probe" [provider: Str, no_record: Bool];
        pub verify_download_agent => "verify_download_agent" [] in Server;
    }

    pub fn benchmark_run(_arg: &Value, workspace: &Path) -> EaiResult<String> {
        let reply = daemon::providers(json!({
            "action": "benchmark",
            "workspace": workspace.display().to_string(),
        }))
        .map_err(EaiError::config)?;
        Ok(reply
            .get("report")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }
}

#[cfg(test)]
#[path = "../tests/provider_tools.rs"]
mod tests;
