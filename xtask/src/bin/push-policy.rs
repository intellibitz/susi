//! Native Git pre-push transport safety entrypoint.
#![forbid(unsafe_code)]
#[cfg(windows)]
extern crate susi_windows_runtime;
#[path = "../push_policy.rs"]
mod push_policy;
use std::io::Read;
fn main() -> std::process::ExitCode {
    let result = (|| {
        let mut input = String::new();
        std::io::stdin()
            .take(1024 * 1024 + 1)
            .read_to_string(&mut input)
            .map_err(|e| e.to_string())?;
        if input.len() > 1024 * 1024 {
            return Err("push transaction exceeds limit".into());
        }
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        push_policy::validate(&root, &input)
    })();
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("pre-push: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
