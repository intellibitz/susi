//! Socket reach for CLI control-plane commands (zero-UI plan B4 groups 2,
//! 6, 11 and 12): the operator view, missions, extension packs, the runtime
//! model cache, placement decisions and the context graph.
//!
//! Every handler calls the same library function its CLI command calls
//! (`susi top --json`, `susi plan`, `susi pulse`, `susi blackboard show`,
//! `susi extensions *`, `susi context-graph stats|show`, `susi os manage
//! model load|unload`) calls, and returns the same JSON document. Capabilities owned by another
//! feature plane (GEMI's operator sampling, GAWD's planning loop and intent
//! ledger) are reached over the plane bus, as every other cross-plane tool
//! here is.

use std::path::{Path, PathBuf};

use crate::susi_config::extensions;
use crate::susi_core::context_graph::ContextGraph;
use crate::susi_core::native_board::{
    check_board, list_items, post_blackboard_local, post_item, resolve_agent, resolve_board_target,
    resolve_host, BlackboardPostRequest, BoardDraft,
};
use crate::susi_core::plane_bus::{gawd, gemi};
use crate::susi_error::{EaiError, EaiResult};
use crate::susi_sandbox::placement_audit::lookup_placement_decision;

use super::args;
use super::CoreTools;

/// `susi context-graph show`'s default `--limit`.
const CONTEXT_GRAPH_SHOW_LIMIT: usize = 20;
/// `susi plan`'s default `--max-steps`.
const PLAN_MAX_STEPS: u32 = 3;

fn pretty(value: &serde_json::Value) -> EaiResult<String> {
    serde_json::to_string_pretty(value)
        .map_err(|error| EaiError::internal(format!("encode tool result: {error}")))
}

/// A required, non-empty string argument.
fn required_str<'a>(arg: &'a serde_json::Value, key: &str) -> EaiResult<&'a str> {
    arg.get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| EaiError::config(format!("{key} is required")))
}

/// The first non-empty string among `keys` (a canonical key, then aliases).
fn first_str<'a>(arg: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        arg.get(*key)
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    })
}

/// An optional absolute path argument. The socket has no caller working
/// directory, so a relative path has no meaning and is refused.
fn absolute_path_arg(arg: &serde_json::Value, key: &str) -> EaiResult<Option<PathBuf>> {
    let Some(value) = arg.get(key) else {
        return Ok(None);
    };
    let text = value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| EaiError::config(format!("{key} must be a non-empty string")))?;
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(EaiError::config(format!(
            "{key} must be an absolute path (the socket has no caller working directory)"
        )));
    }
    Ok(Some(path))
}

impl CoreTools {
    // ── 2. Operator view ───────────────────────────────────────────────

    crate::tool_def!(operator_snapshot_tool_attr {
        name: "operator_snapshot",
        description: "Unified operator state (workload, resources, providers, config, federation, evidence) for the server workspace: the same document as `susi top --json`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn operator_snapshot(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        let state = gemi::operator_snapshot(workspace)
            .map_err(|e| EaiError::internal(format!("operator snapshot: {e}")))?;
        pretty(&state)
    }

    // ── 11. Extension packs ────────────────────────────────────────────

    crate::tool_def!(extensions_list_tool_attr {
        name: "extensions_list",
        description: "List installed extension packs: the same document as `susi extensions list`",
        input_schema: args::no_args(),
    });
    pub fn extensions_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let packs = serde_json::to_value(extensions::list_packs())
            .map_err(|e| EaiError::internal(format!("encode packs: {e}")))?;
        pretty(&packs)
    }

    crate::tool_def!(extensions_status_tool_attr {
        name: "extensions_status",
        description: "Active extension pack, its root, and every installed pack: the same document as `susi extensions status`",
        input_schema: args::no_args(),
    });
    pub fn extensions_status(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&extensions::status_report())
    }

    crate::tool_def!(extensions_validate_tool_attr {
        name: "extensions_validate",
        description: "Check a pack directory against the host's manifest, path containment, signature and SHA-256 checksum rules without mutating host state, as `susi extensions validate`. Args: {path: pack directory, absolute or relative to the server workspace}",
        input_schema: args::typed::<args::ExtensionsValidate>(),
    });
    pub fn extensions_validate(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The socket has no caller working directory: a relative path
        // resolves against the server workspace.
        let raw = first_str(arg, &["path", "dir", "root"])
            .ok_or_else(|| EaiError::config("path is required"))?;
        let path = if Path::new(raw).is_absolute() {
            PathBuf::from(raw)
        } else {
            workspace.join(raw)
        };
        let report = extensions::validate_report(&path)
            .map_err(|e| EaiError::config(format!("pack rejected: {e}")))?;
        pretty(&report)
    }

    crate::tool_def!(extensions_load_tool_attr {
        name: "extensions_load",
        description:
            "Verify and activate an installed extension pack, as `susi extensions load`. Args: {id}",
        input_schema: args::typed::<args::ExtensionPackId>(),
    });
    pub fn extensions_load(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = first_str(arg, &["id", "pack", "name"])
            .ok_or_else(|| EaiError::config("id is required"))?;
        let report = extensions::load_report(id).map_err(EaiError::config)?;
        pretty(&report)
    }

    crate::tool_def!(extensions_unload_tool_attr {
        name: "extensions_unload",
        description: "Deactivate an extension pack (files kept; compiled defaults serve), as `susi extensions unload`. Args: {id}",
        input_schema: args::typed::<args::ExtensionPackId>(),
    });
    pub fn extensions_unload(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let id = first_str(arg, &["id", "pack", "name"])
            .ok_or_else(|| EaiError::config("id is required"))?;
        let report = extensions::unload_report(id).map_err(EaiError::config)?;
        pretty(&report)
    }

    // ── 6. Missions ────────────────────────────────────────────────────

    crate::tool_def!(plan_run_tool_attr {
        name: "plan_run",
        description: "Solve a goal with the autonomous planning loop in the server workspace, as `susi plan`; returns status, final_answer and agents. Args: {goal, optional max_steps (default 3)}. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::PlanRun>(),
    });
    pub fn plan_run(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let goal = required_str(arg, "goal")?;
        let max_steps = match arg.get("max_steps") {
            None => PLAN_MAX_STEPS,
            Some(value) => value
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(|| EaiError::config("max_steps must be a non-negative integer"))?,
        };
        let summary = gawd::plan_autonomous(goal, workspace, env!("CARGO_PKG_VERSION"), max_steps)
            .map_err(|e| EaiError::internal(format!("plan: {e}")))?;
        pretty(&summary)
    }

    crate::tool_def!(intent_ingest_tool_attr {
        name: "intent_ingest",
        description: "Ingest a natural-language intent into the server workspace's evidence ledger, as `susi pulse`. Args: {intent}. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::IntentIngest>(),
    });
    pub fn intent_ingest(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, true)?;
        let intent = required_str(arg, "intent")?;
        gawd::ingest_intent(intent, workspace)
            .map_err(|e| EaiError::internal(format!("pulse ingestion: {e}")))
    }

    crate::tool_def!(blackboard_show_tool_attr {
        name: "blackboard_show",
        description: "The last mission blackboard (swarm shared state, credential-redacted) in the server workspace, as `susi blackboard show`. Optional workspace: the absolute directory the CLI would run in, admitted by the MAC policy (default: the server workspace).",
        input_schema: args::typed::<args::WorkspaceView>(),
    });
    pub fn blackboard_show(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        // The CLI runs this in its working directory; the socket
        // takes an optional, MAC-policy-admitted `workspace`.
        let workspace = &super::forward::policy_workspace(arg, "workspace", workspace, false)?;
        crate::susi_core::agent_types::read_persisted_blackboard(workspace)
            .map_err(EaiError::config)
    }

    // ── 12. Runtime model cache and placement (replaced 9091 routes) ────

    crate::tool_def!(runtime_models_list_tool_attr {
        name: "runtime_models_list",
        description: "Models loaded in the inference cache: the daemon-loaded document `susi os runtime` reports",
        input_schema: args::no_args(),
    });
    pub fn runtime_models_list(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        pretty(&gemi::loaded_models())
    }

    crate::tool_def!(runtime_models_load_tool_attr {
        name: "runtime_models_load",
        description:
            "Load a model into the inference cache, as `susi os manage model load`. Args: {model}",
        input_schema: args::typed::<args::RuntimeModel>(),
    });
    pub fn runtime_models_load(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let model = required_str(arg, "model")?;
        let payload = gemi::preload_model(model).map_err(EaiError::inference)?;
        pretty(&payload)
    }

    crate::tool_def!(runtime_models_unload_tool_attr {
        name: "runtime_models_unload",
        description: "Evict a model from the inference cache, as `susi os manage model unload`. Args: {model}",
        input_schema: args::typed::<args::RuntimeModel>(),
    });
    pub fn runtime_models_unload(arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let model = required_str(arg, "model")?;
        let payload = gemi::unload_model(model).map_err(EaiError::inference)?;
        pretty(&payload)
    }

    crate::tool_def!(placement_decision_tool_attr {
        name: "placement_decision",
        description: "The audited inference-placement decision with this id, plus its audit entry hash (the id a completion returns in X-Susi-Placement-Id). Args: {decision_id}",
        input_schema: args::typed::<args::PlacementDecision>(),
    });
    pub fn placement_decision(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let id = required_str(arg, "decision_id")?;
        let record =
            lookup_placement_decision(workspace, id).map_err(|e| EaiError::config(e.message()))?;
        pretty(&record)
    }

    crate::tool_def!(context_graph_stats_tool_attr {
        name: "context_graph_stats",
        description: "Universal Context Graph node and edge counts by kind: the same document as `susi context-graph stats` and 9091 GET /context-graph/stats",
        input_schema: args::no_args(),
    });
    pub fn context_graph_stats(_arg: &serde_json::Value, _workspace: &Path) -> EaiResult<String> {
        let graph = ContextGraph::global();
        let _ = graph.replay();
        let stats = serde_json::to_value(graph.stats())
            .map_err(|e| EaiError::internal(format!("encode graph stats: {e}")))?;
        pretty(&stats)
    }

    crate::tool_def!(context_graph_show_tool_attr {
        name: "context_graph_show",
        description: "The most recent Universal Context Graph events for a workspace: the same document as `susi context-graph show`. Args: optional limit (default 20), optional absolute workspace (default: the server workspace)",
        input_schema: args::typed::<args::ContextGraphShow>(),
    });
    pub fn context_graph_show(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let limit = match arg.get("limit") {
            None => CONTEXT_GRAPH_SHOW_LIMIT,
            Some(value) => value
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| EaiError::config("limit must be a non-negative integer"))?,
        };
        let target =
            absolute_path_arg(arg, "workspace")?.unwrap_or_else(|| workspace.to_path_buf());
        let graph = ContextGraph::global();
        let _ = graph.replay();
        pretty(&graph.workspace_show(&target, limit))
    }

    crate::tool_def!(blackboard_post_tool_attr {
        name: "blackboard_post",
        description: "Post a key-value update to the shared blackboard and mirror it onto the Susi Native Board by default",
        input_schema: args::typed::<args::BlackboardPost>(),
    });
    pub fn blackboard_post(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let key = required_str(arg, "key")?.to_owned();
        let value = arg
            .get("value")
            .cloned()
            .ok_or_else(|| EaiError::config("value is required"))?;
        let target_ws =
            absolute_path_arg(arg, "workspace")?.unwrap_or_else(|| workspace.to_path_buf());
        let req = BlackboardPostRequest {
            key,
            value,
            author: first_str(arg, &["author"]).map(str::to_owned),
            task: first_str(arg, &["task"]).map(str::to_owned),
            mirror_to_board: true,
            kind: first_str(arg, &["kind"]).map(str::to_owned),
            title: first_str(arg, &["title"]).map(str::to_owned),
        };
        let entry = post_blackboard_local(Some(&target_ws), &req).map_err(EaiError::governance)?;
        pretty(&serde_json::to_value(&entry).map_err(|e| EaiError::internal(e.to_string()))?)
    }

    crate::tool_def!(board_list_tool_attr {
        name: "board_list",
        description: "List collaborative items on the Susi Native Board",
        input_schema: args::typed::<args::BoardList>(),
    });
    pub fn board_list(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let target_ws =
            absolute_path_arg(arg, "workspace")?.unwrap_or_else(|| workspace.to_path_buf());
        let target = resolve_board_target(Some(&target_ws)).map_err(EaiError::governance)?;
        let (mut items, findings) = list_items(&target).map_err(EaiError::governance)?;
        if let Some(kind) = first_str(arg, &["kind"]) {
            items.retain(|s| s.item.kind == kind);
        }
        if let Some(limit) = arg.get("limit").and_then(|v| v.as_u64()) {
            let lim = usize::try_from(limit).unwrap_or(usize::MAX);
            if items.len() > lim {
                items = items.split_off(items.len() - lim);
            }
        }
        pretty(&serde_json::json!({ "items": items, "findings": findings }))
    }

    crate::tool_def!(board_check_tool_attr {
        name: "board_check",
        description: "Check the Susi Native Board for integrity, open stops, and unanswered requests (fail-closed)",
        input_schema: args::typed::<args::BoardCheck>(),
    });
    pub fn board_check(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let target_ws =
            absolute_path_arg(arg, "workspace")?.unwrap_or_else(|| workspace.to_path_buf());
        let agent = resolve_agent(first_str(arg, &["agent"]), Some(&target_ws))
            .map_err(EaiError::config)?;
        let allow_empty = arg
            .get("allow_empty")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let target = resolve_board_target(Some(&target_ws)).map_err(EaiError::governance)?;
        let status = check_board(&target, &agent, allow_empty).map_err(EaiError::governance)?;
        pretty(&serde_json::to_value(&status).map_err(|e| EaiError::internal(e.to_string()))?)
    }

    crate::tool_def!(board_post_tool_attr {
        name: "board_post",
        description: "Post a collaborative coordination item to the Susi Native Board",
        input_schema: args::typed::<args::BoardPost>(),
    });
    pub fn board_post(arg: &serde_json::Value, workspace: &Path) -> EaiResult<String> {
        let target_ws =
            absolute_path_arg(arg, "workspace")?.unwrap_or_else(|| workspace.to_path_buf());
        let agent = resolve_agent(first_str(arg, &["agent"]), Some(&target_ws))
            .map_err(EaiError::config)?;
        let draft = BoardDraft {
            kind: required_str(arg, "kind")?.to_owned(),
            agent,
            host: resolve_host(None),
            title: required_str(arg, "title")?.to_owned(),
            body: arg
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned(),
            to: first_str(arg, &["to"]).map(str::to_owned),
            re: arg.get("re").and_then(|v| v.as_u64()),
            task: first_str(arg, &["task"]).map(str::to_owned),
        };
        let target = resolve_board_target(Some(&target_ws)).map_err(EaiError::governance)?;
        let stored = post_item(&target, &draft, None).map_err(EaiError::governance)?;
        pretty(&serde_json::to_value(&stored).map_err(|e| EaiError::internal(e.to_string()))?)
    }
}

#[cfg(test)]
#[path = "../tests/control_plane.rs"]
mod tests;
