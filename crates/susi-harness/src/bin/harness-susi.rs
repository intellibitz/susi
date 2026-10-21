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

//! Run one agent task against susi itself.
//!
//! The adapter prepares a worktree, adds and claims a task, runs OpenClaw in
//! that worktree, verifies the commit and the acceptance, and lands the work
//! through `workflow finish`.

use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use susi_harness::adapter::{land, run_task, SusiRunPolicy, SusiTarget, SusiTaskSpec};
use susi_harness::executor::{OpenClaw, RunOutcome};

struct RunOptions {
    repo: PathBuf,
    susi_program: PathBuf,
    spec: SusiTaskSpec,
}

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => fail(&message),
    };
    let target = SusiTarget::new(options.repo, options.susi_program);
    eprintln!(
        "harness-susi: preparing {} and running the agent; this can take minutes",
        options.spec.worktree_name
    );
    let report = match run_task(
        &target,
        &options.spec,
        &OpenClaw,
        &SusiRunPolicy::unmetered(),
    ) {
        Ok(report) => report,
        Err(error) => fail(&error.to_string()),
    };
    let final_text = match &report.agent {
        RunOutcome::Succeeded { final_text, .. } => final_text.clone(),
        RunOutcome::Failed { message } => message.clone(),
        RunOutcome::TimedOut => "timed out".to_string(),
        RunOutcome::Stalled { message } => message.clone(),
    };
    let summary = json!({
        "task": report.task,
        "worktree": report.worktree.display().to_string(),
        "committed": report.committed,
        "accepted": report.accepted,
        "agent": describe(&report.agent),
        "final": final_text
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if !report.accepted {
        fail("the worktree was not a committed, accepted task");
    }
    eprintln!(
        "harness-susi: landing {} through workflow finish",
        report.task
    );
    match land(&target, &report) {
        Ok(output) => {
            let tail = if output.stderr.trim().is_empty() {
                output.stdout.trim()
            } else {
                output.stderr.trim()
            };
            eprintln!("harness-susi: finish said: {tail}");
        }
        Err(error) => fail(&error.to_string()),
    }
}

fn describe(outcome: &RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Succeeded { .. } => "succeeded",
        RunOutcome::Failed { .. } => "failed",
        RunOutcome::TimedOut => "timed_out",
        RunOutcome::Stalled { .. } => "stalled",
    }
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<RunOptions, String> {
    let mut repo = PathBuf::new();
    let mut susi_program = PathBuf::new();
    let mut worktree_name = String::new();
    let mut title = String::new();
    let mut accept = String::new();
    let mut scopes: Vec<String> = Vec::new();
    let mut gateway = String::new();
    let mut model = String::new();
    let mut alternates = Vec::new();
    let mut program = PathBuf::new();
    let mut timeout: u64 = 600;
    let mut state = PathBuf::new();
    let mut prompt_words: Vec<String> = Vec::new();

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--repo" => repo = PathBuf::from(take(&mut args, &argument)?),
            "--susi" => susi_program = PathBuf::from(take(&mut args, &argument)?),
            "--name" => worktree_name = take(&mut args, &argument)?,
            "--title" => title = take(&mut args, &argument)?,
            "--accept" => accept = take(&mut args, &argument)?,
            "--scope" => scopes.push(take(&mut args, &argument)?),
            "--gateway" => gateway = take(&mut args, &argument)?,
            "--model" => model = take(&mut args, &argument)?,
            "--alternate" => alternates.push(take(&mut args, &argument)?),
            "--program" => program = PathBuf::from(take(&mut args, &argument)?),
            "--timeout" => {
                timeout = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "timeout must be a number of seconds".to_string())?;
            }
            "--state" => state = PathBuf::from(take(&mut args, &argument)?),
            other if other.starts_with("--") => {
                return Err(format!("unknown argument {other}"));
            }
            other => prompt_words.push(other.to_string()),
        }
    }

    if repo.as_os_str().is_empty() {
        return Err("--repo is required".into());
    }
    if susi_program.as_os_str().is_empty() {
        return Err("--susi is required".into());
    }
    if worktree_name.trim().is_empty() {
        return Err("--name is required".into());
    }
    if title.trim().is_empty() {
        return Err("--title is required".into());
    }
    if accept.trim().is_empty() {
        return Err("--accept is required".into());
    }
    if scopes.is_empty() {
        return Err("at least one --scope is required".into());
    }
    if gateway.trim().is_empty() || model.trim().is_empty() {
        return Err("--gateway and --model are required".into());
    }
    if program.as_os_str().is_empty() {
        return Err("--program is required".into());
    }
    if state.as_os_str().is_empty() {
        return Err("--state is required".into());
    }
    if prompt_words.is_empty() {
        return Err("a prompt is required".into());
    }

    Ok(RunOptions {
        repo,
        susi_program,
        spec: SusiTaskSpec {
            worktree_name,
            title,
            accept,
            scopes,
            prompt: prompt_words.join(" "),
            program,
            gateway_url: gateway,
            model,
            alternates,
            timeout: Duration::from_secs(timeout),
            state_dir: state,
            task: None,
        },
    })
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn fail(message: &str) -> ! {
    eprintln!("harness-susi: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parses_a_complete_run() {
        let options = parse(args(&[
            "--repo",
            "repo",
            "--susi",
            "susi.exe",
            "--name",
            "smoke",
            "--title",
            "docs",
            "--accept",
            "cargo test -p susi-harness",
            "--scope",
            "docs/harness.md",
            "--gateway",
            "http://127.0.0.1:9188",
            "--model",
            "deepseek-flash",
            "--program",
            "openclaw.cmd",
            "--state",
            "state",
            "write",
            "the",
            "docs",
        ]))
        .unwrap();
        assert_eq!(options.spec.worktree_name, "smoke");
        assert_eq!(options.spec.prompt, "write the docs");
        assert_eq!(options.spec.scopes, vec!["docs/harness.md".to_string()]);
    }

    #[test]
    fn refuses_missing_required_flags() {
        assert!(parse(args(&["--repo", "r"])).is_err());
        assert!(parse(args(&["--attempt", "nope"])).is_err());
        assert!(parse(args(&["--nope"])).is_err());
    }
}
