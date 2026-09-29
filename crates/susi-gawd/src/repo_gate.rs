//! Full-repository promotion gate for self-patches (VC-201-015).
//!
//! A candidate is promotion-ready only when fmt, clippy `-D warnings`, and
//! workspace tests all report success. Missing, skipped, or failing checks
//! block promotion.

use crate::susi_core::self_build::VERIFY_COMMAND;
use serde::{Deserialize, Serialize};

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
}

impl GateReport {
    #[must_use]
    pub fn promotion_ready(&self) -> bool {
        matches!(self.fmt, CheckOutcome::Pass)
            && matches!(self.clippy, CheckOutcome::Pass)
            && matches!(self.test, CheckOutcome::Pass)
    }

    #[must_use]
    pub fn blocking_reasons(&self) -> Vec<&'static str> {
        let mut reasons = Vec::new();
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
    };
    for line in transcript.lines() {
        let line = line.trim().to_ascii_lowercase();
        let (key, rest) = match line.split_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let outcome = parse_outcome(rest.trim());
        match key.trim() {
            "fmt" => report.fmt = outcome,
            "clippy" => report.clippy = outcome,
            "test" | "tests" => report.test = outcome,
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

/// The command string a candidate must clear (same as self-build).
#[must_use]
pub fn required_verify_command() -> &'static str {
    VERIFY_COMMAND
}

/// Whether `cmd` covers all three required checks (substring presence).
#[must_use]
pub fn command_covers_full_gate(cmd: &str) -> bool {
    let c = cmd.to_ascii_lowercase();
    c.contains("cargo fmt")
        && c.contains("clippy")
        && c.contains("-d warnings")
        && c.contains("cargo test")
}
