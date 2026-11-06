//! Autonomous planning loop CLI.
use anyhow::Result;
use std::path::Path;

pub use susi_cli_defs::plan::*;

pub fn execute(args: PlanArgs, workspace: &Path) -> Result<()> {
    let ama = susi_gawd::ama::SusiMasterAgent::new();
    let report = ama.solve_autonomous(
        &args.goal,
        workspace,
        env!("CARGO_PKG_VERSION"),
        args.max_steps,
    )?;
    println!("{}", serde_json::to_string_pretty(&report.plan_summary())?);
    Ok(())
}
