//! Integration: bare `susi` prints status and one next step.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable,
        clippy::wildcard_enum_match_arm
    )
)]

use susi_config::zc_bare_susi::{bare_susi_status, render_bare};

#[test]
fn zc_bare_susi_prints_status_and_next_step() {
    let s = bare_susi_status(false, false, false);
    let out = render_bare(&s);
    assert!(out.contains("not ready"));
    assert!(out.contains("daemon"));
    let ready = bare_susi_status(true, true, true);
    assert!(ready.ready);
    assert!(render_bare(&ready).contains("mission"));
}
