#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable
)]
#![allow(missing_docs)] // integration test crate: no public API to document

//! A tool installed from source in a job that also restores `rust-cache`.
//!
//! `Swatinem/rust-cache` saves and restores `~/.cargo/bin` along with the build
//! cache, so a runner can already hold the binary a `cargo install` step is about
//! to build - and `cargo install` refuses to overwrite a binary it did not install
//! ("binary `cargo-geiger` already exists in destination"). When the dedicated
//! 10 MB cache that normally short-circuits the install was evicted, every push of
//! every branch failed in `Unsafe Exposure Ratchet`, and a failed job never reaches
//! the post-step that would have saved the entry again, so nothing healed. Nothing
//! else notices: the workflow is valid YAML and the step is correct on a cold runner.

use std::path::PathBuf;

fn workflows() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows");
    let mut out: Vec<(String, String)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yml"))
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

/// The workflow without its comment lines, so prose about a fix cannot trip it.
fn code(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// (job id, job body): a job id is a key indented exactly two spaces under `jobs:`.
fn jobs(text: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut in_jobs = false;
    for line in code(text).lines() {
        if line.starts_with("jobs:") {
            in_jobs = true;
            continue;
        }
        if !in_jobs {
            continue;
        }
        let id = line
            .strip_prefix("  ")
            .and_then(|rest| rest.strip_suffix(':'))
            .filter(|rest| !rest.is_empty() && !rest.starts_with(' '));
        match id {
            Some(id) => out.push((id.to_string(), String::new())),
            None => {
                if let Some((_, body)) = out.last_mut() {
                    body.push_str(line);
                    body.push('\n');
                }
            }
        }
    }
    out
}

/// The steps of a job body, each as its own text.
fn steps(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in body.lines() {
        if line.starts_with("      - ") {
            out.push(String::new());
        }
        if let Some(step) = out.last_mut() {
            step.push_str(line);
            step.push('\n');
        }
    }
    out
}

#[test]
fn a_source_install_tolerates_the_binary_rust_cache_restored() {
    let mut workflows_checked = 0;
    for (name, text) in workflows() {
        workflows_checked += 1;
        for (id, body) in jobs(&text) {
            if !body.contains("Swatinem/rust-cache") {
                continue;
            }
            for step in steps(&body) {
                if !step.contains("cargo install") {
                    continue;
                }
                assert!(
                    step.contains("--force"),
                    "{name}: job `{id}` restores rust-cache (which restores ~/.cargo/bin) and \
                     then runs `cargo install` without --force: it fails with \"binary already \
                     exists in destination\" whenever the restored bin dir already holds the \
                     tool, which took down every branch's Unsafe Exposure Ratchet when its \
                     dedicated cache entry was evicted:\n{step}"
                );
            }
        }
    }
    assert!(
        workflows_checked >= 1,
        "workflow installation guards must inspect actual workflows"
    );
}

#[test]
fn native_unsafe_ratchet_does_not_install_or_cache_retired_geiger() {
    let workflow = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/test.yml"),
    )
    .unwrap();
    let parsed = jobs(&workflow);
    let (_, ratchet) = parsed
        .iter()
        .find(|(id, _)| id == "unsafe-ratchet")
        .unwrap();
    assert!(ratchet.contains(".github/workflows/unsafe-exposure-ratchet.sh"));
    assert!(ratchet.contains("Swatinem/rust-cache@v2"));
    assert!(ratchet.contains("cargo build --locked -p susi-unsafe-ratchet --bins"));
    assert!(ratchet.contains("cargo metadata --locked --format-version 1 --filter-platform"));
    assert!(ratchet.contains("refs/heads/main"));
    assert!(!ratchet.contains("cargo-geiger"));
    assert!(!ratchet.contains("cargo install"));
    let (_, merge) = parsed.iter().find(|(id, _)| id == "merge").unwrap();
    assert!(
        merge.contains("unsafe-ratchet"),
        "the required admission gate cannot be removed"
    );
}
