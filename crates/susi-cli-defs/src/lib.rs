#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

//! Dependency-light `clap` CLI surface definitions shared by `susi` and the
//! `susi-cli` thin client (`D-SP-4`).
//!
//! This crate intentionally carries no package version attribute on the root
//! parser: each binary attaches its own `env!("CARGO_PKG_VERSION")` at parse
//! time via `Cli::command().version(env!("CARGO_PKG_VERSION"))`, so `--version`
//! always reports the running executable's package version.

/// Wall-clock budget in seconds for `susi tasks verify-merged` (`20 * 60`).
///
/// Mirrors `susi_gawd::admin::tasks::VERIFY_BUDGET_SECS` without pulling
/// `susi-gawd` into the CLI definition dependency closure; a root test pins
/// equality between the two constants.
pub const VERIFY_BUDGET_SECS: u64 = 20 * 60;

/// Semver level to bump when cutting an engine release via
/// `susi admin release --cut <patch|minor|major>`.
///
/// Mirrors `susi_gawd::admin::VersionBump` with an identical `FromStr`
/// implementation so `render_help()` stays byte-identical (`--cut <CUT>`) and
/// `susi-cli-defs` stays independent of `susi-gawd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionBump {
    /// Increment the patch component (`x.y.Z`).
    Patch,
    /// Increment the minor component (`x.Y.0`).
    Minor,
    /// Increment the major component (`X.0.0`).
    Major,
}

impl std::str::FromStr for VersionBump {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "patch" => Ok(Self::Patch),
            "minor" => Ok(Self::Minor),
            "major" => Ok(Self::Major),
            other => Err(format!(
                "'{other}' is not a valid version bump (expected patch, minor, or major)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{VersionBump, VERIFY_BUDGET_SECS};
    use std::str::FromStr;

    #[test]
    fn version_bump_parses_all_levels_case_insensitively() {
        assert_eq!(VersionBump::from_str("patch"), Ok(VersionBump::Patch));
        assert_eq!(VersionBump::from_str("MINOR"), Ok(VersionBump::Minor));
        assert_eq!(VersionBump::from_str("Major"), Ok(VersionBump::Major));
        assert!(VersionBump::from_str("invalid").is_err());
    }

    #[test]
    fn verify_budget_secs_is_twenty_minutes() {
        assert_eq!(VERIFY_BUDGET_SECS, 1200);
    }
}
