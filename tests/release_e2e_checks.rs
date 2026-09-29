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
