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

use super::args;
use super::forward::forwarded_tools;
use super::forward::{forward, merged, policy_paths, policy_workspace, Kind};
use super::CoreTools;

const KIND: (&str, Kind) = ("kind", Kind::Str);
const ID: (&str, Kind) = ("id", Kind::Str);

fn adapters(
    action: &str,
    arg: &Value,
    keys: &[(&str, Kind)],
    workspace: &Path,
) -> EaiResult<String> {
    forward(
        daemon::adapters,
        action,
        arg,
        keys,
        json!({ "workspace": workspace.display().to_string() }),
    )
}

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
    forwarded_tools! {
        adapter_doctor(adapter_doctor_tool_attr, args::typed::<args::AdapterDoctor>()),
        "Readiness rows for agent, framework, coding-model or MCP adapters (nothing installed, no paid call), as `susi <agents|frameworks|models|mcp> doctor`. Args: kind, optional id",
        |arg, workspace| adapters("doctor", arg, &[KIND, ID], workspace);

        adapter_setup(adapter_setup_tool_attr, args::typed::<args::AdapterId>()),
        "Setup instructions and the effective adapter, as `susi <agents|frameworks|models|mcp> setup`. Args: kind, id",
        |arg, workspace| adapters("setup", arg, &[KIND, ID], workspace);

        adapter_configure(adapter_configure_tool_attr, args::typed::<args::AdapterConfigure>()),
        "Store a host adapter override (agent and framework adapters pass the verified upgrade), as `susi <agents|frameworks|models|mcp> configure`. Args: kind, id, and adapter (object) or file (absolute path)",
        |arg, workspace| forward(
            daemon::adapters,
            "configure",
            arg,
            &[KIND, ID, ("adapter", Kind::Object), ("file", Kind::Str)],
            merged(
                json!({ "workspace": workspace.display().to_string() }),
                policy_paths(arg, &[("file", false)])?,
            ),
        );

        adapter_reset(adapter_reset_tool_attr, args::typed::<args::AdapterId>()),
        "Drop a host adapter override and return the bundled adapter, as `susi <agents|frameworks|models|mcp> reset`. Args: kind, id",
        |arg, workspace| adapters("reset", arg, &[KIND, ID], workspace);

        agents_retry(agents_retry_tool_attr, args::typed::<args::TaskRetry>()),
        "Run a finished external agent task again with its original prompt (refused while it is active or unknown), as `susi agents retry`. Args: task_id, optional workspace",
        |arg, workspace| retry("agents", arg, workspace);

        frameworks_retry(frameworks_retry_tool_attr, args::typed::<args::TaskRetry>()),
        "Run a finished agent-framework task again with its original prompt, as `susi frameworks retry`. Args: task_id, optional workspace",
        |arg, workspace| retry("frameworks", arg, workspace);

        mcp_add(mcp_add_tool_attr, args::typed::<args::McpAdd>()),
        "Admit an MCP server (stdio command or streamable HTTP URL) into the registry and hot-plug it, as `susi mcp-add`. Args: name, command_or_url, optional args",
        |arg, workspace| adapters(
            "mcp_add",
            arg,
            &[
                ("name", Kind::Str),
                ("command_or_url", Kind::Str),
                ("args", Kind::StrArray),
            ],
            workspace,
        );
    }
}

#[cfg(test)]
#[path = "../tests/adapter_tools.rs"]
mod tests;
