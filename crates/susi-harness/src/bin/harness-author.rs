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

//! Author a plan from a roadmap vector, through the metered gateway.

use serde_json::{json, Value};
use std::path::PathBuf;
use susi_harness::author::{author_plan, AuthorConfig, AuthorRequest};
use susi_harness::validate::{validate_plan, RepoFacts};

struct Options {
    gateway: String,
    model: String,
    repo: PathBuf,
    roadmap: PathBuf,
    vector: String,
    out: PathBuf,
    max_tasks: usize,
    timeout: u64,
    workers: Vec<String>,
}

fn main() {
    let options = match parse(std::env::args().skip(1)) {
        Ok(options) => options,
        Err(message) => fail(&message),
    };
    let text = std::fs::read_to_string(&options.roadmap)
        .unwrap_or_else(|error| fail(&format!("read {}: {error}", options.roadmap.display())));
    let roadmap: Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| fail(&format!("parse roadmap: {error}")));
    let entry = roadmap
        .get("vectors")
        .and_then(Value::as_array)
        .and_then(|vectors| {
            vectors.iter().find(|vector| {
                vector.get("id").and_then(Value::as_str) == Some(options.vector.as_str())
            })
        })
        .unwrap_or_else(|| fail(&format!("vector {} not found in roadmap", options.vector)));
    let title = entry.get("vector").and_then(Value::as_str).unwrap_or("");
    let target = entry
        .get("mastery_target")
        .and_then(Value::as_str)
        .unwrap_or("");
    let narrative = entry.get("progress").and_then(Value::as_str).unwrap_or("");
    let facts = match RepoFacts::load(&options.repo) {
        Ok(facts) => facts,
        Err(error) => fail(&error.to_string()),
    };
    let request = AuthorRequest {
        vector: format!("{} {}", options.vector, title),
        target: target.to_string(),
        packages: facts.packages.iter().cloned().collect(),
        workers: options.workers,
        narrative: tail(narrative, 4000),
        max_tasks: options.max_tasks,
        retry_hint: None,
    };
    let config = AuthorConfig {
        gateway_url: options.gateway,
        model: options.model,
        timeout_secs: options.timeout,
    };
    let plan = match author_plan(&config, &request) {
        Ok(plan) => plan,
        Err(error) => fail(&error.to_string()),
    };
    if let Err(error) = validate_plan(&plan, &facts) {
        fail(&format!("authored plan rejected: {error}"));
    }
    let document = serde_json::to_string_pretty(&plan).unwrap_or_default();
    if let Err(error) = std::fs::write(&options.out, &document) {
        fail(&format!("write {}: {error}", options.out.display()));
    }
    let summary = json!({
        "mission": plan.mission,
        "tasks": plan.tasks.iter().map(|task| task.key.clone()).collect::<Vec<_>>(),
        "out": options.out.display().to_string()
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
}

fn tail(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    text.get(text.len() - limit..).unwrap_or(text).to_string()
}

fn parse(mut args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut gateway = String::new();
    let mut model = String::new();
    let mut repo = PathBuf::from(".");
    let mut roadmap = PathBuf::new();
    let mut vector = String::new();
    let mut out = PathBuf::from("plan.json");
    let mut max_tasks: usize = 3;
    let mut timeout: u64 = 300;
    let mut workers: Vec<String> = vec!["deepseek-flash".into(), "gemini-flash".into()];
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--gateway" => gateway = take(&mut args, &argument)?,
            "--model" => model = take(&mut args, &argument)?,
            "--repo" => repo = PathBuf::from(take(&mut args, &argument)?),
            "--roadmap" => roadmap = PathBuf::from(take(&mut args, &argument)?),
            "--vector" => vector = take(&mut args, &argument)?,
            "--out" => out = PathBuf::from(take(&mut args, &argument)?),
            "--max-tasks" => {
                max_tasks = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "max-tasks must be a number".to_string())?;
            }
            "--workers" => {
                workers = take(&mut args, &argument)?
                    .split(',')
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
                    .collect();
            }
            "--timeout" => {
                timeout = take(&mut args, &argument)?
                    .parse()
                    .map_err(|_| "timeout must be a number of seconds".to_string())?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if gateway.trim().is_empty() || model.trim().is_empty() {
        return Err("--gateway and --model are required".into());
    }
    if roadmap.as_os_str().is_empty() {
        return Err("--roadmap is required".into());
    }
    if vector.trim().is_empty() {
        return Err("--vector is required".into());
    }
    Ok(Options {
        gateway,
        model,
        repo,
        roadmap,
        vector,
        out,
        max_tasks,
        timeout,
        workers,
    })
}

fn take(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    args.next().ok_or_else(|| format!("{flag} needs a value"))
}

fn fail(message: &str) -> ! {
    eprintln!("harness-author: {message}");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args<'a>(list: &'a [&'a str]) -> impl Iterator<Item = String> + 'a {
        list.iter().map(|value| (*value).to_string())
    }

    #[test]
    fn parses_the_required_flags_and_defaults() {
        let options = parse(args(&[
            "--gateway",
            "http://127.0.0.1:9188",
            "--model",
            "deepseek-flash",
            "--roadmap",
            ".agents/roadmap.json",
            "--vector",
            "VC-203-001",
        ]))
        .unwrap();
        assert_eq!(options.max_tasks, 3);
        assert_eq!(options.out, PathBuf::from("plan.json"));
        assert_eq!(options.repo, PathBuf::from("."));
    }

    #[test]
    fn refuses_missing_required_flags() {
        assert!(parse(args(&["--gateway", "g"])).is_err());
        assert!(parse(args(&["--nope"])).is_err());
    }
}
