//! Socket reach for the adapter lifecycle commands (zero-UI plan B4 group
//! 7): `susi agents|frameworks|models|mcp doctor|setup|configure|reset`,
//! `susi agents|frameworks retry` and `susi mcp-add`.
//!
//! Each tool forwards to the daemon's `daemon.adapters` topic, which calls
//! the same `susi_daemon::adapter_ops` function the CLI command calls and
//! returns the document the command prints. Adapter overrides are host
//! configuration; only `retry` touches a workspace (its task records), and
//! a caller-named workspace is admitted by the MAC policy. Storing or
//! dropping an override needs filesystem.write, and storing one a later
//! run executes needs what it runs: process.exec for a program (agent or
//! framework `command`/`acp`, an MCP runner), network.egress for a remote
//! A2A agent (`MacPolicy::authorize_tool`, re-checked by the daemon on the
//! document it reads from `file`). A stdio `mcp_add` needs process.exec.

use serde_json::{json, Value};
use std::path::Path;

use crate::susi_core::plane_bus::daemon;
use crate::susi_error::EaiResult;

use super::forward::{forward, policy_workspace, Kind};
use super::forward::{forwarded_tools, plane_tools};
use super::CoreTools;

fn retry(kind: &str, arg: &Value, workspace: &Path) -> EaiResult<String> {
    let workspace = policy_workspace(arg, "workspace", workspace, true)?;
    forward(
        daemon::adapters,
        "retry",
        arg,
        &[("task_id", Kind::Str)],
        json!({ "kind": kind, "workspace": workspace.display().to_string() }),
    )
}

impl CoreTools {
    plane_tools! {
        daemon::adapters;
        pub adapter_doctor => "doctor" [kind: Str, id: Str] in Server;
        pub adapter_setup => "setup" [kind: Str, id: Str] in Server;
        pub adapter_configure => "configure" [kind: Str, id: Str, adapter: Object] in Server
            paths [file: read];
        pub adapter_reset => "reset" [kind: Str, id: Str] in Server;
        pub mcp_add => "mcp_add" [name: Str, command_or_url: Str, args: StrArray] in Server;
    }

    forwarded_tools! {
        agents_retry(arg, workspace) => retry("agents", arg, workspace);
        frameworks_retry(arg, workspace) => retry("frameworks", arg, workspace);
    }
}

#[cfg(test)]
#[path = "../tests/adapter_tools.rs"]
mod tests;
