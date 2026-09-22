//! Autonomous planning loop CLI.
use anyhow::Result;
use std::path::Path;

#[derive(Debug, clap::Args)]
pub struct PlanArgs {
    /// Natural language goal to solve via iterative planning.
    goal: String,
    /// Maximum number of planning steps.
    #[arg(short, long, default_value_t = 3)]
    max_steps: u32,
}

pub fn execute(args: PlanArgs, workspace: &Path) -> Result<()> {
    let ama = susi_gawd::ama::SusiMasterAgent::new();
    let report = ama.solve_autonomous(
        &args.goal,
        workspace,
        env!("CARGO_PKG_VERSION"),
        args.max_steps,
    )?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": report.status,
            "final_answer": report.final_answer,
            "agents": report.agents,
        }))?
    );
    Ok(())
}
