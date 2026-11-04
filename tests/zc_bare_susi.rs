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

#[test]
fn zc_bare_susi_guides_missing_keys_when_daemon_is_up() {
    let status = bare_susi_status(true, true, false);
    assert!(!status.ready);
    assert!(status.next_step.contains("cloud key"));
    let rendered = render_bare(&status);
    assert!(rendered.starts_with("susi status: not ready\n"));
    assert!(rendered.contains("cloud key"));
}

#[test]
fn zc_bare_susi_guides_missing_model_when_daemon_and_keys_are_ok() {
    let status = bare_susi_status(true, false, true);
    assert!(!status.ready);
    assert!(status.next_step.contains("ecosystem scan"));
    let rendered = render_bare(&status);
    assert!(rendered.starts_with("susi status: not ready\n"));
    assert!(rendered.contains("local model"));
}

#[test]
fn zc_bare_susi_serde_roundtrip_and_two_line_render_contract() {
    for (d, m, k) in [
        (false, false, false),
        (true, false, false),
        (true, false, true),
        (true, true, true),
    ] {
        let status = bare_susi_status(d, m, k);
        let json = serde_json::to_string(&status).expect("serialize BareStatus");
        let decoded: susi_config::zc_bare_susi::BareStatus =
            serde_json::from_str(&json).expect("deserialize BareStatus");
        assert_eq!(decoded, status);
        let rendered = render_bare(&status);
        assert_eq!(rendered.lines().count(), 2);
    }
}

#[test]
fn zc_bare_susi_ready_status_has_ready_banner_and_next_step_prefix() {
    let status = bare_susi_status(true, true, true);
    assert!(status.ready);
    assert!(!status.next_step.is_empty());
    let rendered = render_bare(&status);
    assert!(rendered.starts_with("susi status: ready\n"));
    assert!(rendered.contains("next: "));
}
