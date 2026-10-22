#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

//! Run a mission from a JSON file.
//!
//! ```text
//! harness-mission mission.json
//! ```
//!
//! The file names the tasks, their workspaces, the candidate models (cheapest
//! first), the parallelism, and where the state, event feed and ledger live.

use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use susi_harness::agents::{AgentSpec, Graph, Isolation};
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::executor::OpenClaw;
use susi_harness::money::MicroUsd;
use susi_harness::policy::{Policy, UsageProfile};
use susi_harness::scheduler::{run_mission, AttemptOutcome, MissionConfig, TaskSpec};

#[derive(Deserialize)]
struct MissionFile {
    id: String,
    #[serde(default)]
    tasks: Vec<TaskFile>,
    #[serde(default)]
    candidates: Vec<String>,
    #[serde(default = "default_parallel")]
    max_parallel: usize,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    #[serde(default)]
    program: String,
    #[serde(default)]
    gateway_url: String,
    #[serde(default)]
    state_dir: String,
    #[serde(default)]
    ledger_path: String,
    /// Mission cap in micro-USD; requires `catalogue`.
    #[serde(default)]
    budget_micro_usd: Option<u64>,
    /// Price catalogue used to rank candidates and bound spend.
    #[serde(default)]
    catalogue: Option<String>,
    /// Hard cap on attempts per task.
    #[serde(default)]
    max_attempts: Option<usize>,
    /// Agent graph to validate and, with `dry_run`, print before spending.
    #[serde(default)]
    agents: Option<AgentsFile>,
    /// Validate the graph and print its steps; call no model.
    #[serde(default)]
    dry_run: bool,
}

/// The agent-graph block of a mission file.
#[derive(Deserialize)]
struct AgentsFile {
    planner: String,
    coders: Vec<String>,
    reviewer: String,
    #[serde(default)]
    tools: Vec<String>,
}

#[derive(Deserialize)]
struct TaskFile {
    id: String,
    prompt: String,
    workspace: String,
}

fn default_parallel() -> usize {
    2
}

fn default_timeout() -> u64 {
    600
}

/// Build the planner/coders/reviewer graph from the mission file.
fn graph_from(agents: &AgentsFile) -> Graph {
    let defaults = [
        "view_file",
        "write_to_file",
        "replace_file_content",
        "run_command",
        "list_dir",
    ];
    let tools: Vec<&str> = if agents.tools.is_empty() {
        defaults.to_vec()
    } else {
        agents.tools.iter().map(String::as_str).collect()
    };
    let mut specs = vec![AgentSpec::new("planner", &agents.planner, &tools)];
    for (index, model) in agents.coders.iter().enumerate() {
        specs.push(AgentSpec::new(
            &format!("coder-{}", index + 1),
            model,
            &tools,
        ));
    }
    specs.push(AgentSpec::new("reviewer", &agents.reviewer, &tools).shared());
    Graph::new(specs)
}

fn main() {
    let path = match std::env::args().nth(1) {
        Some(path) => path,
        None => fail("usage: harness-mission <mission.json>"),
    };
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| fail(&format!("read {path}: {error}")));
    let file: MissionFile =
        serde_json::from_str(&text).unwrap_or_else(|error| fail(&format!("parse {path}: {error}")));
    if let Some(agents) = &file.agents {
        let graph = graph_from(agents);
        let steps = graph.steps().unwrap_or_else(|error| fail(&error));
        if file.dry_run {
            let steps = steps
                .iter()
                .map(|step| {
                    json!({
                        "role": step.role,
                        "model": step.model,
                        "isolation": match step.isolation {
                            Isolation::Worktree => "worktree",
                            Isolation::Shared => "shared",
                        },
                    })
                })
                .collect::<Vec<_>>();
            let summary = json!({"mission": file.id, "dry_run": true, "agents": steps});
            println!(
                "{}",
                serde_json::to_string_pretty(&summary).unwrap_or_default()
            );
            return;
        }
        fail("live agent-graph missions are not enabled yet; set \"dry_run\": true");
    }
    let catalogue = file
        .catalogue
        .as_deref()
        .map(|catalogue| PriceCatalogue::load(std::path::Path::new(catalogue)))
        .transpose()
        .unwrap_or_else(|error| fail(&error.to_string()));
    let policy = Policy {
        max_attempts: file.max_attempts.unwrap_or(4).max(1),
        ..Policy::default()
    };
    let config = MissionConfig {
        id: file.id,
        tasks: file
            .tasks
            .into_iter()
            .map(|task| TaskSpec {
                id: task.id,
                prompt: task.prompt,
                workspace: PathBuf::from(task.workspace),
            })
            .collect(),
        candidates: file.candidates,
        max_parallel: file.max_parallel,
        timeout: Duration::from_secs(file.timeout_secs),
        program: PathBuf::from(file.program),
        gateway_url: file.gateway_url,
        state_dir: PathBuf::from(file.state_dir),
        ledger_path: PathBuf::from(file.ledger_path),
        budget: file
            .budget_micro_usd
            .map(|micros| Budget::new(MicroUsd::from_micros(micros))),
        policy,
        usage: UsageProfile::agent(),
        catalogue,
    };
    let report = match run_mission(&config, &OpenClaw) {
        Ok(report) => report,
        Err(error) => fail(&error.to_string()),
    };
    let tasks: Vec<_> = report
        .results
        .iter()
        .map(|result| {
            json!({
                "task": result.task,
                "succeeded": result.succeeded,
                "final": result.final_text(),
                "attempts": result.attempts.iter().map(|attempt| json!({
                    "model": attempt.model,
                    "outcome": describe(&attempt.outcome),
                    "elapsed_ms": attempt.elapsed_ms
                })).collect::<Vec<_>>()
            })
        })
        .collect();
    let summary = json!({
        "mission": report.mission,
        "succeeded": report.succeeded,
        "failed": report.failed,
        "stuck": report.stuck,
        "budget_exhausted": report.budget_exhausted,
        "budget_limit_micro_usd": report.budget_limit.map(MicroUsd::micros),
        "total_cost_micro_usd": report.total_cost.micros(),
        "events": report.events_path.display().to_string(),
        "tasks": tasks
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if report.failed > 0 {
        std::process::exit(1);
    }
}

fn describe(outcome: &AttemptOutcome) -> &'static str {
    match outcome {
        AttemptOutcome::Succeeded { .. } => "succeeded",
        AttemptOutcome::Failed { .. } => "failed",
        AttemptOutcome::TimedOut => "timed_out",
        AttemptOutcome::Error { .. } => "error",
        AttemptOutcome::Skipped { .. } => "skipped",
    }
}

fn fail(message: &str) -> ! {
    eprintln!("harness-mission: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agents() -> AgentsFile {
        AgentsFile {
            planner: "planner".into(),
            coders: vec!["coder".into(), "coder".into()],
            reviewer: "planner".into(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn a_dry_run_graph_lists_planner_coders_and_reviewer() {
        let steps = graph_from(&agents()).steps().unwrap();
        let roles: Vec<&str> = steps.iter().map(|step| step.role.as_str()).collect();
        assert_eq!(roles, vec!["planner", "coder-1", "coder-2", "reviewer"]);
        assert_eq!(steps[1].model, "coder");
        assert_eq!(steps[3].isolation, Isolation::Shared);
    }

    #[test]
    fn an_unknown_tool_is_refused_before_any_model_runs() {
        let mut file = agents();
        file.tools = vec!["format_disk".into()];
        assert!(graph_from(&file).steps().is_err());
    }
}
