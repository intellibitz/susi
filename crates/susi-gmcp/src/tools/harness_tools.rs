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

use serde_json::{json, Value};
use std::path::Path;

use crate::susi_core::plane_bus::daemon;
use crate::susi_error::{EaiError, EaiResult};

use super::args;
use super::forward::{forward, merged, policy_paths, policy_workspace, Kind};
use super::CoreTools;

const STR: Kind = Kind::Str;
const U64: Kind = Kind::U64;

/// `repo`, resolved through the MAC policy; `required` refuses its absence.
fn repo(arg: &Value, server: &Path, required: bool, write: bool) -> EaiResult<Value> {
    if arg.get("repo").is_none_or(Value::is_null) {
        if required {
            return Err(EaiError::config("repo is required"));
        }
        return Ok(json!({}));
    }
    let path = policy_workspace(arg, "repo", server, write)?;
    Ok(json!({ "repo": path.display().to_string() }))
}

fn harness(action: &str, arg: &Value, keys: &[(&str, Kind)], extra: Value) -> EaiResult<String> {
    forward(daemon::harness, action, arg, keys, extra)
}

impl CoreTools {
    crate::tool_def!(harness_status_tool_attr {
        name: "harness_status",
        description: "The harness API snapshot (spend, tasks, workers, warnings) for a ledger, with a run's state and plan when given, as `susi harness status`. Args: ledger, optional events, state, plan (absolute paths)",
        input_schema: args::typed::<args::HarnessStatus>(),
    });
    pub fn harness_status(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        harness(
            "status",
            arg,
            &[
                ("ledger", STR),
                ("events", STR),
                ("state", STR),
                ("plan", STR),
            ],
            policy_paths(
                arg,
                &[
                    ("ledger", false),
                    ("events", false),
                    ("state", false),
                    ("plan", false),
                ],
            )?,
        )
    }

    crate::tool_def!(harness_budget_tool_attr {
        name: "harness_budget",
        description: "The one-line harness budget report and any top-up alerts, as `susi harness budget`. Args: optional ledger, repo (absolute)",
        input_schema: args::typed::<args::HarnessBudget>(),
    });
    pub fn harness_budget(arg: &Value, workspace: &Path) -> EaiResult<String> {
        harness(
            "budget",
            arg,
            &[("ledger", STR)],
            merged(
                repo(arg, workspace, false, false)?,
                policy_paths(arg, &[("ledger", false)])?,
            ),
        )
    }

    crate::tool_def!(harness_models_tool_attr {
        name: "harness_models",
        description: "The harness provider registry (registry, no network), its live drift check (verify), the models keyed providers add (discover) or the keyed models by cost (available), as `susi harness models`. Args: optional mode, provider, repo",
        input_schema: args::typed::<args::HarnessModels>(),
    });
    pub fn harness_models(arg: &Value, workspace: &Path) -> EaiResult<String> {
        harness(
            "models",
            arg,
            &[("mode", STR), ("provider", STR)],
            repo(arg, workspace, false, false)?,
        )
    }

    crate::tool_def!(harness_plan_tool_attr {
        name: "harness_plan",
        description: "Plan a batch of dependency-ready queue tasks and write the plan, as `susi harness plan`. Args: repo, out (absolute), optional mission, limit, workers, roadmap, gateway",
        input_schema: args::typed::<args::HarnessPlan>(),
    });
    pub fn harness_plan(arg: &Value, workspace: &Path) -> EaiResult<String> {
        harness(
            "plan",
            arg,
            &[
                ("out", STR),
                ("mission", STR),
                ("limit", U64),
                ("workers", STR),
                ("roadmap", STR),
                ("gateway", STR),
            ],
            merged(
                repo(arg, workspace, true, true)?,
                policy_paths(arg, &[("out", true)])?,
            ),
        )
    }

    crate::tool_def!(harness_author_tool_attr {
        name: "harness_author",
        description: "Author a plan for a roadmap vector through the metered gateway, validate it and write it, as `susi harness author`. Args: repo, gateway, vector, out, optional model, roadmap, max_tasks, workers",
        input_schema: args::typed::<args::HarnessAuthor>(),
    });
    pub fn harness_author(arg: &Value, workspace: &Path) -> EaiResult<String> {
        harness(
            "author",
            arg,
            &[
                ("gateway", STR),
                ("vector", STR),
                ("out", STR),
                ("model", STR),
                ("roadmap", STR),
                ("max_tasks", U64),
                ("workers", STR),
            ],
            merged(
                repo(arg, workspace, true, true)?,
                policy_paths(arg, &[("out", true), ("roadmap", false)])?,
            ),
        )
    }

    crate::tool_def!(harness_run_tool_attr {
        name: "harness_run",
        description: "Start running a plan under the budget gate on a daemon thread (prepare in parallel, land in order); returns the mission and its state file, as `susi harness run`. Needs process.exec: the run drives this daemon's own susi binary (the CLI's --susi stays local). Args: repo, plan, gateway, and the CLI's other optional flags",
        input_schema: args::typed::<args::HarnessRun>(),
    });
    pub fn harness_run(arg: &Value, workspace: &Path) -> EaiResult<String> {
        if arg.get("susi").is_some_and(|v| !v.is_null()) {
            return Err(EaiError::config(
                "susi cannot be chosen over the socket; a socket run drives the daemon's own binary",
            ));
        }
        harness(
            "run",
            arg,
            &[
                ("plan", STR),
                ("gateway", STR),
                ("model", STR),
                ("timeout", U64),
                ("parallel", U64),
                ("prefix", STR),
                ("workers", STR),
                ("budget_usd", STR),
                ("daily_usd", STR),
                ("attempts", U64),
                ("state", STR),
                ("catalogue", STR),
                ("ledger", STR),
                ("controls", STR),
                ("rsi_log", STR),
                ("adapter_state", STR),
            ],
            merged(
                repo(arg, workspace, true, true)?,
                policy_paths(
                    arg,
                    &[
                        ("plan", false),
                        ("catalogue", false),
                        ("state", true),
                        ("ledger", true),
                        ("controls", true),
                        ("rsi_log", true),
                        ("adapter_state", true),
                    ],
                )?,
            ),
        )
    }

    crate::tool_def!(harness_runs_tool_attr {
        name: "harness_runs",
        description: "The harness missions this daemon started: running or finished, with notes, the report and the budget line",
        input_schema: args::no_args(),
    });
    pub fn harness_runs(arg: &Value, _workspace: &Path) -> EaiResult<String> {
        harness("runs", arg, &[], json!({}))
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
    fn harness_tools_require_and_confine_the_repository() {
        PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let ledger = ws.join("susi-harness-tool-l.jsonl");
        let status = sent(CoreTools::harness_status(
            &json!({"ledger": ledger.display().to_string()}),
            &ws,
        ));
        assert_eq!(status["action"], "status");
        assert!(
            Path::new(status["ledger"].as_str().unwrap()).ends_with("susi-harness-tool-l.jsonl")
        );
        // Every caller-named file path is absolute and policy-confined.
        assert!(
            CoreTools::harness_status(&json!({"ledger": "rel.jsonl"}), &ws)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        assert_eq!(
            sent(CoreTools::harness_budget(&json!({}), &ws)),
            json!({"action": "budget"})
        );
        assert!(CoreTools::harness_plan(&json!({"out": "/p.json"}), &ws)
            .unwrap_err()
            .to_string()
            .contains("repo is required"));
        assert!(
            CoreTools::harness_run(&json!({"repo": "relative", "plan": "/p"}), &ws)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        let repo_dir =
            std::env::temp_dir().join(format!("susi-harness-tool-{}", std::process::id()));
        std::fs::create_dir_all(&repo_dir).unwrap();
        let repo_text = repo_dir.display().to_string();
        let out = repo_dir.join("plan.json").display().to_string();
        let planned = sent(CoreTools::harness_plan(
            &json!({"repo": repo_text, "out": out, "limit": 2}),
            &ws,
        ));
        assert_eq!(planned["limit"], 2);
        assert!(Path::new(planned["out"].as_str().unwrap()).ends_with("plan.json"));
        assert!(
            CoreTools::harness_plan(&json!({"repo": repo_text, "out": "p.json"}), &ws)
                .unwrap_err()
                .to_string()
                .contains("absolute")
        );
        // The binary a run drives is never the caller's choice.
        let chosen = CoreTools::harness_run(
            &json!({"repo": repo_text, "plan": out, "gateway": "g", "susi": "/bin/sh"}),
            &ws,
        )
        .unwrap_err();
        assert!(
            chosen.to_string().contains("susi cannot be chosen"),
            "{chosen}"
        );
        let run = sent(CoreTools::harness_run(
            &json!({"repo": repo_text, "plan": out, "gateway": "g"}),
            &ws,
        ));
        assert!(run.get("susi").is_none());
        assert!(Path::new(run["plan"].as_str().unwrap()).ends_with("plan.json"));
        assert_eq!(
            Path::new(planned["repo"].as_str().unwrap())
                .canonicalize()
                .unwrap(),
            repo_dir.canonicalize().unwrap()
        );
        assert!(CoreTools::harness_models(&json!({"mode": 1}), &ws).is_err());
        assert_eq!(
            sent(CoreTools::harness_runs(&json!({}), &ws)),
            json!({"action": "runs"})
        );
        let _ = std::fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn harness_models_forwards_mode_provider_and_optional_repo() {
        PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let out = sent(CoreTools::harness_models(
            &json!({"mode": "available", "provider": "openrouter"}),
            &ws,
        ));
        assert_eq!(out["action"], "models");
        assert_eq!(out["mode"], "available");
        assert_eq!(out["provider"], "openrouter");
    }

    #[test]
    fn harness_author_requires_repo_and_confines_out_and_roadmap_paths() {
        PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        assert!(CoreTools::harness_author(&json!({"vector": "VC-1"}), &ws)
            .unwrap_err()
            .to_string()
            .contains("repo is required"));

        let repo_dir =
            std::env::temp_dir().join(format!("susi-harness-author-{}", std::process::id()));
        std::fs::create_dir_all(&repo_dir).unwrap();
        let out_file = repo_dir.join("authored.json");
        let authored = sent(CoreTools::harness_author(
            &json!({
                "repo": repo_dir.display().to_string(),
                "gateway": "http://127.0.0.1:9000",
                "vector": "VC-201-001",
                "out": out_file.display().to_string(),
                "max_tasks": 4
            }),
            &ws,
        ));
        assert_eq!(authored["action"], "author");
        assert_eq!(authored["vector"], "VC-201-001");
        assert_eq!(authored["max_tasks"], 4);
        let _ = std::fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn harness_budget_confines_optional_ledger_and_repo_arguments() {
        PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        let ledger = ws.join("budget-ledger.jsonl");
        let budget = sent(CoreTools::harness_budget(
            &json!({"ledger": ledger.display().to_string()}),
            &ws,
        ));
        assert_eq!(budget["action"], "budget");
        assert!(Path::new(budget["ledger"].as_str().unwrap()).ends_with("budget-ledger.jsonl"));
        assert!(CoreTools::harness_budget(&json!({"ledger": "relative.jsonl"}), &ws).is_err());
    }

    #[test]
    fn harness_status_and_run_reject_relative_events_or_plan_paths() {
        PlaneBus::global().register(topics::DAEMON_HARNESS, Arc::new(Echo));
        let ws = std::env::temp_dir();
        assert!(CoreTools::harness_status(&json!({"events": "rel-events.jsonl"}), &ws).is_err());
        assert!(CoreTools::harness_run(
            &json!({
                "repo": ws.display().to_string(),
                "plan": "rel-plan.json",
                "gateway": "http://127.0.0.1:9000"
            }),
            &ws
        )
        .is_err());
    }
}
