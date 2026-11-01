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

#[path = "../../../xtask/src/vendor_regressions.rs"]
mod vendor_regressions;

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
    #[serde(default = "missing")]
    pub policy: CheckOutcome,
    /// Only a report produced by [`run_repository_gate`] is trusted for
    /// promotion.  Transcript parsing is diagnostic and deliberately cannot
    /// manufacture execution evidence.
    #[serde(skip)]
    verified_execution: bool,
}

fn missing() -> CheckOutcome {
    CheckOutcome::Missing
}

impl GateReport {
    #[must_use]
    pub fn promotion_ready(&self) -> bool {
        self.verified_execution
            && matches!(self.fmt, CheckOutcome::Pass)
            && matches!(self.clippy, CheckOutcome::Pass)
            && matches!(self.test, CheckOutcome::Pass)
            && matches!(self.policy, CheckOutcome::Pass)
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
            ("policy", self.policy),
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
        policy: CheckOutcome::Missing,
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
        policy: CheckOutcome::Missing,
        verified_execution: true,
    }
}

fn run_cargo_check(workspace: &Path, args: &[&str]) -> EaiResult<CheckOutcome> {
    let status = cargo_command(workspace)?
        .args(args)
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
            "--features",
            "xtask/release-tooling",
            "--locked",
            "--offline",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    let test = run_cargo_check(
        workspace,
        &[
            "test",
            "--workspace",
            "--locked",
            "--offline",
            "--features",
            "xtask/release-tooling",
        ],
    )?;
    let mut report = executed_report(fmt, clippy, test);
    // Never call an older three-check report complete. Every policy command
    // must have executed successfully against this same candidate workspace.
    report.policy = run_policy_checks(workspace).unwrap_or(CheckOutcome::Fail);
    Ok(report)
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

fn cargo_command(workspace: &Path) -> EaiResult<Command> {
    let target = workspace.join("target/self-patch-gate");
    for path in [workspace.join("target"), target.clone()] {
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            let redirected = meta.file_type().is_symlink();
            #[cfg(windows)]
            let redirected = {
                use std::os::windows::fs::MetadataExt;
                redirected || meta.file_attributes() & 0x400 != 0
            };
            if redirected || !meta.is_dir() {
                return Err(EaiError::filesystem("unsafe self-patch output directory"));
            }
        }
    }
    let home = target.join("home");
    std::fs::create_dir_all(&home).map_err(|e| EaiError::filesystem(e.to_string()))?;
    let original = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .ok_or_else(|| EaiError::filesystem("missing toolchain home"))?;
    let mut command = Command::new("cargo");
    command.current_dir(workspace);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.starts_with("SUSI_")
            || [
                "TOKEN",
                "SECRET",
                "PASSWORD",
                "CREDENTIAL",
                "API_KEY",
                "ACCESS_KEY",
                "BASE_URL",
                "API_BASE",
                "API_URL",
                "PROXY",
                "CUSTOM_HEADERS",
            ]
            .iter()
            .any(|part| name.contains(part))
        {
            command.env_remove(key);
        }
    }
    command
        .env(
            "CARGO_HOME",
            std::env::var_os("CARGO_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| original.join(".cargo")),
        )
        .env(
            "RUSTUP_HOME",
            std::env::var_os("RUSTUP_HOME")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| original.join(".rustup")),
        )
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_BUILD_TARGET_DIR", &target)
        .env("CARGO_BUILD_BUILD_DIR", &target)
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("SUSI_HOME", home.join("susi"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"));
    Ok(command)
}

fn run_policy_checks(workspace: &Path) -> EaiResult<CheckOutcome> {
    let compiler = Command::new("rustc")
        .arg("-vV")
        .current_dir(workspace)
        .output()
        .map_err(|e| EaiError::process(e.to_string()))?;
    if !compiler.status.success() {
        return Ok(CheckOutcome::Fail);
    }
    let version = String::from_utf8_lossy(&compiler.stdout);
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| EaiError::process("compiler host unavailable"))?;
    if cfg!(windows) && !host.ends_with("-pc-windows-msvc") {
        return Ok(CheckOutcome::Fail);
    }
    let mut checks = vec![
        vec![
            "deny".into(),
            "--offline".into(),
            "--locked".into(),
            "check".into(),
            "licenses".into(),
        ],
        vec![
            "run".into(),
            "--offline".into(),
            "--locked".into(),
            "-p".into(),
            "xtask".into(),
            "--features".into(),
            "release-tooling".into(),
            "--bin".into(),
            "local-release".into(),
            "--".into(),
            "check-inventory".into(),
        ],
    ];
    checks.extend(
        vendor_regressions::commands(workspace, host, &workspace.join("target/self-patch-gate"))
            .map_err(|e| EaiError::process(e.to_string()))?,
    );
    for args in checks {
        let refs: Vec<_> = args.iter().map(String::as_str).collect();
        if run_cargo_check(workspace, &refs)? != CheckOutcome::Pass {
            return Ok(CheckOutcome::Fail);
        }
    }
    Ok(CheckOutcome::Pass)
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    #[test]
    fn three_checks_cannot_omit_policy() {
        let mut report =
            executed_report(CheckOutcome::Pass, CheckOutcome::Pass, CheckOutcome::Pass);
        assert!(!report.promotion_ready());
        assert!(report.blocking_reasons().contains(&"policy"));
        report.policy = CheckOutcome::Fail;
        assert!(!report.promotion_ready());
        report.policy = CheckOutcome::Pass;
        assert!(report.promotion_ready());
    }
}
