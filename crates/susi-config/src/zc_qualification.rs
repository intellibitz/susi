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
    let required_file = scratch_home.join("config.json");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zc_debt_ratchet::DebtBaseline;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn qualification_verdict_and_release_notes_cover_holds_improved_regressed_and_all_debt_kinds() {
        let holds = Qualification {
            journey_ok: true,
            ratchet: RatchetVerdict::Holds { count: 2 },
            limits: Vec::new(),
        };
        assert!(holds.qualified());
        let holds_notes = holds.release_notes_section();
        assert!(holds_notes.contains("- Fresh-HOME journey: pass\n"));
        assert!(holds_notes.contains("- Config debt: 2 (at baseline)\n"));
        assert!(holds_notes.contains("None — no required inputs remain.\n"));

        let improved = Qualification {
            journey_ok: true,
            ratchet: RatchetVerdict::Improved { now: 1, was: 3 },
            limits: vec![
                DebtItem {
                    id: "1".into(),
                    kind: DebtKind::Secret,
                    description: "sec".into(),
                    removes_via: "T-1".into(),
                },
                DebtItem {
                    id: "2".into(),
                    kind: DebtKind::Consent,
                    description: "con".into(),
                    removes_via: "T-2".into(),
                },
                DebtItem {
                    id: "3".into(),
                    kind: DebtKind::EnvVar,
                    description: "env".into(),
                    removes_via: "T-3".into(),
                },
                DebtItem {
                    id: "4".into(),
                    kind: DebtKind::Prompt,
                    description: "prm".into(),
                    removes_via: "T-4".into(),
                },
                DebtItem {
                    id: "5".into(),
                    kind: DebtKind::SetupStep,
                    description: "stp".into(),
                    removes_via: "T-5".into(),
                },
            ],
        };
        assert!(improved.qualified());
        let improved_notes = improved.release_notes_section();
        assert!(improved_notes.contains("- Config debt: 1 (down from 3)\n"));
        assert!(improved_notes.contains("- [secret] sec (removes via T-1)\n"));
        assert!(improved_notes.contains("- [consent] con (removes via T-2)\n"));
        assert!(improved_notes.contains("- [env var] env (removes via T-3)\n"));
        assert!(improved_notes.contains("- [prompt] prm (removes via T-4)\n"));
        assert!(improved_notes.contains("- [setup step] stp (removes via T-5)\n"));

        let journey_failed = Qualification {
            journey_ok: false,
            ratchet: RatchetVerdict::Holds { count: 2 },
            limits: Vec::new(),
        };
        assert!(!journey_failed.qualified());
        assert!(journey_failed
            .release_notes_section()
            .contains("- Fresh-HOME journey: FAIL\n"));

        let regressed = Qualification {
            journey_ok: true,
            ratchet: RatchetVerdict::Regressed {
                now: 4,
                max: 2,
                new_ids: vec!["env:a".into(), "prompt:b".into()],
            },
            limits: Vec::new(),
        };
        assert!(!regressed.qualified());
        assert!(regressed
            .release_notes_section()
            .contains("- Config debt: 4 (REGRESSED above 2; new: env:a, prompt:b)\n"));
    }

    #[test]
    fn qualify_and_qualify_repo_enforce_clean_scratch_home_and_pinned_baseline() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "susi_zc_qual_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let repo_root = root.join("repo");
        let scratch_home = root.join("scratch");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::create_dir_all(&scratch_home).unwrap();

        // Missing baseline file fails `qualify_repo`.
        assert!(qualify_repo(&repo_root, &scratch_home).is_err());

        // Pin baseline matching current scan_debt(&[]).
        let baseline_path = repo_root.join(DEBT_BASELINE_REL);
        std::fs::create_dir_all(baseline_path.parent().unwrap()).unwrap();
        let baseline = DebtBaseline::from_scan(&[]);
        baseline.save(&baseline_path).unwrap();

        let ok_verdict = qualify_repo(&repo_root, &scratch_home).expect("qualify_repo succeeds");
        assert!(ok_verdict.journey_ok);
        assert!(ok_verdict.qualified());

        // Pre-existing config.json in scratch_home fails the fresh-HOME journey.
        std::fs::write(scratch_home.join("config.json"), b"{}").unwrap();
        let dirty_verdict =
            qualify(&scratch_home, &baseline_path).expect("qualify returns verdict");
        assert!(!dirty_verdict.journey_ok);
        assert!(!dirty_verdict.qualified());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn qualify_rejects_corrupt_baseline_json_and_unqualified_when_both_journey_and_ratchet_fail() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "susi_zc_qual_corrupt_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let scratch_home = root.join("scratch");
        std::fs::create_dir_all(&scratch_home).unwrap();
        let corrupt_baseline = root.join("corrupt-baseline.json");
        std::fs::write(&corrupt_baseline, "{ invalid json ").unwrap();

        assert!(qualify(&scratch_home, &corrupt_baseline).is_err());

        let both_failed = Qualification {
            journey_ok: false,
            ratchet: RatchetVerdict::Regressed {
                now: 5,
                max: 2,
                new_ids: vec!["env:extra".into()],
            },
            limits: Vec::new(),
        };
        assert!(!both_failed.qualified());
        let notes = both_failed.release_notes_section();
        assert!(notes.contains("- Fresh-HOME journey: FAIL\n"));
        assert!(notes.contains("- Config debt: 5 (REGRESSED above 2; new: env:extra)\n"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
