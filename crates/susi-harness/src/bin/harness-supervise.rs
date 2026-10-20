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
use std::time::Duration;
use susi_harness::adapter::SusiTarget;
use susi_harness::executor::OpenClaw;
use susi_harness::supervisor::{Plan, Supervisor, SusiExecutor};
use susi_harness::validate::{validate_plan, RepoFacts};

struct Options {
    plan: PathBuf,
    state: PathBuf,
    repo: PathBuf,
    susi_program: PathBuf,
    program: PathBuf,
    gateway_url: String,
    model: String,
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
    let openclaw = OpenClaw;
    let executor = SusiExecutor {
        target: &target,
        runner: &openclaw,
        worktree_prefix: options.prefix,
        program: options.program,
        gateway_url: options.gateway_url,
        model: options.model,
        timeout: options.timeout,
        state_dir: options.adapter_state,
    };
    let supervisor = Supervisor::new(options.state, &executor).with_parallelism(options.parallel);
    let report = match supervisor.run(&plan) {
        Ok(report) => report,
        Err(error) => fail(&error.to_string()),
    };
    let summary = json!({
        "mission": report.mission,
        "done": report.done,
        "failed": report.failed,
        "skipped": report.skipped
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
    let mut program = PathBuf::new();
    let mut gateway_url = String::new();
    let mut model = String::new();
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
            "--program" => program = PathBuf::from(take(&mut args, &argument)?),
            "--gateway" => gateway_url = take(&mut args, &argument)?,
            "--model" => model = take(&mut args, &argument)?,
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
    if program.as_os_str().is_empty() {
        return Err("--program is required".into());
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

    Ok(Options {
        plan,
        state,
        repo,
        susi_program,
        program,
        gateway_url,
        model,
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
            "--program",
            "openclaw.cmd",
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
