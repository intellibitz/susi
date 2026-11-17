//! Socket reach for the self-build harness (zero-UI plan B4 group 1):
//! `susi harness status|budget|models|plan|author|run`.
//!
//! Each tool forwards to the daemon's `daemon.harness` topic, which calls
//! `susi_harness::operator`, the library the CLI command calls. The socket
//! has no caller working directory: paths are absolute, and `repo` (the
//! CLI's working directory) is admitted by the MAC policy. A run is long,
//! so `harness_run` starts it on a daemon thread and returns; `harness_runs`
//! and `harness_status` (with the run's `state`) report progress. Serving
//! the harness API and the gateway stays with the CLI (process entry points).

#[cfg(test)]
use serde_json::json;
use serde_json::Value;
use std::path::Path;

use crate::susi_core::plane_bus::daemon;
use crate::susi_error::{EaiError, EaiResult};

use super::forward::plane_tools;
use super::CoreTools;

impl CoreTools {
    plane_tools! {
        daemon::harness;
        pub harness_status => "status" [] paths [ledger: read, events: read, state: read, plan: read];
        pub harness_budget => "budget" [] in OptionalReadRepo paths [ledger: read];
        pub harness_models => "models" [mode: Str, provider: Str] in OptionalReadRepo;
        pub harness_plan => "plan" [mission: Str, limit: U64, workers: Str, roadmap: Str, gateway: Str] in RequiredWriteRepo
            paths [out: write];
        pub harness_author => "author" [gateway: Str, vector: Str, model: Str, max_tasks: U64, workers: Str] in RequiredWriteRepo
            paths [out: write, roadmap: read];
        harness_run_admitted => "run" [
            gateway: Str, model: Str, timeout: U64, parallel: U64,
            prefix: Str, workers: Str, budget_usd: Str, daily_usd: Str, attempts: U64,
        ] in RequiredWriteRepo
            paths [plan: read, catalogue: read, state: write, ledger: write, controls: write, rsi_log: write, adapter_state: write];
        pub harness_runs => "runs" [];
    }

    pub fn harness_run(arg: &Value, workspace: &Path) -> EaiResult<String> {
        if arg.get("susi").is_some_and(|v| !v.is_null()) {
            return Err(EaiError::config(
                "susi cannot be chosen over the socket; a socket run drives the daemon's own binary",
            ));
        }
        Self::harness_run_admitted(arg, workspace)
    }
}

#[cfg(test)]
#[path = "../tests/harness_tools.rs"]
mod tests;
