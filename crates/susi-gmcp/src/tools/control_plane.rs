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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::susi_core::plane_bus::{topics, PlaneBus, PlaneHandler};
    use serde_json::json;
    use std::sync::Arc;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "susi-control-plane-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Echoes the topic and payload, so a test sees exactly what a tool
    /// sent over the plane bus.
    struct Echo;
    impl PlaneHandler for Echo {
        fn handle(
            &self,
            topic: &str,
            payload: serde_json::Value,
        ) -> Result<serde_json::Value, String> {
            if topic == topics::GAWD_PULSE_INGEST {
                return Ok(json!({ "text": format!("ingested {}", payload["intent"]) }));
            }
            Ok(json!({ "topic": topic, "payload": payload }))
        }
    }

    /// Run `f` with an isolated substrate root (`SUSI_HOME`) so pack state is
    /// never the host's own, holding the env lock other gmcp tests share.
    fn with_isolated_home(f: impl FnOnce(&Path)) {
        let _lock = crate::susi_core::commit_log::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let home = temp_dir("home");
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("SUSI_HOME", &home);
        env.remove("SUSI_EXTENSION_PACK");
        extensions::invalidate_extension_caches();
        f(&home);
        drop(env);
        extensions::invalidate_extension_caches();
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn extension_tools_return_the_cli_documents_and_change_pack_state() {
        with_isolated_home(|home| {
            let ws = Path::new(".");
            let status: serde_json::Value =
                serde_json::from_str(&CoreTools::extensions_status(&json!({}), ws).unwrap())
                    .unwrap();
            assert_eq!(status, extensions::status_report());
            assert_eq!(status["active"], "default");

            let created = extensions::create_pack("acme").unwrap();
            assert!(
                created.root.starts_with(home),
                "pack must live under the isolated home: {}",
                created.root.display()
            );
            let listed: serde_json::Value =
                serde_json::from_str(&CoreTools::extensions_list(&json!({}), ws).unwrap()).unwrap();
            assert!(
                listed
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|p| p["id"] == "acme" && p["seeded"] == true),
                "{listed}"
            );

            let loaded: serde_json::Value = serde_json::from_str(
                &CoreTools::extensions_load(&json!({"id": "acme"}), ws).unwrap(),
            )
            .unwrap();
            assert_eq!(loaded["loaded"]["id"], "acme");
            assert_eq!(loaded["active"], "acme");

            let unloaded: serde_json::Value = serde_json::from_str(
                &CoreTools::extensions_unload(&json!({"id": "acme"}), ws).unwrap(),
            )
            .unwrap();
            assert_eq!(unloaded, json!({"unloaded": "acme", "active": "default"}));

            assert!(CoreTools::extensions_load(&json!({"id": "missing-pack"}), ws).is_err());
            assert!(CoreTools::extensions_load(&json!({}), ws).is_err());
            assert!(CoreTools::extensions_unload(&json!({"id": " "}), ws).is_err());
        });
    }

    #[test]
    fn extensions_validate_checks_absolute_and_workspace_relative_pack_dirs() {
        with_isolated_home(|_| {
            let ws = Path::new(".");
            let pack = extensions::create_pack("authored").unwrap();
            let report: serde_json::Value = serde_json::from_str(
                &CoreTools::extensions_validate(
                    &json!({"path": pack.root.display().to_string()}),
                    ws,
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(report, extensions::validate_report(&pack.root).unwrap());
            assert_eq!(report["valid"], true);
            assert_eq!(report["id"], "authored");

            // A relative path resolves against the server workspace.
            let parent = pack.root.parent().unwrap();
            let relative: serde_json::Value = serde_json::from_str(
                &CoreTools::extensions_validate(&json!({"path": "authored"}), parent).unwrap(),
            )
            .unwrap();
            assert_eq!(relative, report);
            assert!(CoreTools::extensions_validate(&json!({"path": "authored"}), ws).is_err());
            assert!(CoreTools::extensions_validate(&json!({}), ws).is_err());
            let empty = temp_dir("empty-pack");
            assert!(CoreTools::extensions_validate(
                &json!({"path": empty.display().to_string()}),
                ws
            )
            .is_err());
            let _ = std::fs::remove_dir_all(&empty);
        });
    }

    #[test]
    fn plane_backed_tools_forward_the_server_workspace() {
        let bus = PlaneBus::global();
        for topic in [
            topics::GEMI_OPERATOR_SNAPSHOT,
            topics::GAWD_PLAN_AUTONOMOUS,
            topics::GAWD_PULSE_INGEST,
        ] {
            bus.register(topic, Arc::new(Echo));
        }
        let ws = temp_dir("plane");
        let ws_text = ws.display().to_string();

        let snapshot: serde_json::Value =
            serde_json::from_str(&CoreTools::operator_snapshot(&json!({}), &ws).unwrap()).unwrap();
        assert_eq!(snapshot["topic"], topics::GEMI_OPERATOR_SNAPSHOT);
        assert_eq!(snapshot["payload"]["workspace"], ws_text);

        let plan: serde_json::Value = serde_json::from_str(
            &CoreTools::plan_run(&json!({"goal": "tidy docs", "max_steps": 5}), &ws).unwrap(),
        )
        .unwrap();
        assert_eq!(plan["payload"]["goal"], "tidy docs");
        assert_eq!(plan["payload"]["max_steps"], 5);
        assert_eq!(plan["payload"]["workspace"], ws_text);
        let default_steps: serde_json::Value =
            serde_json::from_str(&CoreTools::plan_run(&json!({"goal": "tidy docs"}), &ws).unwrap())
                .unwrap();
        assert_eq!(default_steps["payload"]["max_steps"], PLAN_MAX_STEPS);
        assert!(CoreTools::plan_run(&json!({}), &ws).is_err());
        assert!(CoreTools::plan_run(&json!({"goal": "x", "max_steps": -1}), &ws).is_err());

        let pulse = CoreTools::intent_ingest(&json!({"intent": "ship B4"}), &ws).unwrap();
        assert_eq!(pulse, "ingested \"ship B4\"");
        assert!(CoreTools::intent_ingest(&json!({}), &ws).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn blackboard_show_reads_the_persisted_blackboard() {
        let ws = temp_dir("blackboard");
        let missing = CoreTools::blackboard_show(&json!({}), &ws).unwrap_err();
        assert!(missing.to_string().contains("no mission blackboard"));
        std::fs::create_dir_all(ws.join(".susi")).unwrap();
        let body = r#"{"kind":"mission_blackboard","entries":[],"agent_count":0}"#;
        std::fs::write(ws.join(".susi/last_blackboard.json"), body).unwrap();
        assert_eq!(CoreTools::blackboard_show(&json!({}), &ws).unwrap(), body);
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn placement_decision_reads_the_server_audit_log() {
        let ws = temp_dir("placement");
        std::fs::create_dir_all(ws.join(".susi")).unwrap();
        let entry = json!({
            "type": "INFERENCE_PLACEMENT",
            "ts": 3,
            "entry_hash": "abc",
            "details": json!({"target": "local", "contract": {"decision_id": "placement-1-2-3"}})
                .to_string(),
        });
        std::fs::write(ws.join(".susi/audit.log"), format!("{entry}\n")).unwrap();
        let found: serde_json::Value = serde_json::from_str(
            &CoreTools::placement_decision(&json!({"decision_id": "placement-1-2-3"}), &ws)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(found["decision"]["target"], "local");
        assert_eq!(found["audit"]["entry_hash"], "abc");
        let not_found =
            CoreTools::placement_decision(&json!({"decision_id": "placement-9-9-9"}), &ws);
        assert!(not_found
            .unwrap_err()
            .to_string()
            .contains("no INFERENCE_PLACEMENT"));
        let invalid = CoreTools::placement_decision(&json!({"decision_id": "../audit.log"}), &ws);
        assert!(invalid
            .unwrap_err()
            .to_string()
            .contains("invalid placement decision id"));
        assert!(CoreTools::placement_decision(&json!({}), &ws).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn runtime_model_tools_require_a_model() {
        let ws = Path::new(".");
        assert!(CoreTools::runtime_models_load(&json!({}), ws).is_err());
        assert!(CoreTools::runtime_models_unload(&json!({"model": ""}), ws).is_err());
    }

    #[test]
    fn runtime_model_tools_return_the_gemi_plane_documents() {
        struct Models;
        impl PlaneHandler for Models {
            fn handle(
                &self,
                topic: &str,
                payload: serde_json::Value,
            ) -> Result<serde_json::Value, String> {
                match topic {
                    topics::GEMI_MODELS_LOADED => Ok(json!({"pid": 1, "models": ["m"]})),
                    topics::GEMI_MODELS_PRELOAD if payload["model"] == "bad" => {
                        Err("unknown model".to_string())
                    }
                    other => Ok(json!({"topic": other, "model": payload["model"]})),
                }
            }
        }
        let bus = PlaneBus::global();
        for topic in [
            topics::GEMI_MODELS_LOADED,
            topics::GEMI_MODELS_PRELOAD,
            topics::GEMI_MODELS_UNLOAD,
        ] {
            bus.register(topic, Arc::new(Models));
        }
        let ws = Path::new(".");
        let listed: serde_json::Value =
            serde_json::from_str(&CoreTools::runtime_models_list(&json!({}), ws).unwrap()).unwrap();
        assert_eq!(listed, json!({"pid": 1, "models": ["m"]}));
        let loaded: serde_json::Value = serde_json::from_str(
            &CoreTools::runtime_models_load(&json!({"model": "m"}), ws).unwrap(),
        )
        .unwrap();
        assert_eq!(loaded["topic"], topics::GEMI_MODELS_PRELOAD);
        let unloaded: serde_json::Value = serde_json::from_str(
            &CoreTools::runtime_models_unload(&json!({"model": "m"}), ws).unwrap(),
        )
        .unwrap();
        assert_eq!(unloaded["topic"], topics::GEMI_MODELS_UNLOAD);
        let refused = CoreTools::runtime_models_load(&json!({"model": "bad"}), ws).unwrap_err();
        assert!(refused.to_string().contains("unknown model"));
    }

    #[test]
    fn context_graph_tools_serve_stats_and_the_cli_show_document() {
        let ws = temp_dir("graph");
        let stats: serde_json::Value =
            serde_json::from_str(&CoreTools::context_graph_stats(&json!({}), &ws).unwrap())
                .unwrap();
        assert!(stats["node_count"].is_u64());
        assert!(stats["edge_count"].is_u64());

        let graph = ContextGraph::global();
        graph.record_external_context("test", "first", &json!({}), Some(&ws), None);
        graph.record_external_context("test", "second", &json!({}), Some(&ws), None);
        let shown: serde_json::Value = serde_json::from_str(
            &CoreTools::context_graph_show(
                &json!({"limit": 1, "workspace": ws.display().to_string()}),
                Path::new("."),
            )
            .unwrap(),
        )
        .unwrap();
        // Subgraph order is not stable between calls, so compare by id.
        let ids = |doc: &serde_json::Value| -> std::collections::BTreeSet<String> {
            doc["events"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["id"].as_str().unwrap().to_string())
                .collect()
        };
        let all: std::collections::BTreeSet<String> = graph
            .workspace_events(&ws)
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(shown["workspace"], ws.display().to_string());
        assert_eq!(shown["event_count"], 1);
        assert!(ids(&shown).is_subset(&all), "{shown}");
        let default_ws: serde_json::Value =
            serde_json::from_str(&CoreTools::context_graph_show(&json!({}), &ws).unwrap()).unwrap();
        assert_eq!(
            default_ws["event_count"],
            all.len().min(CONTEXT_GRAPH_SHOW_LIMIT)
        );
        assert_eq!(ids(&default_ws), all);

        let relative = CoreTools::context_graph_show(&json!({"workspace": "repo"}), &ws);
        assert!(relative.unwrap_err().to_string().contains("absolute"));
        assert!(CoreTools::context_graph_show(&json!({"limit": "ten"}), &ws).is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }
}
