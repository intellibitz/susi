//! Unified operator control surface CLI (`susi top`, VC-201-099).

use std::path::Path;
use std::process::ExitCode;
use susi_gemi::operator_report::{
    render_operator_dimension, render_operator_state, sample_operator_state,
};

pub use susi_cli_defs::top::*;

/// Execute `susi top`.
pub fn execute(args: TopArgs, cwd: &Path) -> ExitCode {
    let state = sample_operator_state(cwd);
    if args.json {
        match serde_json::to_string_pretty(&state) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("Failed to serialize operator state to JSON: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else if let Some(dimension) = args.filter {
        let dimension = dimension.to_lowercase();
        let Some(report) = render_operator_dimension(&state, &dimension) else {
            eprintln!("Unknown filter dimension: '{dimension}'. Choose from: workload, resources, providers, config, federation, evidence.");
            return ExitCode::FAILURE;
        };
        print!("{report}");
    } else {
        print!("{}", render_operator_state(&state));
    }
    ExitCode::SUCCESS
}
