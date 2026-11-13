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
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;
use susi_harness::agents::{AgentOutcome, AgentRunner, AgentSpec, Graph, Isolation};
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::executor::{self, ExecutorConfig, NativeWorker, RunOutcome};
use susi_harness::ledger::spent_for;
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
    specs.push(AgentSpec::new("reviewer", &agents.reviewer, &tools));
    Graph::new(specs)
}

/// Run each graph role as one real worker turn in its own isolated Git worktree
/// and with mandatory Susi Native Board collaboration.
struct SusiRunner {
    gateway_url: String,
    repo: PathBuf,
    state_dir: PathBuf,
    mission: String,
    timeout: Duration,
    ledger_path: Option<PathBuf>,
    /// Cost already read back for each role, so a role the graph retries is
    /// charged only the rows its latest turn added.
    costed: BTreeMap<String, MicroUsd>,
}

impl SusiRunner {
    fn from_mission(file: &MissionFile) -> Self {
        let ledger = file.ledger_path.trim();
        let repo = file
            .tasks
            .first()
            .map(|task| PathBuf::from(&task.workspace))
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from(&file.state_dir))
            });
        Self {
            gateway_url: file.gateway_url.clone(),
            repo,
            state_dir: PathBuf::from(&file.state_dir),
            mission: file.id.clone(),
            timeout: Duration::from_secs(file.timeout_secs),
            ledger_path: (!ledger.is_empty()).then(|| PathBuf::from(ledger)),
            costed: BTreeMap::new(),
        }
    }

    /// The cost the ledger recorded for this role since the previous readback.
    ///
    /// The worker tags every gateway row `mission=<file id>, task=<role>`, so
    /// the sum is exactly what this role spent in this mission. The cumulative
    /// figure is remembered per role: a role the graph retries reports only the
    /// retry's rows, which keeps [`Graph`]'s total equal to the sum of the same
    /// mission/role rows the cockpit groups in `role_spend`. A blank ledger
    /// path, an absent ledger or an unreadable row costs zero; readback is
    /// strictly read-only and never appends.
    fn readback(&mut self, role: &str) -> MicroUsd {
        let Some(path) = &self.ledger_path else {
            return MicroUsd::ZERO;
        };
        let cumulative = spent_for(path, &self.mission, role);
        let previous = self
            .costed
            .insert(role.to_string(), cumulative)
            .unwrap_or(MicroUsd::ZERO);
        cumulative.checked_sub(previous).unwrap_or(MicroUsd::ZERO)
    }
}

impl AgentRunner for SusiRunner {
    fn run(&mut self, spec: &AgentSpec, goal: &str) -> Result<AgentOutcome, String> {
        let role_or_task = format!("{}:{}", self.mission, spec.role);
        let mut session = susi_core::native_board::CollaborativeAgentSession::begin(
            &susi_core::native_board::SessionSpec {
                workspace: &self.repo,
                explicit_agent: None,
                role_or_task: &role_or_task,
                title: &format!("{} role {}", self.mission, spec.role),
                scope: Vec::new(),
                require_fresh: true,
            },
        )?;
        let workspace = session.worktree.path().to_path_buf();
        let config = ExecutorConfig {
            gateway_url: self.gateway_url.clone(),
            model: spec.model.clone(),
            workspace,
            state_dir: self.state_dir.clone(),
            mission: self.mission.clone(),
            task: spec.role.clone(),
            attempt: 1,
            timeout: self.timeout,
            prompt: goal.to_string(),
        };
        let run_res = executor::run(&config);
        let cost = self.readback(&spec.role);
        let agent_outcome = match run_res {
            Ok(RunOutcome::Succeeded { final_text, .. }) => Ok(AgentOutcome {
                summary: final_text,
                accepted: true,
                cost,
            }),
            Ok(RunOutcome::Failed { message }) => Ok(AgentOutcome {
                summary: message,
                accepted: false,
                cost,
            }),
            Ok(RunOutcome::TimedOut) => Ok(AgentOutcome {
                summary: format!("timed out after {}s", self.timeout.as_secs()),
                accepted: false,
                cost,
            }),
            Err(error) => Err(format!("run `{}`: {error}", spec.role)),
        };
        match &agent_outcome {
            Ok(out) => {
                session.finish(out.accepted, &out.summary)?;
            }
            Err(err) => {
                session.finish(false, err)?;
            }
        }
        agent_outcome
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
                    "cost_micro_usd": outcome.cost.micros(),
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
    let report = match run_mission(&config, &NativeWorker) {
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
    use susi_harness::ledger::{append_entry, Dispatch, LedgerEntry, Outcome};
    use susi_harness::pricing::{ModelPrice, TokenUsage};

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
        assert_eq!(steps[3].isolation, Isolation::Worktree);
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

    /// A row whose cost is exactly `micros`: one million input tokens at that
    /// per-million miss rate and no output.
    fn charged(mission: &str, task: &str, micros: u64) -> LedgerEntry {
        LedgerEntry::settle(Dispatch {
            mission: mission.into(),
            task: task.into(),
            attempt: 1,
            model: "deepseek-flash".into(),
            provider: "deepseek".into(),
            usage: TokenUsage::new(1_000_000, 0, 0),
            price: ModelPrice {
                input_miss: MicroUsd::from_micros(micros),
                input_hit: MicroUsd::ZERO,
                output: MicroUsd::ZERO,
            },
            started_unix_ms: 0,
            ended_unix_ms: 1,
            outcome: Outcome::Completed,
        })
    }

    #[test]
    fn readback_charges_each_run_only_the_rows_it_added() {
        let dir = susi_paths::tempfile::tempdir().unwrap();
        let ledger = dir.path().join("ledger.jsonl");
        let mut runner = SusiRunner {
            gateway_url: String::new(),
            repo: dir.path().to_path_buf(),
            state_dir: dir.path().to_path_buf(),
            mission: "live-1".into(),
            timeout: Duration::from_secs(1),
            ledger_path: Some(ledger.clone()),
            costed: BTreeMap::new(),
        };

        append_entry(&ledger, &charged("live-1", "planner", 100)).unwrap();
        append_entry(&ledger, &charged("other-1", "planner", 900)).unwrap();
        assert_eq!(runner.readback("planner"), MicroUsd::from_micros(100));
        // The rows already charged are not charged to the next run again.
        assert_eq!(runner.readback("planner"), MicroUsd::ZERO);
        // Another role in this mission made no call yet.
        assert_eq!(runner.readback("coder-1"), MicroUsd::ZERO);
        append_entry(&ledger, &charged("live-1", "planner", 50)).unwrap();
        append_entry(&ledger, &charged("live-1", "coder-1", 7)).unwrap();
        assert_eq!(runner.readback("planner"), MicroUsd::from_micros(50));
        assert_eq!(runner.readback("coder-1"), MicroUsd::from_micros(7));
    }

    #[test]
    fn a_blank_ledger_path_costs_nothing() {
        let mut runner = SusiRunner::from_mission(&mission(
            r#"{"planner":"planner","coders":["coder"],"reviewer":"reasoner"}"#,
        ));
        assert!(runner.ledger_path.is_none());
        assert_eq!(runner.readback("planner"), MicroUsd::ZERO);
    }
}
