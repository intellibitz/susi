#![allow(clippy::expect_used, clippy::panic)]

//! Regression coverage for avoiding an unnecessary post-merge CLI build.

#[test]
fn skip_rechecks_owed_attestations_after_setup() {
    let workflow = include_str!("../.github/workflows/test.yml");
    let step = workflow
        .split("      - name: Re-run the acceptance of freshly merged tasks")
        .nth(1)
        .and_then(|after| after.split("\n      - name:").next())
        .expect("post-merge acceptance step exists");
    let pending_check = step
        .find("if ! scripts/ci-verified-pending.sh; then")
        .expect("the queue is rechecked after environment setup");
    let cli_build = step
        .find("cargo run --locked -q -- tasks verify-merged --limit 2")
        .expect("the merged acceptance verifier remains the work step");

    assert!(
        pending_check < cli_build,
        "recheck the refs before compiling the verifier"
    );
    assert!(
        step[pending_check..cli_build].contains("exit 0"),
        "a drained queue must exit successfully without building susi"
    );
}

#[test]
fn ci_verified_pending_script_exists_and_checks_refs_verified() {
    let script = include_str!("../scripts/ci-verified-pending.sh");
    assert!(!script.trim().is_empty());
    assert!(
        script.contains("refs/verified") || script.contains("done/"),
        "ci-verified-pending.sh must inspect verified refs or done tasks"
    );
}

#[test]
fn test_workflow_defines_workflow_compliance_and_compile_check_jobs() {
    let workflow = include_str!("../.github/workflows/test.yml");
    assert!(workflow.contains("Workflow Compliance"));
    assert!(workflow.contains("Compile Check (branch pushes)"));
}

#[test]
fn post_merge_acceptance_limits_batch_verification_to_two_tasks() {
    let workflow = include_str!("../.github/workflows/test.yml");
    assert!(
        workflow.contains("tasks verify-merged --limit 2"),
        "post-merge acceptance must bound per-push verification batch size"
    );
}

#[test]
fn test_workflow_includes_unsafe_ratchet_and_merge_jobs() {
    let workflow = include_str!("../.github/workflows/test.yml");
    assert!(workflow.contains("Unsafe Exposure Ratchet"));
    assert!(workflow.contains("Re-run the acceptance of freshly merged tasks"));
}
