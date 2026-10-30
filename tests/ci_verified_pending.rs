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
