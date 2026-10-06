#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The nightly `Cleanup Workflow Runs` job must keep the Actions backlog small:
//! only the newest few completed runs per workflow, and the surplus ages out
//! fast. At KEEP_LATEST=10 with a 7-day floor the tab accumulated ~300 finished
//! runs before anybody noticed; this bound exists so it cannot silently regrow.
//! The workflow is read as text - the root crate carries no YAML parser and
//! this does not justify one (same convention as ci_workflow_perf.rs).

use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn cleanup_runs_retention_stays_bounded() {
    let text = std::fs::read_to_string(root().join(".github/workflows/cleanup-runs.yml"))
        .unwrap_or_else(|e| panic!("read cleanup-runs.yml: {e}"));
    // Comment lines are stripped so prose explaining the policy cannot trip
    // or satisfy the check.
    let body = text
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("KEEP_LATEST: '5'"),
        "cleanup-runs.yml must keep only the newest 5 completed runs per workflow"
    );
    assert!(
        body.contains("RETAIN_DAYS: ${{ inputs.retain_days || '2' }}"),
        "cleanup-runs.yml must age out surplus runs within 2 days, so a busy week \
         cannot rebuild the backlog between nightly runs"
    );
}
