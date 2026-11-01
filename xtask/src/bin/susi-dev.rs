//! Native Rust process entry for the Cargo-built local bootstrap.
#![forbid(unsafe_code)]
#[cfg(windows)]
extern crate susi_windows_runtime;
#[allow(dead_code)]
#[path = "../../bootstrap/susi-dev.rs"]
mod bootstrap;
fn main() -> std::process::ExitCode {
    match bootstrap::run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("susi-dev: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
