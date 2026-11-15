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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[path = "../tests/harness_tools.rs"]
mod tests;
