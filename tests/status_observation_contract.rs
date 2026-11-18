//! Keep the active status consumer connected to measured observation costs.

/// The status surface publishes the measured cost — the spend section
/// reads the observation ledger and renders it, pinned by source scan.
#[test]
fn audit_retention_prune_status_publishes_observation_cost(
) -> Result<(), Box<dyn std::error::Error>> {
    // The report lives in `susi_daemon::status_report` (the MCP
    // `operator_status` tool returns the same document); `status_cli`
    // collects and renders it.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let cli = std::fs::read_to_string(root.join("src/cli/status_cli.rs"))?;
    assert!(
        cli.contains("use susi_daemon::status_report::{collect, measured_json, render};")
            && cli.contains("let status = collect(")
            && cli.contains("render(&status)"),
        "susi status collects and renders the shared status report"
    );
    let report = std::fs::read_to_string(root.join("crates/susi-daemon/src/status_report.rs"))?;
    // Production code only: any tests sit after `#[cfg(test)]`.
    let report = report
        .split_once("#[cfg(test)]")
        .map_or(report.as_str(), |(production, _)| production);
    let fn_body = |signature: &str| -> Result<String, String> {
        report
            .split(signature)
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .map(str::to_owned)
            .ok_or_else(|| format!("status_report.rs is missing `{signature}`"))
    };
    assert!(
        fn_body("pub fn collect(")?.contains("read_spend(global_dir)"),
        "the status snapshot includes the spend section"
    );
    let spend = fn_body("fn read_spend(global_dir: &Path) -> SpendSection {")?;
    assert!(
        spend.contains("observation-cost.jsonl") && spend.contains("elapsed_ns"),
        "susi status reads the observation ledger"
    );
    let render = fn_body("pub fn render(s: &UnifiedStatus) -> String {")?;
    assert!(
        render.contains("observation:") && render.contains("observation_by_agent"),
        "the report renders bytes + overhead attributed per agent"
    );
    Ok(())
}
