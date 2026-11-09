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
    crate::tool_def!(adapter_doctor_tool_attr {
        name: "adapter_doctor",
        description: "Readiness rows for agent, framework, coding-model or MCP adapters (nothing installed, no paid call), as `susi <agents|frameworks|models|mcp> doctor`. Args: kind, optional id",
        input_schema: args::typed::<args::AdapterDoctor>(),
    });
    pub fn adapter_doctor(arg: &Value, workspace: &Path) -> EaiResult<String> {
        adapters("doctor", arg, &[KIND, ID], workspace)
    }

    crate::tool_def!(adapter_setup_tool_attr {
        name: "adapter_setup",
        description: "Setup instructions and the effective adapter, as `susi <agents|frameworks|models|mcp> setup`. Args: kind, id",
        input_schema: args::typed::<args::AdapterId>(),
    });
    pub fn adapter_setup(arg: &Value, workspace: &Path) -> EaiResult<String> {
        adapters("setup", arg, &[KIND, ID], workspace)
    }

    crate::tool_def!(adapter_configure_tool_attr {
        name: "adapter_configure",
        description: "Store a host adapter override (agent and framework adapters pass the verified upgrade), as `susi <agents|frameworks|models|mcp> configure`. Args: kind, id, and adapter (object) or file (absolute path)",
        input_schema: args::typed::<args::AdapterConfigure>(),
    });
    pub fn adapter_configure(arg: &Value, workspace: &Path) -> EaiResult<String> {
        forward(
            daemon::adapters,
            "configure",
            arg,
            &[KIND, ID, ("adapter", Kind::Object), ("file", Kind::Str)],
            merged(
                json!({ "workspace": workspace.display().to_string() }),
                policy_paths(arg, &[("file", false)])?,
            ),
        )
    }

    crate::tool_def!(adapter_reset_tool_attr {
        name: "adapter_reset",
        description: "Drop a host adapter override and return the bundled adapter, as `susi <agents|frameworks|models|mcp> reset`. Args: kind, id",
        input_schema: args::typed::<args::AdapterId>(),
    });
    pub fn adapter_reset(arg: &Value, workspace: &Path) -> EaiResult<String> {
        adapters("reset", arg, &[KIND, ID], workspace)
    }

    crate::tool_def!(agents_retry_tool_attr {
        name: "agents_retry",
        description: "Run a finished external agent task again with its original prompt (refused while it is active or unknown), as `susi agents retry`. Args: task_id, optional workspace",
        input_schema: args::typed::<args::TaskRetry>(),
    });
    pub fn agents_retry(arg: &Value, workspace: &Path) -> EaiResult<String> {
        retry("agents", arg, workspace)
    }

    crate::tool_def!(frameworks_retry_tool_attr {
        name: "frameworks_retry",
        description: "Run a finished agent-framework task again with its original prompt, as `susi frameworks retry`. Args: task_id, optional workspace",
        input_schema: args::typed::<args::TaskRetry>(),
    });
    pub fn frameworks_retry(arg: &Value, workspace: &Path) -> EaiResult<String> {
        retry("frameworks", arg, workspace)
    }

    crate::tool_def!(mcp_add_tool_attr {
        name: "mcp_add",
        description: "Admit an MCP server (stdio command or streamable HTTP URL) into the registry and hot-plug it, as `susi mcp-add`. Args: name, command_or_url, optional args",
        input_schema: args::typed::<args::McpAdd>(),
    });
    pub fn mcp_add(arg: &Value, workspace: &Path) -> EaiResult<String> {
        adapters(
            "mcp_add",
            arg,
            &[
                ("name", Kind::Str),
                ("command_or_url", Kind::Str),
                ("args", Kind::StrArray),
            ],
            workspace,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
    use std::sync::Arc;

    struct Echo;
    impl PlaneHandler for Echo {
        fn handle(&self, _topic: &str, payload: Value) -> Result<Value, String> {
            if payload["id"] == "refused" {
                return Err("refused by plane".to_string());
            }
            Ok(json!({ "payload": payload }))
        }
    }

    fn sent(result: EaiResult<String>) -> Value {
        serde_json::from_str::<Value>(&result.unwrap()).unwrap()["payload"].clone()
    }

    #[test]
    fn adapter_tools_forward_kind_id_and_the_server_workspace() {
        PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let ws_text = ws.display().to_string();
        let doctor = sent(CoreTools::adapter_doctor(&json!({"kind": "mcp"}), &ws));
        assert_eq!(
            doctor,
            json!({"action": "doctor", "kind": "mcp", "workspace": ws_text})
        );
        let configure = sent(CoreTools::adapter_configure(
            &json!({"kind": "agents", "id": "a", "adapter": {"type": "process"}}),
            &ws,
        ));
        assert_eq!(configure["adapter"], json!({"type": "process"}));
        assert_eq!(
            sent(CoreTools::adapter_reset(
                &json!({"kind": "models", "id": "m"}),
                &ws
            ))["action"],
            "reset"
        );
        let retried = sent(CoreTools::frameworks_retry(&json!({"task_id": "t1"}), &ws));
        assert_eq!(retried["kind"], "frameworks");
        assert_eq!(retried["task_id"], "t1");
        assert_eq!(retried["workspace"], ws_text);
        let added = sent(CoreTools::mcp_add(
            &json!({"name": "n", "command_or_url": "https://mcp.example/x", "args": ["-v"]}),
            &ws,
        ));
        assert_eq!(added["args"], json!(["-v"]));
        assert!(CoreTools::mcp_add(&json!({"name": "n", "args": "-v"}), &ws).is_err());
        assert!(
            CoreTools::agents_retry(&json!({"task_id": "t", "workspace": "rel"}), &ws)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        assert!(CoreTools::adapter_configure(
            &json!({"kind": "agents", "id": "a", "file": "rel.json"}),
            &ws
        )
        .unwrap_err()
        .to_string()
        .contains("absolute"));
        let refused = CoreTools::adapter_setup(&json!({"kind": "mcp", "id": "refused"}), &ws);
        assert!(refused
            .unwrap_err()
            .to_string()
            .contains("refused by plane"));
    }

    #[test]
    fn adapter_setup_and_agents_retry_forward_expected_actions_and_workspace() {
        PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let setup = sent(CoreTools::adapter_setup(
            &json!({"kind": "agents", "id": "claude"}),
            &ws,
        ));
        assert_eq!(setup["action"], "setup");
        assert_eq!(setup["kind"], "agents");
        assert_eq!(setup["id"], "claude");

        let retried = sent(CoreTools::agents_retry(&json!({"task_id": "t-42"}), &ws));
        assert_eq!(retried["action"], "retry");
        assert_eq!(retried["kind"], "agents");
        assert_eq!(retried["task_id"], "t-42");
    }

    #[test]
    fn adapter_configure_resolves_granted_absolute_file_path() {
        PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let file = ws.join("susi-adapter-override.json");
        let configured = sent(CoreTools::adapter_configure(
            &json!({
                "kind": "frameworks",
                "id": "autogen",
                "file": file.display().to_string()
            }),
            &ws,
        ));
        assert_eq!(configured["action"], "configure");
        assert!(
            Path::new(configured["file"].as_str().unwrap()).ends_with("susi-adapter-override.json")
        );
    }

    #[test]
    fn mcp_add_forwards_url_without_optional_args_and_rejects_bad_types() {
        PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let added = sent(CoreTools::mcp_add(
            &json!({"name": "local", "command_or_url": "https://mcp.local/sse"}),
            &ws,
        ));
        assert_eq!(added["action"], "mcp_add");
        assert_eq!(added["name"], "local");
        assert!(added.get("args").is_none());
        assert!(CoreTools::adapter_doctor(&json!({"kind": 123}), &ws).is_err());
    }

    #[test]
    fn adapter_reset_and_frameworks_retry_reject_mistyped_or_relative_arguments() {
        PlaneBus::global().register(topics::DAEMON_ADAPTERS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        assert!(CoreTools::adapter_reset(&json!({"kind": "mcp", "id": 99}), &ws).is_err());
        assert!(CoreTools::frameworks_retry(
            &json!({"task_id": "t1", "workspace": "relative/ws"}),
            &ws
        )
        .is_err());
    }
}
