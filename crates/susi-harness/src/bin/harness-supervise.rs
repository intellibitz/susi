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

//! Run a mission plan against susi, resuming where the last run stopped.

use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use susi_harness::adapter::SusiTarget;
use susi_harness::budget::Budget;
use susi_harness::catalog::PriceCatalogue;
use susi_harness::executor::NativeWorker;
use susi_harness::money::MicroUsd;
use susi_harness::policy::Policy;
use susi_harness::supervisor::{Plan, Supervisor, SusiExecutor};
use susi_harness::validate::{validate_plan, RepoFacts};

struct Options {
    plan: PathBuf,
    state: PathBuf,
    repo: PathBuf,
    susi_program: PathBuf,
    gateway_url: String,
    model: String,
    alternates: Vec<String>,
    catalogue: Option<PriceCatalogue>,
    budget: Option<Budget>,
    ledger: PathBuf,
    controls: PathBuf,
    rsi_log: Option<PathBuf>,
    max_attempts: usize,
    timeout: Duration,
    prefix: String,
    adapter_state: PathBuf,
    parallel: usize,
}

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => fail(&message),
    };
    let plan = match Plan::load(&options.plan) {
        Ok(plan) => plan,
        Err(error) => fail(&error.to_string()),
    };
    let facts = match RepoFacts::load(&options.repo) {
        Ok(facts) => facts,
        Err(error) => fail(&error.to_string()),
    };
    if let Err(error) = validate_plan(&plan, &facts) {
        fail(&format!("plan rejected: {error}"));
    }
    let target = SusiTarget::new(options.repo, options.susi_program);
    let native = NativeWorker;
    let executor = SusiExecutor {
        target: &target,
        runner: &native,
        worktree_prefix: options.prefix,
        gateway_url: options.gateway_url,
        model: options.model,
        alternates: options.alternates,
        catalogue: options.catalogue,
        budget: options.budget,
        ledger_path: options.ledger,
        mission: plan.mission.clone(),
        policy: Policy {
            max_attempts: options.max_attempts,
            ..Policy::default()
        },
        timeout: options.timeout,
        state_dir: options.adapter_state,
        lifecycle_path: options.rsi_log,
        guard: std::sync::OnceLock::new(),
        exhausted: AtomicBool::new(false),
    };
    let supervisor = Supervisor::new(options.state, &executor)
        .with_parallelism(options.parallel)
        .with_controls(options.controls);
    let report = match supervisor.run(&plan) {
        Ok(report) => report,
        Err(error) => fail(&error.to_string()),
    };
    let summary = json!({
        "mission": report.mission,
        "done": report.done,
        "failed": report.failed,
        "skipped": report.skipped,
        "unstarted": report.unstarted,
        "budget_exhausted": report.budget_exhausted,
        "paused": report.paused,
        "aborted": report.aborted
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if !report.failed.is_empty() {
        std::process::exit(1);
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut plan = PathBuf::new();
    let mut state = PathBuf::new();
    let mut repo = PathBuf::new();
    let mut susi_program = PathBuf::new();
    let mut gateway_url = String::new();
    let mut model = String::new();
    let mut alternates = Vec::new();
    let mut catalogue_path: Option<PathBuf> = None;
    let mut budget: Option<Budget> = None;
    let mut ledger = PathBuf::from("out/ledger.jsonl");
    let mut controls = PathBuf::from("out/control.jsonl");
    let mut rsi_log: Option<PathBuf> = None;
    let mut max_attempts: usize = 4;
    let mut timeout: u64 = 900;
    let mut prefix = String::from("harness");
    let mut adapter_state = PathBuf::new();
    let mut parallel: usize = 1;

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--plan" => plan = PathBuf::from(take(&mut args, &argument)?),
            "--state" => state = PathBuf::from(take(&mut args, &argument)?),
            "--repo" => repo = PathBuf::from(take(&mut args, &argument)?),
            "--susi" => susi_program = PathBuf::from(take(&mut args, &argument)?),
            "--gateway" => gateway_url = take(&mut args, &argument)?,
            "--model" => model = take(&mut args, &argument)?,
            "--alternate" => alternates.push(take(&mut args, &argument)?),
            "--catalogue" => catalogue_path = Some(PathBuf::from(take(&mut args, &argument)?)),
            "--budget-usd" => {
                let amount = take(&mut args, &argument)?;
                let micros = MicroUsd::parse_usd(&amount)
                    .ok_or_else(|| format!("--budget-usd {amount} is not a dollar amount"))?;
                budget = Some(Budget::new(micros));
            }
            "--ledger" => ledger = PathBuf::from(take(&mut args, &argument)?),
            "--controls" => controls = PathBuf::from(take(&mut args, &argument)?),
            "--rsi-log" => rsi_log = Some(PathBuf::from(take(&mut args, &argument)?)),
            "--attempts" => {
                max_attempts = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "attempts must be a number".to_string())?;
            }
            "--timeout" => {
                timeout = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "timeout must be a number of seconds".to_string())?;
            }
            "--prefix" => prefix = take(&mut args, &argument)?,
            "--adapter-state" => adapter_state = PathBuf::from(take(&mut args, &argument)?),
            "--parallel" => {
                parallel = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "parallel must be a number".to_string())?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }

    if plan.as_os_str().is_empty() {
        return Err("--plan is required".into());
    }
    if repo.as_os_str().is_empty() || susi_program.as_os_str().is_empty() {
        return Err("--repo and --susi are required".into());
    }
    if gateway_url.trim().is_empty() || model.trim().is_empty() {
        return Err("--gateway and --model are required".into());
    }
    if adapter_state.as_os_str().is_empty() {
        return Err("--adapter-state is required".into());
    }
    if state.as_os_str().is_empty() {
        state = plan.with_file_name("supervisor-state.json");
    }
    if budget.is_some() && catalogue_path.is_none() {
        return Err("--budget-usd needs --catalogue: an unpriced attempt cannot be bounded".into());
    }
    let catalogue = match catalogue_path {
        Some(path) => Some(PriceCatalogue::load(&path).map_err(|error| error.to_string())?),
        None => None,
    };

    Ok(Options {
        plan,
        state,
        repo,
        susi_program,
        gateway_url,
        model,
        alternates,
        catalogue,
        budget,
        ledger,
        controls,
        rsi_log,
        max_attempts: max_attempts.max(1),
        timeout: Duration::from_secs(timeout),
        prefix,
        adapter_state,
        parallel,
    })
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn fail(message: &str) -> ! {
    eprintln!("harness-supervise: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parses_a_complete_run_and_defaults_the_state() {
        let options = parse(args(&[
            "--plan",
            "plan.json",
            "--repo",
            "repo",
            "--susi",
            "susi.exe",
            "--gateway",
            "http://127.0.0.1:9188",
            "--model",
            "deepseek-flash",
            "--adapter-state",
            "state",
        ]))
        .unwrap();
        assert_eq!(options.state, PathBuf::from("supervisor-state.json"));
        assert_eq!(options.timeout, Duration::from_secs(900));
    }

    #[test]
    fn refuses_missing_required_flags() {
        assert!(parse(args(&["--plan", "p"])).is_err());
        assert!(parse(args(&["--nope"])).is_err());
    }
}
