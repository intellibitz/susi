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
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::executor::OpenClaw;
use susi_harness::money::MicroUsd;
use susi_harness::policy::{Policy, UsageProfile};
use susi_harness::scheduler::{run_mission, AttemptOutcome, MissionConfig, TaskSpec};

#[derive(Deserialize)]
struct MissionFile {
    id: String,
    tasks: Vec<TaskFile>,
    candidates: Vec<String>,
    #[serde(default = "default_parallel")]
    max_parallel: usize,
    #[serde(default = "default_timeout")]
    timeout_secs: u64,
    program: String,
    gateway_url: String,
    state_dir: String,
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

fn main() {
    let path = match std::env::args().nth(1) {
        Some(path) => path,
        None => fail("usage: harness-mission <mission.json>"),
    };
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| fail(&format!("read {path}: {error}")));
    let file: MissionFile =
        serde_json::from_str(&text).unwrap_or_else(|error| fail(&format!("parse {path}: {error}")));
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
