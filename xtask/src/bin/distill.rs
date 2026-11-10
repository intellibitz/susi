//! Plan-only release distillation: read task records at a Git revision and
//! print which tasks a release could include, defer or reject, and why.
//! Changes no branch, ref, index or working tree; writes only `--out`.
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../distill.rs"]
mod distill;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| distill::run(&root, &args));
    match result {
        Ok(message) => {
            print!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("distill: {message}");
            ExitCode::FAILURE
        }
    }
}
