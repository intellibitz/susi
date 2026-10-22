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
use susi_harness::agents::{AgentOutcome, AgentRunner, AgentSpec, Graph, Isolation};
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::executor::{self, ExecutorConfig, OpenClaw, RunOutcome};
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
    /// Objective handed to every role of a live graph mission.
    ///
    /// Defaults to the first task's prompt, then to the mission id.
    #[serde(default)]
    goal: Option<String>,
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

/// The program a live graph role runs: the mission's own program unless it is
/// the OpenClaw runtime this harness replaces, in which case the native worker.
fn runner_program(program: &str) -> PathBuf {
    let trimmed = program.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("openclaw") {
        PathBuf::from("native")
    } else {
        PathBuf::from(trimmed)
    }
}

/// Run each graph role as one real worker turn.
///
/// The reviewer's `accepted` means "its run succeeded": there is no verdict
/// protocol on the wire yet, so a reviewer that finishes approves the work.
/// That is the documented limitation of live graph missions.
struct SusiRunner {
    program: PathBuf,
    gateway_url: String,
    state_dir: PathBuf,
    mission: String,
    timeout: Duration,
    ledger_path: Option<PathBuf>,
}

impl SusiRunner {
    fn from_mission(file: &MissionFile) -> Self {
        let ledger = file.ledger_path.trim();
        Self {
            program: runner_program(&file.program),
            gateway_url: file.gateway_url.clone(),
            state_dir: PathBuf::from(&file.state_dir),
            mission: file.id.clone(),
            timeout: Duration::from_secs(file.timeout_secs),
            ledger_path: (!ledger.is_empty()).then(|| PathBuf::from(ledger)),
        }
    }
}

impl AgentRunner for SusiRunner {
    fn run(&mut self, spec: &AgentSpec, goal: &str) -> Result<AgentOutcome, String> {
        let workspace = self.state_dir.join(&spec.role);
        std::fs::create_dir_all(&workspace)
            .map_err(|error| format!("create workspace {}: {error}", workspace.display()))?;
        let config = ExecutorConfig {
            program: self.program.clone(),
            gateway_url: self.gateway_url.clone(),
            model: spec.model.clone(),
            workspace,
            state_dir: self.state_dir.clone(),
            mission: self.mission.clone(),
            task: spec.role.clone(),
            attempt: 1,
            timeout: self.timeout,
            stall_after: None,
            ledger_path: self.ledger_path.clone(),
            prompt: goal.to_string(),
        };
        let outcome =
            executor::run(&config).map_err(|error| format!("run `{}`: {error}", spec.role))?;
        let zero = MicroUsd::ZERO;
        Ok(match outcome {
            RunOutcome::Succeeded { final_text, .. } => AgentOutcome {
                summary: final_text,
                accepted: true,
                cost: zero,
            },
            RunOutcome::Failed { message } => AgentOutcome {
                summary: message,
                accepted: false,
                cost: zero,
            },
            RunOutcome::TimedOut => AgentOutcome {
                summary: format!("timed out after {}s", self.timeout.as_secs()),
                accepted: false,
                cost: zero,
            },
            RunOutcome::Stalled { message } => AgentOutcome {
                summary: message,
                accepted: false,
                cost: zero,
            },
        })
    }
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
        let goal = file
            .goal
            .clone()
            .or_else(|| file.tasks.first().map(|task| task.prompt.clone()))
            .unwrap_or_else(|| file.id.clone());
        let mut runner = SusiRunner::from_mission(&file);
        let report = match graph.run(&mut runner, &goal) {
            Ok(report) => report,
            Err(error) => fail(&error),
        };
        let bus_path = PathBuf::from(&file.state_dir).join("graph.jsonl");
        if let Err(error) = report.bus.persist(&bus_path) {
            fail(&format!("persist {}: {error}", bus_path.display()));
        }
        let ok = report.outcomes.iter().all(|(_, outcome)| outcome.accepted);
        let steps = report
            .outcomes
            .iter()
            .map(|(role, outcome)| {
                let model = graph
                    .agents()
                    .iter()
                    .find(|agent| &agent.role == role)
                    .map(|agent| agent.model.as_str())
                    .unwrap_or_default();
                json!({
                    "role": role,
                    "model": model,
                    "accepted": outcome.accepted,
                    "summary": outcome.summary,
                })
            })
            .collect::<Vec<_>>();
        let summary = json!({
            "mission": file.id,
            "ok": ok,
            "total_cost_micro_usd": report.total.micros(),
            "steps": steps,
            "bus": bus_path.display().to_string(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
        if !ok {
            std::process::exit(1);
        }
        return;
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

    fn mission(agents: &str) -> MissionFile {
        let text =
            format!(r#"{{"id":"live-1","state_dir":"state","timeout_secs":30,"agents":{agents}}}"#);
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn a_live_agents_mission_parses_and_validates_without_spawning() {
        let file = mission(r#"{"planner":"planner","coders":["coder"],"reviewer":"reasoner"}"#);
        assert!(!file.dry_run);
        assert_eq!(runner_program(""), PathBuf::from("native"));
        assert_eq!(runner_program("openclaw"), PathBuf::from("native"));
        assert_eq!(
            runner_program("C:/tools/worker.exe"),
            PathBuf::from("C:/tools/worker.exe")
        );
        let steps = graph_from(file.agents.as_ref().unwrap()).steps().unwrap();
        let roles: Vec<&str> = steps.iter().map(|step| step.role.as_str()).collect();
        assert_eq!(roles, vec!["planner", "coder-1", "reviewer"]);
        assert_eq!(steps[1].model, "coder");
    }

    #[test]
    fn a_live_agents_mission_refuses_an_unknown_model_before_spawning() {
        let file =
            mission(r#"{"planner":"planner","coders":["mystery-model"],"reviewer":"planner"}"#);
        let error = graph_from(file.agents.as_ref().unwrap())
            .steps()
            .unwrap_err();
        assert!(error.contains("unknown model"), "{error}");
        assert!(error.contains("mystery-model"), "{error}");
    }
}
