//! Unified operator control surface CLI (`susi top`, VC-201-099). Since
//! 2026-11-17 (Mandate 84) Susi Commercial Apps serves it: this forwards to
//! `susi-commercial top` with the same flags, in this working directory.

use std::process::ExitCode;

pub use susi_cli_defs::top::*;

/// Execute `susi top` by forwarding to `susi-commercial top`.
pub fn execute(args: TopArgs) -> ExitCode {
    let mut forwarded = vec!["top".to_string()];
    if args.json {
        forwarded.push("--json".to_string());
    }
    if let Some(filter) = args.filter {
        forwarded.extend(["--filter".to_string(), filter]);
    }
    match susi_config::vault_exec::run_commercial(&forwarded, &[]) {
        Ok(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        Err(e) => {
            eprintln!("susi top is served by Susi Commercial Apps (`susi-commercial top`): {e}");
            ExitCode::FAILURE
        }
    }
}
