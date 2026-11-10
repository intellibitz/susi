//! Shell-free structural validator for the GMCP completion roadmap.
#![forbid(unsafe_code)]

#[cfg(windows)]
extern crate susi_windows_runtime;

#[path = "../gmcp_roadmap.rs"]
mod gmcp_roadmap;

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let root = args.next();
    if args.next().is_some() {
        eprintln!("usage: check-gmcp-roadmap [repository-root]");
        return ExitCode::FAILURE;
    }
    let root = root.as_deref().map(Path::new).unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap_or(Path::new("."))
    });
    match gmcp_roadmap::validate(root) {
        Ok(errors) if errors.is_empty() => {
            println!("{}", gmcp_roadmap::SUCCESS);
            ExitCode::SUCCESS
        }
        Ok(errors) => {
            for error in &errors {
                eprintln!("GMCP roadmap error: {error}");
            }
            eprintln!("GMCP roadmap invalid: {} error(s)", errors.len());
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("GMCP roadmap error: {error}");
            ExitCode::FAILURE
        }
    }
}
