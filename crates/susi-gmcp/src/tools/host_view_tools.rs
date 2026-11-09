//! Socket reach for the host views and maintenance commands left after the
//! planned B4 groups (zero-UI plan B4 groups 2, 4, 9, 10 and 13): `susi
//! status --json`, `susi review`, `susi os status|runtime|ecosystem`,
//! `susi os manage model uninstall`, `susi peers add|remove|unban|rekey`,
//! `susi auto status|prime`, `susi deep-scan`, `susi extensions create`,
//! `susi admin reload|provider-contract|intent-ladder` and `susi stop`.
//!
//! Each tool calls the library function its CLI command calls: directly
//! when it lives in a crate this one depends on (`susi_core::peer_admin`,
//! `susi_config::extensions`, the GEMI model facade), otherwise over the
//! daemon's `daemon.host` plane topic (`susi_daemon::host_views` and the
//! view modules it dispatches to). A workspace the caller names stands in
//! for the CLI's working directory and is admitted by the MAC policy.

use serde_json::{json, Value};
use std::path::Path;

use crate::susi_config::extensions;
use crate::susi_core::peer_admin;
use crate::susi_core::plane_bus::{daemon, gemi};
use crate::susi_error::{EaiError, EaiResult};

use super::args;
use super::forward::{forward, policy_workspace, Kind};
use super::CoreTools;

fn host(action: &str, arg: &Value, keys: &[(&str, Kind)]) -> EaiResult<String> {
    forward(daemon::host, action, arg, keys, json!({}))
}

/// A `daemon.host` action that reads a (policy-admitted) workspace.
fn host_in(action: &str, arg: &Value, server: &Path, keys: &[(&str, Kind)]) -> EaiResult<String> {
    let workspace = policy_workspace(arg, "workspace", server, false)?;
    forward(
        daemon::host,
        action,
        arg,
        keys,
        json!({ "workspace": workspace.display().to_string() }),
    )
}

fn pretty(value: &Value) -> EaiResult<String> {
    serde_json::to_string_pretty(value)
        .map_err(|error| EaiError::internal(format!("encode tool result: {error}")))
}

fn required_str<'a>(arg: &'a Value, key: &str) -> EaiResult<&'a str> {
    arg.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| EaiError::config(format!("{key} is required")))
}

/// Run a membership command and return `{ok, lines, notes, error}`: the
/// lines it printed, its notes, and its refusal when it failed (the lines
/// before a refusal still say what happened).
fn membership(run: impl FnOnce(peer_admin::Out<'_>) -> Result<(), String>) -> EaiResult<String> {
    let (result, lines, notes) = peer_admin::collect(run);
    let error = result.as_ref().err().cloned();
    if let (Some(error), true) = (&error, lines.is_empty() && notes.is_empty()) {
        return Err(EaiError::config(error.clone()));
    }
    pretty(&json!({
        "ok": error.is_none(),
        "lines": lines,
        "notes": notes,
        "error": error,
    }))
}

impl CoreTools {
    // ── 2. Operator view ───────────────────────────────────────────────

    crate::tool_def!(operator_status_tool_attr {
        name: "operator_status",
        description: "The executable's measured identity (sha256), daemon, local model, missions in flight, resumable missions, approvals and the scheduled queue, as `susi status --json`; with detail true, the whole `susi status` view (missions with prompts, resumable DAG nodes, rollback snapshots, brain leaders and unfit providers, routing, spend, approvals). Args: optional workspace, detail",
        input_schema: args::typed::<args::OperatorStatus>(),
    });
    pub fn operator_status(arg: &Value, workspace: &Path) -> EaiResult<String> {
        host_in("status", arg, workspace, &[("detail", Kind::Bool)])
    }

    crate::tool_def!(operator_review_tool_attr {
        name: "operator_review",
        description:
            "Daemon liveness and reclaimable OS cache, as `susi review`. Args: optional workspace",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn operator_review(arg: &Value, workspace: &Path) -> EaiResult<String> {
        host_in("review", arg, workspace, &[])
    }

    // ── 4. Onboarding ──────────────────────────────────────────────────

    crate::tool_def!(os_ecosystem_tool_attr {
        name: "os_ecosystem",
        description: "The local AI ecosystem: hardware, runtimes, models, agent and framework readiness, MCP servers and running AI processes, as `susi os ecosystem --json`. Args: optional workspace",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn os_ecosystem(arg: &Value, workspace: &Path) -> EaiResult<String> {
        host_in("os_ecosystem", arg, workspace, &[])
    }

    // ── 9. OS ──────────────────────────────────────────────────────────

    crate::tool_def!(os_status_tool_attr {
        name: "os_status",
        description: "Substrate status: consensus, ledger, services, peers, endpoints, storage and warnings, as `susi os status --json`",
        input_schema: args::no_args(),
    });
    pub fn os_status(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        host("os_status", arg, &[])
    }

    crate::tool_def!(os_runtime_tool_attr {
        name: "os_runtime",
        description: "Live AI processes by owner (SUSI or external), managed models with daemon residency, and per-process GPU memory, as `susi os runtime --json`",
        input_schema: args::no_args(),
    });
    pub fn os_runtime(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        host("os_runtime", arg, &[])
    }

    crate::tool_def!(model_uninstall_tool_attr {
        name: "model_uninstall",
        description: "Remove one SUSI-managed .gguf model (refused while loaded or selected), as `susi os manage model uninstall`. Args: name",
        input_schema: args::typed::<args::ModelUninstall>(),
    });
    pub fn model_uninstall(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        host("model_uninstall", arg, &[("name", Kind::Str)])
    }

    crate::tool_def!(peers_add_tool_attr {
        name: "peers_add",
        description: "Join a peer by signed handshake and commit its admission to the replicated ledger, as `susi peers add`. Args: host, optional port",
        input_schema: args::typed::<args::PeersAdd>(),
    });
    pub fn peers_add(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        let host = required_str(arg, "host")?.to_string();
        let port = match arg.get("port") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .and_then(|p| u16::try_from(p).ok())
                    .ok_or_else(|| EaiError::config("port must be 0-65535"))?,
            ),
        };
        membership(|out| peer_admin::add(out, &host, port))
    }

    crate::tool_def!(peers_remove_tool_attr {
        name: "peers_remove",
        description: "Evict a verified peer (revoke trust and ban re-verification), committed cluster-wide, as `susi peers remove`. Args: peer",
        input_schema: args::typed::<args::PeerName>(),
    });
    pub fn peers_remove(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        let peer = required_str(arg, "peer")?.to_string();
        membership(|out| peer_admin::remove(out, &peer))
    }

    crate::tool_def!(peers_unban_tool_attr {
        name: "peers_unban",
        description: "Lift a ban so an evicted peer can re-verify, committed cluster-wide, as `susi peers unban`. Args: peer",
        input_schema: args::typed::<args::PeerName>(),
    });
    pub fn peers_unban(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        let peer = required_str(arg, "peer")?.to_string();
        membership(|out| peer_admin::unban(out, &peer))
    }

    crate::tool_def!(peers_rekey_tool_attr {
        name: "peers_rekey",
        description: "Rotate the cluster key across every verified member, two-phase (no key material is returned), as `susi peers rekey`. Args: optional force",
        input_schema: args::typed::<args::PeersRekey>(),
    });
    pub fn peers_rekey(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        let force = match arg.get("force") {
            None | Some(Value::Null) => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| EaiError::config("force must be a boolean"))?,
        };
        membership(|out| peer_admin::rekey(out, force))
    }

    // ── 10. Lifecycle ──────────────────────────────────────────────────

    crate::tool_def!(daemon_shutdown_tool_attr {
        name: "daemon_shutdown",
        description: "Stop this daemon instance after replying (the call `susi stop` makes); starting it again stays with the CLI",
        input_schema: args::no_args(),
    });
    pub fn daemon_shutdown(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        host("daemon_shutdown", arg, &[])
    }

    // ── 13. Maintenance ────────────────────────────────────────────────

    crate::tool_def!(extensions_create_tool_attr {
        name: "extensions_create",
        description: "Create an empty extension pack shell under the host extensions root (not activated), as `susi extensions create`. Args: id",
        input_schema: args::typed::<args::ExtensionPackId>(),
    });
    pub fn extensions_create(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        let status = extensions::create_pack(required_str(arg, "id")?).map_err(EaiError::config)?;
        let doc = serde_json::to_value(status)
            .map_err(|error| EaiError::internal(format!("encode pack: {error}")))?;
        pretty(&doc)
    }

    crate::tool_def!(auto_status_tool_attr {
        name: "auto_status",
        description: "Zero-config substrate readiness: packs, leading MCP, preferred coding model, agent and framework readiness, last prime, as `susi auto status`. Args: optional workspace",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn auto_status(arg: &Value, workspace: &Path) -> EaiResult<String> {
        host_in("auto_status", arg, workspace, &[])
    }

    crate::tool_def!(auto_prime_tool_attr {
        name: "auto_prime",
        description: "Re-run auto-prime now (models and installed peers; never enables MCP), as `susi auto prime`",
        input_schema: args::no_args(),
    });
    pub fn auto_prime(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        host("auto_prime", arg, &[])
    }

    crate::tool_def!(deep_scan_tool_attr {
        name: "deep_scan",
        description:
            "Scan the home directory for model files and register them, as `susi deep-scan`",
        input_schema: args::no_args(),
    });
    pub fn deep_scan(_arg: &Value, _workspace: &Path) -> EaiResult<String> {
        let reply =
            gemi::ModelManager::deep_scan_home_and_register(&susi_paths::SusiDirs::config_dir());
        if let Some(error) = reply.get("error").and_then(Value::as_str) {
            return Err(EaiError::filesystem(error.to_string()));
        }
        Ok(reply
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    crate::tool_def!(config_reload_tool_attr {
        name: "config_reload",
        description: "Reload config.json and report the engine, model, model-ladder steps and MCP bootstrap servers in effect, as `susi admin reload`",
        input_schema: args::no_args(),
    });
    pub fn config_reload(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        host("config_reload", arg, &[])
    }

    crate::tool_def!(provider_contract_tool_attr {
        name: "provider_contract",
        description: "Provider contract fixture checks, plus the read-only credentialed claims when SUSI_PROVIDER_PROBE=1, as `susi admin provider-contract`",
        input_schema: args::no_args(),
    });
    pub fn provider_contract(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        host("provider_contract", arg, &[])
    }

    crate::tool_def!(intent_ladder_tool_attr {
        name: "intent_ladder",
        description: "The intent ladder: last scorecard, a full or start-up-slice run, one intent's verification, the failing-rung outbox, or adopting a gap as a repair task, as `susi admin intent-ladder`. Args: optional mode, id, workspace",
        input_schema: args::typed::<args::IntentLadderArgs>(),
    });
    pub fn intent_ladder(arg: &Value, workspace: &Path) -> EaiResult<String> {
        // `adopt_gap` authors a task in the checkout: it needs write.
        let write = arg.get("mode").and_then(Value::as_str) == Some("adopt_gap");
        let workspace = policy_workspace(arg, "workspace", workspace, write)?;
        forward(
            daemon::host,
            "intent_ladder",
            arg,
            &[("mode", Kind::Str), ("id", Kind::Str)],
            json!({ "workspace": workspace.display().to_string() }),
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
            Ok(json!({ "payload": payload }))
        }
    }

    fn sent(result: EaiResult<String>) -> Value {
        serde_json::from_str::<Value>(&result.unwrap()).unwrap()["payload"].clone()
    }

    #[test]
    fn host_tools_forward_actions_and_policy_workspaces() {
        PlaneBus::global().register(topics::DAEMON_HOST, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let ws_text = ws.display().to_string();
        assert_eq!(
            sent(CoreTools::operator_status(&json!({}), &ws)),
            json!({"action": "status", "workspace": ws_text})
        );
        assert_eq!(
            sent(CoreTools::operator_status(&json!({"detail": true}), &ws))["detail"],
            true
        );
        assert!(CoreTools::operator_status(&json!({"detail": "yes"}), &ws).is_err());
        assert_eq!(
            sent(CoreTools::os_status(&json!({}), &ws)),
            json!({"action": "os_status"})
        );
        assert_eq!(
            sent(CoreTools::model_uninstall(&json!({"name": "m.gguf"}), &ws))["name"],
            "m.gguf"
        );
        let ladder = sent(CoreTools::intent_ladder(
            &json!({"mode": "verify", "id": "I-1"}),
            &ws,
        ));
        assert_eq!(ladder["mode"], "verify");
        assert_eq!(ladder["workspace"], ws_text);
        assert!(
            CoreTools::os_ecosystem(&json!({"workspace": "relative"}), &ws)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        assert_eq!(
            sent(CoreTools::daemon_shutdown(&json!({}), &ws)),
            json!({"action": "daemon_shutdown"})
        );
    }

    #[test]
    fn membership_tools_validate_before_touching_the_roster() {
        let ws = Path::new(".");
        assert!(CoreTools::peers_add(&json!({}), ws).is_err());
        assert!(
            CoreTools::peers_add(&json!({"host": "h", "port": 70000}), ws)
                .unwrap_err()
                .to_string()
                .contains("port")
        );
        assert!(CoreTools::peers_remove(&json!({"peer": ""}), ws).is_err());
        assert!(CoreTools::peers_rekey(&json!({"force": "yes"}), ws).is_err());
        assert!(CoreTools::extensions_create(&json!({}), ws).is_err());
    }

    #[test]
    fn membership_returns_lines_and_the_refusal() {
        let done = membership(|out| {
            out(peer_admin::Line::Info("evicted".into()));
            out(peer_admin::Line::Note("note".into()));
            Ok(())
        })
        .unwrap();
        let doc: Value = serde_json::from_str(&done).unwrap();
        assert_eq!(doc["ok"], true);
        assert_eq!(doc["lines"], json!(["evicted"]));
        assert_eq!(doc["notes"], json!(["note"]));
        let partial = membership(|out| {
            out(peer_admin::Line::Note("staged 0/1".into()));
            Err("rekey aborted".into())
        })
        .unwrap();
        let doc: Value = serde_json::from_str(&partial).unwrap();
        assert_eq!(doc["ok"], false);
        assert_eq!(doc["error"], "rekey aborted");
        assert!(membership(|_| Err("no verified peer".into())).is_err());
    }
}
