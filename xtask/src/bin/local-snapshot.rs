//! Submit development source and freeze qualification candidates using Git only.
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../git_publish.rs"]
mod git_publish;
#[path = "../storage_guard.rs"]
mod storage_guard;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| match args.split_first() {
            Some((command, rest)) if command == "guard" => storage_guard::run(&root, rest),
            Some(_) | None => git_publish::run(&root, &args),
        });
    if args
        .first()
        .is_some_and(|command| storage_guard::publishes(command))
    {
        let advisory = std::env::current_dir()
            .ok()
            .and_then(|root| storage_guard::witness(&root));
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
