// Live core CLI/daemon reachability contracts retained before implementation transfer.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // Original source-contract assertions.

#[test]
fn intent_ladder_startup_wiring_is_on_the_production_path() {
    // Boot slice + scheduled full run + on-demand CLI, all through the
    // same seams.
    let admin = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/runtime_admin.rs"
    ))
    .unwrap();
    for needle in [
        "intent_ladder::startup_slice",
        "intent_ladder::full_run",
        "intent_ladder::schedule_secs()",
        "\"intent_ladder\"",
    ] {
        assert!(
            admin.contains(needle),
            "runtime_admin.rs is missing `{needle}`"
        );
    }
    let defs = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/defs.rs"
    ))
    .unwrap();
    assert!(defs.contains("IntentLadder"));
    let cli = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/admin_cli.rs"
    ))
    .unwrap();
    // The CLI arm maps its flags onto `host_views::IntentLadder` and calls
    // the shared `susi_daemon::host_views::intent_ladder` (the MCP
    // `intent_ladder` tool calls the same function).
    for needle in [
        "AdminCommands::IntentLadder",
        "IntentLadder::Run",
        "IntentLadder::Scorecard",
        "susi_daemon::host_views::intent_ladder(&action, cwd)",
    ] {
        assert!(cli.contains(needle), "admin_cli.rs is missing `{needle}`");
    }
    let views = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/host_views.rs"
    ))
    .unwrap();
    // Production code only: the module's tests sit after `#[cfg(test)]`.
    let views = views.split("#[cfg(test)]").next().unwrap();
    let body = views
        .split("pub fn intent_ladder(action: &IntentLadder, cwd: &Path) -> R<Value> {")
        .nth(1)
        .and_then(|rest| rest.split("\n}").next())
        .expect("host_views::intent_ladder is production code");
    for needle in [
        "use susi_gawd::intent_ladder as ladder;",
        "ladder::full_run(",
        "ladder::startup_slice(",
        "ladder::read_scorecard(",
    ] {
        assert!(
            body.contains(needle),
            "host_views::intent_ladder is missing `{needle}`"
        );
    }
    assert!(
        views.contains("\"run\" => IntentLadder::Run")
            && views.contains("intent_ladder(&action, &workspace)"),
        "the intent_ladder host action must reach the same function"
    );
}

#[test]
fn durable_mission_state_production_wiring() {
    // `susi status` surfaces resumable workspace DAG missions — the
    // enumeration is production-reachable, not a test-only helper. The
    // report lives in `susi_daemon::status_report` (the MCP
    // `operator_status` tool returns the same document); `status_cli`
    // collects and renders it.
    let status_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/status_cli.rs"
    ))
    .expect("status_cli.rs");
    assert!(
        status_src.contains("use susi_daemon::status_report::{collect, measured_json, render};")
            && status_src.contains("let status = collect(")
            && status_src.contains("render(&status)"),
        "susi status must collect and render the shared status report"
    );
    let report = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/status_report.rs"
    ))
    .expect("status_report.rs");
    // Production code only: any tests sit after `#[cfg(test)]`.
    let report = report.split("#[cfg(test)]").next().expect("source");
    let fn_body = |signature: &str| -> String {
        report
            .split(signature)
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .unwrap_or_else(|| panic!("status_report.rs is missing `{signature}`"))
            .to_string()
    };
    assert!(
        fn_body("pub fn collect(").contains("read_resumable_missions(cwd)"),
        "susi status must enumerate resumable durable missions"
    );
    assert!(
        fn_body("pub fn render(s: &UnifiedStatus) -> String {").contains("resumable DAG missions"),
        "susi status must render the resume surface"
    );
}

#[test]
fn snapshot_rollback_production_wiring() {
    // The report lives in `susi_daemon::status_report` (the MCP
    // `operator_status` tool returns the same document); `status_cli`
    // collects and renders it.
    let status_src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../src/cli/status_cli.rs"
    ))
    .expect("status_cli.rs");
    assert!(
        status_src.contains("use susi_daemon::status_report::{collect, measured_json, render};")
            && status_src.contains("let status = collect(")
            && status_src.contains("render(&status)"),
        "susi status must collect and render the shared status report"
    );
    let report = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../susi-daemon/src/status_report.rs"
    ))
    .expect("status_report.rs");
    // Production code only: any tests sit after `#[cfg(test)]`.
    let report = report.split("#[cfg(test)]").next().expect("source");
    let fn_body = |signature: &str| -> String {
        report
            .split(signature)
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .unwrap_or_else(|| panic!("status_report.rs is missing `{signature}`"))
            .to_string()
    };
    assert!(
        fn_body("pub fn collect(").contains("read_rollback_ready(cwd)")
            && fn_body("fn read_rollback_ready(").contains("mission-snapshots")
            && fn_body("pub fn render(s: &UnifiedStatus) -> String {").contains("s.rollback_ready"),
        "susi status must surface failed missions holding snapshots"
    );
}
