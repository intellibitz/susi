//! Spend ceilings that refuse before the call is made (VC-202-005,
//! T-DEEPSEEK-102).
//!
//! The production cascade prices every priced candidate with
//! [`crate::engines::cost::expected_task_cost_usd`] and asks [`check`]
//! before `provider.generate` runs: a call that would cross an hourly,
//! daily, per-mission, per-task or per-vendor-daily ceiling is refused with
//! a typed [`BudgetRefusal`], the routing ladder steps down to the next —
//! hopefully cheaper — rung, and the local rung (expected cost `None`, i.e.
//! unpriced/free) always remains reachable. Every attempted call is then
//! [`record`]ed against the persisted usage ledger with agent, mission and
//! task attribution, so a ceiling's window sum is real spend, not a guess.
//!
//! Ceilings come from `budget_ceilings.json` in the config dir (or
//! `SUSI_BUDGET_CEILINGS_FILE`); an absent file means no caps — the
//! operator opts ceilings in, the enforcement path is always live.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::usage_accounting::{UsageLedger, UsageOutcome};

const HOUR_MS: u64 = 3_600_000;
const DAY_MS: u64 = 86_400_000;

/// Operator-declared spend ceilings. Every field is optional; an absent
/// axis is not capped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetCeilings {
    /// Rolling-hour USD ceiling across all providers.
    #[serde(default)]
    pub hourly_usd: Option<f64>,
    /// Rolling-24h USD ceiling across all providers.
    #[serde(default)]
    pub daily_usd: Option<f64>,
    /// USD ceiling per mission id.
    #[serde(default)]
    pub mission_usd: Option<f64>,
    /// USD ceiling for a single call (per task).
    #[serde(default)]
    pub task_usd: Option<f64>,
    /// vendor-name-substring -> rolling-24h USD cap for that vendor alone.
    #[serde(default)]
    pub vendor_daily_usd: BTreeMap<String, f64>,
}

impl BudgetCeilings {
    /// No axis capped — the fast path the cascade takes when the operator
    /// never configured a ceiling.
    #[must_use]
    pub fn is_uncapped(&self) -> bool {
        self.hourly_usd.is_none()
            && self.daily_usd.is_none()
            && self.mission_usd.is_none()
            && self.task_usd.is_none()
            && self.vendor_daily_usd.is_empty()
    }
}

/// Which ceiling refused the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetWindow {
    Task,
    Hourly,
    Daily,
    Mission,
    VendorDaily,
}

impl BudgetWindow {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Mission => "mission",
            Self::VendorDaily => "vendor-daily",
        }
    }
}

/// A typed pre-spend refusal: which ceiling, its cap, what was already
/// spent in the window, and what this call would have pushed it to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetRefusal {
    pub window: BudgetWindow,
    pub cap_usd: f64,
    pub spent_usd: f64,
    pub projected_usd: f64,
}

impl BudgetRefusal {
    /// One-line reason for the routing ladder / operator logs.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "over {} ceiling ${:.2} — spent ${:.4}, this call est. ${:.4} would reach ${:.4}",
            self.window.label(),
            self.cap_usd,
            self.spent_usd,
            self.projected_usd - self.spent_usd,
            self.projected_usd,
        )
    }
}

/// Pure ceiling check against an already-loaded ledger. `mission` scopes
/// the per-mission axis; `now_ms` anchors the rolling windows.
///
/// # Errors
/// [`BudgetRefusal`] naming the first ceiling the expected spend crosses.
pub fn check_in(
    ledger: &UsageLedger,
    ceilings: &BudgetCeilings,
    provider: &str,
    expected_usd: f64,
    mission: &str,
    now_ms: u64,
) -> Result<(), BudgetRefusal> {
    let refuse = |window: BudgetWindow, cap: f64, spent: f64| BudgetRefusal {
        window,
        cap_usd: cap,
        spent_usd: spent,
        projected_usd: spent + expected_usd,
    };
    if let Some(cap) = ceilings.task_usd {
        if expected_usd > cap {
            return Err(refuse(BudgetWindow::Task, cap, 0.0));
        }
    }
    if let Some(cap) = ceilings.hourly_usd {
        let spent = ledger.spend_usd(now_ms.saturating_sub(HOUR_MS), None, None);
        if spent + expected_usd > cap {
            return Err(refuse(BudgetWindow::Hourly, cap, spent));
        }
    }
    if let Some(cap) = ceilings.daily_usd {
        let spent = ledger.spend_usd(now_ms.saturating_sub(DAY_MS), None, None);
        if spent + expected_usd > cap {
            return Err(refuse(BudgetWindow::Daily, cap, spent));
        }
    }
    if let Some(cap) = ceilings.mission_usd {
        if !mission.is_empty() {
            let spent = ledger.spend_usd(0, None, Some(mission));
            if spent + expected_usd > cap {
                return Err(refuse(BudgetWindow::Mission, cap, spent));
            }
        }
    }
    let lower = provider.to_ascii_lowercase();
    for (vendor, cap) in &ceilings.vendor_daily_usd {
        if lower.contains(&vendor.to_ascii_lowercase()) {
            let spent = ledger.spend_usd(now_ms.saturating_sub(DAY_MS), Some(provider), None);
            if spent + expected_usd > *cap {
                return Err(refuse(BudgetWindow::VendorDaily, *cap, spent));
            }
        }
    }
    Ok(())
}

/// The installed ceilings file. `SUSI_BUDGET_CEILINGS_FILE` overrides the
/// path; under `cfg(test)` the override is required so hermetic tests never
/// read host state. An absent or unparsable file means no caps.
#[must_use]
pub fn ceilings() -> BudgetCeilings {
    let path =
        if let Some(p) = std::env::var_os("SUSI_BUDGET_CEILINGS_FILE").filter(|p| !p.is_empty()) {
            std::path::PathBuf::from(p)
        } else {
            if cfg!(test) {
                return BudgetCeilings::default();
            }
            susi_paths::SusiDirs::config_dir().join("budget_ceilings.json")
        };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Where the usage ledger persists — sibling of `brain_evidence.json`, the
/// path `susi status`'s spend axis already reads. `SUSI_USAGE_FILE`
/// overrides; `None` under `cfg(test)` without the override (hermetic tests
/// never touch `~/.susi*`).
#[must_use]
pub fn ledger_path() -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("SUSI_USAGE_FILE").filter(|p| !p.is_empty()) {
        Some(std::path::PathBuf::from(p))
    } else if cfg!(test) {
        None
    } else {
        Some(susi_paths::SusiDirs::config_dir().join("usage.json"))
    }
}

/// Load the persisted ledger; an unreadable or absent file yields an empty
/// ledger (a ceiling cannot gate what it cannot measure — fail open, and
/// `record` rewrites the file on the next outcome).
#[must_use]
pub fn load_ledger() -> UsageLedger {
    ledger_path()
        .and_then(|p| UsageLedger::load(&p).ok())
        .unwrap_or_default()
}

/// Installed-path check: loads ceilings and ledger, then [`check_in`].
/// `Ok(())` unconditionally when no ceiling is configured.
///
/// # Errors
/// [`BudgetRefusal`] when the expected spend would cross a configured cap.
pub fn check(provider: &str, expected_usd: f64, mission: &str) -> Result<(), BudgetRefusal> {
    let ceilings = ceilings();
    if ceilings.is_uncapped() {
        return Ok(());
    }
    check_in(
        &load_ledger(),
        &ceilings,
        provider,
        expected_usd,
        mission,
        now_ms(),
    )
}

/// Append one attributed spend record to the persisted ledger. Called for
/// every dispatch that reached `provider.generate` — success or failure,
/// the vendor billed the attempt.
pub fn record(
    provider: &str,
    class: crate::engines::brain::TaskClass,
    ok: bool,
    latency_ms: u64,
    usd: f64,
    mission: &str,
) {
    let Some(path) = ledger_path() else {
        return;
    };
    let mut ledger = UsageLedger::load(&path).unwrap_or_default();
    ledger.record(UsageOutcome {
        provider: provider.to_string(),
        task_class: class.label().to_string(),
        usage: crate::usage_accounting::TokenUsage::default(),
        success: ok,
        latency_ms,
        unix_ms: now_ms(),
        agent: std::env::var("SUSI_AGENT").unwrap_or_else(|_| "standalone".to_string()),
        mission: mission.to_string(),
        usd,
    });
    if let Err(e) = ledger.save(&path) {
        eprintln!(
            "[SPEND] failed to persist usage ledger to {}: {e}",
            path.display()
        );
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
