//! Many agents advance one release line in parallel without conflicts (CEO Mandate 74).
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../distill_compose.rs"]
mod distill_compose;
#[path = "../release_lanes.rs"]
mod release_lanes;

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = std::env::current_dir()
        .map_err(|error| error.to_string())
        .and_then(|root| release_lanes::run(&root, &args));
    match result {
        Ok(message) => {
            println!("{message}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("release-lanes: {message}");
            ExitCode::FAILURE
        }
    }
}
