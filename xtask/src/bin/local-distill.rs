//! Distill labelled development contributions and compose feature/release branches using Git only.
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../distill_compose.rs"]
mod distill_compose;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| distill_compose::run(&root, &args));
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("local-distill: {message}");
            ExitCode::FAILURE
        }
    }
}
