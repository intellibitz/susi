//! Zero-config qualification gate for a release.
//!
//! A release is qualified only when all three hold:
//! 1. The fresh-HOME journey passes with nothing but the install command
//!    and an optional key — no required files or env vars.
//! 2. The config-debt score did not rise against the pinned baseline.
//! 3. The remaining limits (secrets, consent, missing hardware) are
//!    stated in the release notes — rendered here from the live debt
//!    scan so they cannot silently drift from reality.

use std::path::Path;

use crate::setup_workflow::SetupPlan;
use crate::susi_error::{EaiError, EaiResult};
use crate::zc_debt_ratchet::{ratchet_check, RatchetVerdict};
use crate::zc_debt_report::{scan_debt, DebtItem, DebtKind};
use crate::SusiConfig;

/// Repo-pinned debt baseline (`.agents/config-debt-baseline.json`).
pub const DEBT_BASELINE_REL: &str = ".agents/config-debt-baseline.json";

/// The three-part verdict.
#[derive(Debug)]
pub struct Qualification {
    /// Fresh-HOME journey result.
    pub journey_ok: bool,
    /// Debt ratchet outcome.
    pub ratchet: RatchetVerdict,
    /// Remaining limits that must be named in the release notes.
    pub limits: Vec<DebtItem>,
}

impl Qualification {
    /// A release is qualified when the journey passes and the debt score
    /// did not rise (Improved is welcome; Regressed blocks).
    #[must_use]
    pub fn qualified(&self) -> bool {
        self.journey_ok && !matches!(self.ratchet, RatchetVerdict::Regressed { .. })
    }

    /// The release-notes section naming every remaining limit — the gate
    /// requires this text ship in the notes, so it is generated from the
    /// live scan rather than hand-written.
    #[must_use]
    pub fn release_notes_section(&self) -> String {
        let mut out = String::from("## Zero-config qualification\n\n");
        out.push_str(&format!(
            "- Fresh-HOME journey: {}\n",
            if self.journey_ok { "pass" } else { "FAIL" }
        ));
        match &self.ratchet {
            RatchetVerdict::Improved { now, was } => {
                out.push_str(&format!("- Config debt: {now} (down from {was})\n"));
            }
            RatchetVerdict::Holds { count } => {
                out.push_str(&format!("- Config debt: {count} (at baseline)\n"));
            }
            RatchetVerdict::Regressed { now, max, new_ids } => {
                out.push_str(&format!(
                    "- Config debt: {now} (REGRESSED above {max}; new: {})\n",
                    new_ids.join(", ")
                ));
            }
        }
        out.push_str("\n### Remaining limits\n\n");
        if self.limits.is_empty() {
            out.push_str("None — no required inputs remain.\n");
        } else {
            for l in &self.limits {
                let kind = match l.kind {
                    DebtKind::Secret => "secret",
                    DebtKind::Consent => "consent",
                    DebtKind::EnvVar => "env var",
                    DebtKind::Prompt => "prompt",
                    DebtKind::SetupStep => "setup step",
                };
                out.push_str(&format!(
                    "- [{kind}] {} (removes via {})\n",
                    l.description, l.removes_via
                ));
            }
        }
        out
    }
}

/// Run the gate: fresh-HOME journey probe against `scratch_home` plus the
/// ratchet against `baseline_path`. `scratch_home` must be a throwaway
/// dir — the probe creates nothing there, it only verifies defaults.
///
/// # Errors
/// [`EaiError::io`] when the baseline file is absent or corrupt.
pub fn qualify(scratch_home: &Path, baseline_path: &Path) -> EaiResult<Qualification> {
    // Part 1 — the journey: a clean HOME reaches a usable plan with no
    // required files or env vars. SusiConfig::default must already carry
    // every needed setting (derived/detected/defaulted).
    let cfg = SusiConfig::default();
    let plan = SetupPlan::detect(scratch_home);
    let required_file = scratch_home.join("config").join("config.json");
    let journey_ok = !cfg.settings.is_empty() && !plan.steps.is_empty() && !required_file.is_file();

    // Part 2 — the debt score must not have risen.
    let ratchet = ratchet_check(baseline_path, &[])?;

    // Part 3 — the remaining limits the release notes must name.
    let limits = scan_debt(&[]);

    Ok(Qualification {
        journey_ok,
        ratchet,
        limits,
    })
}

/// Load the baseline from the repo and qualify. Errors when the baseline
/// is missing — an unpinned ratchet is a failed gate, not a pass.
///
/// # Errors
/// [`EaiError::config`] when `repo_root` lacks the baseline file;
/// [`EaiError::io`] on read/parse failures.
pub fn qualify_repo(repo_root: &Path, scratch_home: &Path) -> EaiResult<Qualification> {
    let baseline = repo_root.join(DEBT_BASELINE_REL);
    if !baseline.is_file() {
        return Err(EaiError::config(format!(
            "zero-config debt baseline not pinned at {DEBT_BASELINE_REL}"
        )));
    }
    qualify(scratch_home, &baseline)
}
