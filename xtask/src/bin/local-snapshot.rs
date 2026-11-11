//! Submit development source and freeze qualification candidates using Git only.
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../git_publish.rs"]
mod git_publish;
// One source: the harness owns the guard and this tool compiles the same file.
#[path = "../../../crates/susi-harness/src/storage_guard.rs"]
mod storage_guard;

use std::process::ExitCode;

const GUARD_USAGE: &str = "usage: cargo run --offline --locked -p xtask --bin local-snapshot -- guard [<remote>]\nChecks that the remote's main and release lines only moved forward and that no tag moved or vanished since this clone last looked. Reads the remote and writes only local refs under refs/susi-guard/.";

/// `guard [<remote>]`: a finding is an error exit.
fn guard(root: &std::path::Path, args: &[String]) -> Result<String, String> {
    let remote = match args {
        [] => "origin",
        [remote] if remote != "--help" => remote.as_str(),
        _ => return Err(GUARD_USAGE.to_owned()),
    };
    let report = storage_guard::audit(root, remote)?;
    let summary = report.summary(remote);
    if report.findings.is_empty() {
        Ok(summary)
    } else {
        Err(format!("{summary}\n{}", report.findings.join("\n")))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| match args.split_first() {
            Some((command, rest)) if command == "guard" => guard(&root, rest),
            Some(_) | None => git_publish::run(&root, &args),
        });
    let publishes =
        |command: &String| matches!(command.as_str(), "submit" | "retry-main" | "cut" | "retry");
    if args.first().is_some_and(publishes) {
        // Every publish makes this clone a witness of the storage remote.
        let advisory = std::env::current_dir()
            .ok()
            .and_then(|root| storage_guard::witness(&root, "origin"));
        if let Some(advisory) = advisory {
            eprintln!("local-snapshot: {advisory}");
        }
    }
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("local-snapshot: {message}");
            ExitCode::FAILURE
        }
    }
}
