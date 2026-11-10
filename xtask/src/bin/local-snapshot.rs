//! Freeze an exact main commit for later qualification, using Git storage only.
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../git_publish.rs"]
mod git_publish;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| git_publish::run(&root, &args));
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
