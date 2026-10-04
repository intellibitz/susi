//! Full-repository promotion gate for self-patches (VC-201-015).
//!
//! A candidate is promotion-ready only when fmt, clippy `-D warnings`, and
//! workspace tests all report success. Missing, skipped, or failing checks
//! block promotion.

use crate::susi_core::self_build::VERIFY_COMMAND;
use crate::susi_error::{EaiError, EaiResult};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

/// The three checks Mandate 48 / AGENTS.md require before promoting a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateCheck {
    Fmt,
    Clippy,
    Test,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckOutcome {
    Pass,
    Fail,
    Missing,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateReport {
    pub fmt: CheckOutcome,
    pub clippy: CheckOutcome,
    pub test: CheckOutcome,
    /// Only a report produced by [`run_repository_gate`] is trusted for
    /// promotion.  Transcript parsing is diagnostic and deliberately cannot
    /// manufacture execution evidence.
    #[serde(skip)]
    verified_execution: bool,
}

impl GateReport {
    #[must_use]
    pub fn promotion_ready(&self) -> bool {
        self.verified_execution
            && matches!(self.fmt, CheckOutcome::Pass)
            && matches!(self.clippy, CheckOutcome::Pass)
            && matches!(self.test, CheckOutcome::Pass)
    }

    #[must_use]
    pub fn blocking_reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
        if !self.verified_execution {
            reasons.push("execution");
        }
        for (name, out) in [
            ("fmt", self.fmt),
            ("clippy", self.clippy),
            ("test", self.test),
        ] {
            match out {
                CheckOutcome::Pass => {}
                CheckOutcome::Fail => reasons.push(name),
                CheckOutcome::Missing => reasons.push(name),
                CheckOutcome::Skipped => reasons.push(name),
            }
        }
        reasons
    }
}

/// Parse a recorded verify-command transcript into a gate report.
///
/// Expected shape (one line per check, order flexible):
/// `fmt: pass|fail|missing|skipped` and likewise for `clippy` / `test`.
#[must_use]
pub fn evaluate_transcript(transcript: &str) -> GateReport {
    let mut report = GateReport {
        fmt: CheckOutcome::Missing,
        clippy: CheckOutcome::Missing,
        test: CheckOutcome::Missing,
        verified_execution: false,
    };
    for line in transcript.lines() {
        let line = line.trim().to_ascii_lowercase();
        let (key, rest) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let outcome = parse_outcome(rest.trim());
        match key.trim() {
            "fmt" => merge_transcript_outcome(&mut report.fmt, outcome),
            "clippy" => merge_transcript_outcome(&mut report.clippy, outcome),
            "test" | "tests" => merge_transcript_outcome(&mut report.test, outcome),
            _ => {}
        }
    }
    report
}

fn parse_outcome(s: &str) -> CheckOutcome {
    match s {
        "pass" | "ok" | "passed" => CheckOutcome::Pass,
        "fail" | "failed" | "error" => CheckOutcome::Fail,
        "skipped" | "skip" => CheckOutcome::Skipped,
        _ => CheckOutcome::Missing,
    }
}

fn merge_transcript_outcome(current: &mut CheckOutcome, next: CheckOutcome) {
    if matches!(current, CheckOutcome::Missing) {
        *current = next;
    } else if *current != next {
        // Conflicting receipts are not resolved by trusting the last line.
        *current = CheckOutcome::Fail;
    }
}

/// Build a report only after the three commands have actually been invoked.
/// This narrow constructor is `pub(crate)` so deterministic tests can model
/// process outcomes without recursively launching Cargo.
pub(crate) fn executed_report(
    fmt: CheckOutcome,
    clippy: CheckOutcome,
    test: CheckOutcome,
) -> GateReport {
    GateReport {
        fmt,
        clippy,
        test,
        verified_execution: true,
    }
}

fn run_cargo_check(workspace: &Path, args: &[&str]) -> EaiResult<CheckOutcome> {
    let status = Command::new("cargo")
        .args(args)
        .current_dir(workspace)
        .status()
        .map_err(|error| {
            EaiError::process(format!(
                "repository gate could not run `cargo {}`: {error}",
                args.join(" ")
            ))
        })?;
    Ok(if status.success() {
        CheckOutcome::Pass
    } else {
        CheckOutcome::Fail
    })
}

/// Execute the complete self-build gate in `workspace`.
///
/// The three commands are launched independently so a failure is recorded for
/// the specific check and never hidden behind a caller-supplied transcript.
/// A report is promotion-ready only when this function produced it and every
/// command exited successfully.
pub fn run_repository_gate(workspace: &Path) -> EaiResult<GateReport> {
    if !workspace.is_dir() {
        return Err(EaiError::filesystem(format!(
            "repository gate workspace is not a directory: {}",
            workspace.display()
        )));
    }
    let fmt = run_cargo_check(workspace, &["fmt", "--all", "--check"])?;
    let clippy = run_cargo_check(
        workspace,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    let test = run_cargo_check(workspace, &["test", "--workspace", "--locked"])?;
    Ok(executed_report(fmt, clippy, test))
}

/// The command string a candidate must clear (same as self-build).
#[must_use]
pub fn required_verify_command() -> &'static str {
    VERIFY_COMMAND
}

/// Whether `cmd` is exactly the required full-gate command.
#[must_use]
pub fn command_covers_full_gate(cmd: &str) -> bool {
    let actual: Vec<&str> = cmd.split_whitespace().collect();
    let required: Vec<&str> = VERIFY_COMMAND.split_whitespace().collect();
    actual == required
}
