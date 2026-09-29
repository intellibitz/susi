#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! The release gate's end-to-end checks (`susi_gawd::admin::E2E_CHECKS`) run
//! here against the built binary on every test run, so a check that no longer
//! matches reality fails in CI — not first at release time.

use susi_gawd::admin::{SusiAdmin, E2E_CHECKS};

#[test]
fn every_release_e2e_check_passes_against_the_built_binary() {
    let scratch = std::env::temp_dir().join(format!("susi-e2e-checks-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    let passed =
        SusiAdmin::run_e2e_checks(std::path::Path::new(env!("CARGO_BIN_EXE_susi")), &scratch)
            .unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&scratch);
    assert_eq!(passed.len(), E2E_CHECKS.len());
}

/// The gate's scratch dir sits under `target/` inside the repo being released.
/// A check must never discover that surrounding repo (the v0.18.0 cut aborted
/// when `susi tasks` listed the real queue instead of an empty one).
#[test]
fn e2e_checks_do_not_discover_a_surrounding_repo() {
    let outer = std::env::temp_dir().join(format!("susi-e2e-surrounding-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&outer);
    std::fs::create_dir_all(outer.join(".agents/tasks")).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&outer)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "--quiet"]);
    // A task the checks must NOT see.
    std::fs::write(
        outer.join(".agents/tasks/T-OUTER-1.json"),
        r#"{"id":"T-OUTER-1","title":"outer","accept":{"cmd":["cargo","--version"]},"created_by":"OUTER","created_unix":0}"#,
    )
    .unwrap();
    let scratch = outer.join("target").join("release-e2e-scratch");
    let passed =
        SusiAdmin::run_e2e_checks(std::path::Path::new(env!("CARGO_BIN_EXE_susi")), &scratch)
            .unwrap_or_else(|e| panic!("{e}"));
    let _ = std::fs::remove_dir_all(&outer);
    assert_eq!(passed.len(), E2E_CHECKS.len());
}
