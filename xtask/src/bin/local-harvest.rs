//! Harvest Pure Apache-2.0 code from any Git repository with per-file license
//! verification, Core-vs-Commercial smart routing, raw-branch discovery,
//! manifests, notices and audits (CEO Mandates 74 & 75).
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../harvest.rs"]
mod harvest;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let json = args.iter().any(|arg| arg == "--json");
    let result = std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| harvest::run(&root, &args));
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        // Machine output stays parseable on stdout; the exit code carries failure.
        Err(message) if json && message.starts_with('{') => {
            println!("{message}");
            ExitCode::FAILURE
        }
        Err(message) => {
            eprintln!("local-harvest: {message}");
            ExitCode::FAILURE
        }
    }
}
