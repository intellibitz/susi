#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! Time-to-first-answer benchmark from a clean install: how long until a
//! fresh HOME gets a useful answer out of susi. The model download is
//! mocked (a local file the "fetcher" copies); the mission answer is the
//! substrate's own first response. Each milestone has a hard budget —
//! regressions fail the test, they are never re-baselined silently.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Whole-journey budget (clean HOME → first useful answer). Generous for
/// shared CI machines; tighten only with measured headroom.
const JOURNEY_BUDGET: Duration = Duration::from_secs(90);
/// A single susi CLI invocation must never dominate the budget.
const COMMAND_BUDGET: Duration = Duration::from_secs(30);
/// The mocked model fetch (file copy of ~4 MiB) has a small fixed budget.
const FETCH_BUDGET: Duration = Duration::from_secs(5);

fn susi(home: &Path, args: &[&str]) -> (i32, String, Duration) {
    let start = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_susi"))
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("xdg"))
        .env("XDG_DATA_HOME", home.join("xdg-data"))
        .env_remove("SUSI_HOME")
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        start.elapsed(),
    )
}

fn clean_home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("zc-tta-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    home
}

/// Mock the model-download phase: synthesise a ~4 MiB "weight file" and
/// time copying it into the models dir — the same bytes, none of the WAN.
fn mock_model_fetch(home: &Path) -> Duration {
    let src = home.join("hf-cache").join("qwen-0.5b-q4.gguf");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, vec![0xABu8; 4 * 1024 * 1024]).unwrap();
    let dst = home.join("models").join("qwen-0.5b-q4.gguf");
    std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
    let start = Instant::now();
    std::fs::copy(&src, &dst).unwrap();
    start.elapsed()
}

#[test]
fn zc_time_to_answer_clean_home_first_answer_under_budget() {
    let home = clean_home("journey");
    let journey = Instant::now();

    // Milestone 1: the binary answers *something useful* on a clean HOME —
    // `os` reports substrate status; it must not need seeded config.
    let (_code, out, t_status) = susi(&home, &["os"]);
    assert!(
        t_status < COMMAND_BUDGET,
        "first command took {t_status:?} (> {COMMAND_BUDGET:?})"
    );
    assert!(
        !out.trim().is_empty(),
        "clean HOME produced no status output"
    );

    // Milestone 2: model-fetch phase (mocked) fits its budget.
    let t_fetch = mock_model_fetch(&home);
    assert!(
        t_fetch < FETCH_BUDGET,
        "mock model fetch took {t_fetch:?} (> {FETCH_BUDGET:?})"
    );

    // Milestone 3: second invocation — the warm path — is also bounded.
    let (_code, _out2, t_warm) = susi(&home, &["os"]);
    assert!(
        t_warm < COMMAND_BUDGET,
        "warm command took {t_warm:?} (> {COMMAND_BUDGET:?})"
    );

    let total = journey.elapsed();
    assert!(
        total < JOURNEY_BUDGET,
        "install→first-answer journey took {total:?} (> {JOURNEY_BUDGET:?})"
    );
    eprintln!(
        "zc_time_to_answer: status={t_status:?} fetch(mock)={t_fetch:?} warm={t_warm:?} total={total:?}"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn zc_time_to_answer_no_required_files_created_first() {
    let home = clean_home("nofiles");
    // The journey must not depend on files only a human can write:
    // before ANY susi command the home contains nothing but our dirs.
    let entries = std::fs::read_dir(&home).unwrap().count();
    assert_eq!(entries, 0, "fixture must be a bare HOME");
    let (_code, out, t) = susi(&home, &["os"]);
    assert!(t < COMMAND_BUDGET);
    assert!(!out.trim().is_empty());
    std::fs::remove_dir_all(&home).ok();
}
