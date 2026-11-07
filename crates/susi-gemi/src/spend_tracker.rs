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

    /// Verify structural invariants on configured budget ceilings.
    pub fn verify_ceilings_invariants(&self) -> Result<(), String> {
        for (name, val) in [
            ("hourly_usd", self.hourly_usd),
            ("daily_usd", self.daily_usd),
            ("mission_usd", self.mission_usd),
            ("task_usd", self.task_usd),
        ] {
            if let Some(v) = val {
                if !v.is_finite() || v < 0.0 {
                    return Err(format!(
                        "BudgetCeilings {name} ({v}) must be finite and >= 0.0"
                    ));
                }
            }
        }
        if let (Some(hourly), Some(daily)) = (self.hourly_usd, self.daily_usd) {
            if hourly > daily {
                return Err(format!(
                    "BudgetCeilings hourly_usd ({hourly}) must be <= daily_usd ({daily})"
                ));
            }
        }
        for (vendor, cap) in &self.vendor_daily_usd {
            if vendor.trim().is_empty() {
                return Err(
                    "BudgetCeilings vendor_daily_usd vendor name must not be empty".to_string(),
                );
            }
            if !cap.is_finite() || *cap < 0.0 {
                return Err(format!(
                    "BudgetCeilings vendor_daily_usd[{vendor}] ({cap}) must be finite and >= 0.0"
                ));
            }
        }
        Ok(())
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

    /// Verify structural invariants on a pre-spend budget refusal.
    pub fn verify_refusal_invariants(&self) -> Result<(), String> {
        if !self.cap_usd.is_finite() || self.cap_usd < 0.0 {
            return Err(format!(
                "BudgetRefusal cap_usd ({}) must be finite and >= 0.0",
                self.cap_usd
            ));
        }
        if !self.spent_usd.is_finite() || self.spent_usd < 0.0 {
            return Err(format!(
                "BudgetRefusal spent_usd ({}) must be finite and >= 0.0",
                self.spent_usd
            ));
        }
        if !self.projected_usd.is_finite() || self.projected_usd < 0.0 {
            return Err(format!(
                "BudgetRefusal projected_usd ({}) must be finite and >= 0.0",
                self.projected_usd
            ));
        }
        if self.projected_usd < self.spent_usd {
            return Err(format!(
                "BudgetRefusal projected_usd ({}) must be >= spent_usd ({})",
                self.projected_usd, self.spent_usd
            ));
        }
        if self.projected_usd <= self.cap_usd {
            return Err(format!(
                "BudgetRefusal projected_usd ({}) must be > cap_usd ({})",
                self.projected_usd, self.cap_usd
            ));
        }
        Ok(())
    }
}

/// One proposed dispatch the governor prices: the provider it would run
/// on, the expected USD the catalog puts on the whole task, and the
/// mission id the spend would be attributed to.
#[derive(Debug, Clone, Copy)]
pub struct SpendIntent<'a> {
    pub provider: &'a str,
    pub expected_usd: f64,
    pub mission: &'a str,
}

impl SpendIntent<'_> {
    /// Verify structural invariants on a proposed spend intent.
    pub fn verify_intent_invariants(&self) -> Result<(), String> {
        if self.provider.trim().is_empty() {
            return Err("SpendIntent provider must not be empty".to_string());
        }
        if !self.expected_usd.is_finite() || self.expected_usd < 0.0 {
            return Err(format!(
                "SpendIntent expected_usd ({}) must be finite and >= 0.0",
                self.expected_usd
            ));
        }
        Ok(())
    }
}

/// Pure ceiling check against an already-loaded ledger. `now_ms` anchors
/// the rolling windows.
///
/// # Errors
/// [`BudgetRefusal`] naming the first ceiling the expected spend crosses.
pub fn check_in(
    ledger: &UsageLedger,
    ceilings: &BudgetCeilings,
    intent: &SpendIntent<'_>,
    now_ms: u64,
) -> Result<(), BudgetRefusal> {
    let refuse = |window: BudgetWindow, cap: f64, spent: f64| BudgetRefusal {
        window,
        cap_usd: cap,
        spent_usd: spent,
        projected_usd: spent + intent.expected_usd,
    };
    if let Some(cap) = ceilings.task_usd {
        if intent.expected_usd > cap {
            return Err(refuse(BudgetWindow::Task, cap, 0.0));
        }
    }
    if let Some(cap) = ceilings.hourly_usd {
        let spent = ledger.spend_usd(now_ms.saturating_sub(HOUR_MS), None, None);
        if spent + intent.expected_usd > cap {
            return Err(refuse(BudgetWindow::Hourly, cap, spent));
        }
    }
    if let Some(cap) = ceilings.daily_usd {
        let spent = ledger.spend_usd(now_ms.saturating_sub(DAY_MS), None, None);
        if spent + intent.expected_usd > cap {
            return Err(refuse(BudgetWindow::Daily, cap, spent));
        }
    }
    if let Some(cap) = ceilings.mission_usd {
        if !intent.mission.is_empty() {
            let spent = ledger.spend_usd(0, None, Some(intent.mission));
            if spent + intent.expected_usd > cap {
                return Err(refuse(BudgetWindow::Mission, cap, spent));
            }
        }
    }
    let lower = intent.provider.to_ascii_lowercase();
    for (vendor, cap) in &ceilings.vendor_daily_usd {
        if lower.contains(&vendor.to_ascii_lowercase()) {
            let spent =
                ledger.spend_usd(now_ms.saturating_sub(DAY_MS), Some(intent.provider), None);
            if spent + intent.expected_usd > *cap {
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
pub fn check(intent: &SpendIntent<'_>) -> Result<(), BudgetRefusal> {
    let ceilings = ceilings();
    if ceilings.is_uncapped() {
        return Ok(());
    }
    check_in(&load_ledger(), &ceilings, intent, now_ms())
}

/// One dispatched call as the ledger should remember it — the fields the
/// cascade supplies; the governor stamps agent, timestamp and class.
#[derive(Debug, Clone)]
pub struct SpendWrite<'a> {
    pub provider: &'a str,
    pub class: crate::engines::brain::TaskClass,
    pub ok: bool,
    pub latency_ms: u64,
    pub usd: f64,
    pub mission: &'a str,
}

/// Append one attributed spend record to the persisted ledger. Called for
/// every dispatch that reached `provider.generate` — success or failure,
/// the vendor billed the attempt. The same attribution also depletes the
/// worker's cap: a subscription window or prepaid balance is spent by real
/// dispatch, not by bookkeeping no caller performs (VC-202-021).
pub fn record(spend: &SpendWrite<'_>) {
    let cost_micros = if spend.usd.is_finite() && spend.usd > 0.0 {
        (spend.usd * 1_000_000.0).round() as u64
    } else {
        0
    };
    crate::susi_core::mission_trace::record_active_model_call(
        crate::susi_core::mission_trace::ModelCallCost {
            node: None,
            model: format!("{}|{}", spend.provider, spend.class.label()),
            tokens: 0,
            cost_micros,
        },
    );
    crate::worker::record_spend(spend.provider, spend.usd, now_ms() / 1_000);
    let Some(path) = ledger_path() else {
        return;
    };
    let mut ledger = UsageLedger::load(&path).unwrap_or_default();
    ledger.record(UsageOutcome {
        provider: spend.provider.to_string(),
        task_class: spend.class.label().to_string(),
        usage: crate::usage_accounting::TokenUsage::default(),
        success: spend.ok,
        latency_ms: spend.latency_ms,
        unix_ms: now_ms(),
        agent: std::env::var("SUSI_AGENT").unwrap_or_else(|_| "standalone".to_string()),
        mission: spend.mission.to_string(),
        usd: spend.usd,
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

#[cfg(test)]
// Test-only assertions in inline unit tests.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod inline_tests {
    use super::*;

    #[test]
    fn budget_ceilings_is_uncapped_only_when_all_fields_absent() {
        let mut c = BudgetCeilings::default();
        assert!(c.is_uncapped());
        c.task_usd = Some(0.50);
        assert!(!c.is_uncapped());
        c.task_usd = None;
        c.vendor_daily_usd.insert("openai".to_string(), 10.0);
        assert!(!c.is_uncapped());
    }

    #[test]
    fn budget_window_labels_and_refusal_describe_format_expected_fields() {
        assert_eq!(BudgetWindow::Task.label(), "task");
        assert_eq!(BudgetWindow::Hourly.label(), "hourly");
        assert_eq!(BudgetWindow::Daily.label(), "daily");
        assert_eq!(BudgetWindow::Mission.label(), "mission");
        assert_eq!(BudgetWindow::VendorDaily.label(), "vendor-daily");

        let refusal = BudgetRefusal {
            window: BudgetWindow::Hourly,
            cap_usd: 5.0,
            spent_usd: 4.5,
            projected_usd: 5.25,
        };
        let msg = refusal.describe();
        assert!(msg.contains("over hourly ceiling $5.00"));
        assert!(msg.contains("spent $4.5000"));
        assert!(msg.contains("would reach $5.2500"));
    }

    #[test]
    fn check_in_enforces_task_hourly_daily_mission_and_vendor_caps() {
        let mut ledger = UsageLedger::default();
        ledger.record(UsageOutcome {
            provider: "openai-gpt-4o".to_string(),
            task_class: "code".to_string(),
            usage: crate::usage_accounting::TokenUsage::default(),
            success: true,
            latency_ms: 100,
            unix_ms: 10_000_000,
            agent: "test".to_string(),
            mission: "m-1".to_string(),
            usd: 2.0,
        });

        let intent = SpendIntent {
            provider: "openai-gpt-4o",
            expected_usd: 1.5,
            mission: "m-1",
        };

        let task_cap = BudgetCeilings {
            task_usd: Some(1.0),
            ..Default::default()
        };
        assert_eq!(
            check_in(&ledger, &task_cap, &intent, 10_001_000)
                .unwrap_err()
                .window,
            BudgetWindow::Task
        );

        let mission_cap = BudgetCeilings {
            mission_usd: Some(3.0),
            ..Default::default()
        };
        assert_eq!(
            check_in(&ledger, &mission_cap, &intent, 10_001_000)
                .unwrap_err()
                .window,
            BudgetWindow::Mission
        );

        let vendor_cap = BudgetCeilings {
            vendor_daily_usd: BTreeMap::from([("OPENAI".to_string(), 3.0)]),
            ..Default::default()
        };
        assert_eq!(
            check_in(&ledger, &vendor_cap, &intent, 10_001_000)
                .unwrap_err()
                .window,
            BudgetWindow::VendorDaily
        );
    }

    #[test]
    fn check_in_allows_spend_within_configured_bounds() {
        let ledger = UsageLedger::default();
        let ceilings = BudgetCeilings {
            hourly_usd: Some(10.0),
            daily_usd: Some(50.0),
            mission_usd: Some(20.0),
            task_usd: Some(5.0),
            vendor_daily_usd: BTreeMap::from([("deepseek".to_string(), 15.0)]),
        };
        let intent = SpendIntent {
            provider: "deepseek-chat",
            expected_usd: 0.25,
            mission: "m-ok",
        };
        assert!(check_in(&ledger, &ceilings, &intent, 1_000_000).is_ok());
    }

    #[test]
    fn check_in_enforces_hourly_and_daily_caps_and_check_passes_when_uncapped() {
        let mut ledger = UsageLedger::default();
        ledger.record(UsageOutcome {
            provider: "claude-sonnet".to_string(),
            task_class: "chat".to_string(),
            usage: crate::usage_accounting::TokenUsage::default(),
            success: true,
            latency_ms: 80,
            unix_ms: 50_000_000,
            agent: "test".to_string(),
            mission: String::new(),
            usd: 4.0,
        });
        let intent = SpendIntent {
            provider: "claude-sonnet",
            expected_usd: 2.0,
            mission: "",
        };

        let hourly_cap = BudgetCeilings {
            hourly_usd: Some(5.0),
            ..Default::default()
        };
        assert_eq!(
            check_in(&ledger, &hourly_cap, &intent, 50_001_000)
                .unwrap_err()
                .window,
            BudgetWindow::Hourly
        );

        let daily_cap = BudgetCeilings {
            daily_usd: Some(5.0),
            ..Default::default()
        };
        assert_eq!(
            check_in(&ledger, &daily_cap, &intent, 60_000_000)
                .unwrap_err()
                .window,
            BudgetWindow::Daily
        );

        let _env = crate::engines::env_test_lock();
        #[allow(unsafe_code)]
        // SAFETY: test-only env mutation serialized by `crate::engines::env_test_lock`.
        unsafe {
            std::env::remove_var("SUSI_BUDGET_CEILINGS_FILE");
        }
        assert!(check(&intent).is_ok());
    }

    #[test]
    fn record_attaches_model_call_cost_to_active_mission_trace() {
        let _ = crate::susi_core::mission_trace::drain_active_model_calls();
        record(&SpendWrite {
            provider: "openrouter/qwen3-coder",
            class: crate::engines::brain::TaskClass::Code,
            ok: true,
            latency_ms: 120,
            usd: 0.00125,
            mission: "m-vc-202-013",
        });
        let trace = crate::susi_core::mission_trace::MissionTrace::new(
            "m-vc-202-013",
            "verify spend attribution",
            "COMPLETE",
            "swarm",
        );
        assert_eq!(trace.model_calls.len(), 1);
        assert_eq!(trace.model_calls[0].model, "openrouter/qwen3-coder|code");
        assert_eq!(trace.total_cost_micros(), 1_250);
    }

    #[test]
    fn verify_ceilings_refusal_and_intent_invariants_reject_corrupt_states() {
        let valid_ceilings = BudgetCeilings {
            hourly_usd: Some(5.0),
            daily_usd: Some(25.0),
            mission_usd: Some(10.0),
            task_usd: Some(1.0),
            vendor_daily_usd: BTreeMap::from([("openai".to_string(), 15.0)]),
        };
        assert!(valid_ceilings.verify_ceilings_invariants().is_ok());
        assert!(BudgetCeilings::default()
            .verify_ceilings_invariants()
            .is_ok());

        let mut neg_hourly = valid_ceilings.clone();
        neg_hourly.hourly_usd = Some(-0.1);
        assert!(neg_hourly.verify_ceilings_invariants().is_err());

        let mut nan_task = valid_ceilings.clone();
        nan_task.task_usd = Some(f64::NAN);
        assert!(nan_task.verify_ceilings_invariants().is_err());

        let mut inverted_windows = valid_ceilings.clone();
        inverted_windows.hourly_usd = Some(30.0);
        inverted_windows.daily_usd = Some(25.0);
        assert!(inverted_windows.verify_ceilings_invariants().is_err());

        let mut empty_vendor = valid_ceilings.clone();
        empty_vendor.vendor_daily_usd.insert("   ".to_string(), 5.0);
        assert!(empty_vendor.verify_ceilings_invariants().is_err());

        let mut neg_vendor_cap = valid_ceilings.clone();
        neg_vendor_cap
            .vendor_daily_usd
            .insert("deepseek".to_string(), -1.0);
        assert!(neg_vendor_cap.verify_ceilings_invariants().is_err());

        let valid_refusal = BudgetRefusal {
            window: BudgetWindow::Hourly,
            cap_usd: 5.0,
            spent_usd: 4.5,
            projected_usd: 5.5,
        };
        assert!(valid_refusal.verify_refusal_invariants().is_ok());

        let mut neg_cap = valid_refusal.clone();
        neg_cap.cap_usd = -1.0;
        assert!(neg_cap.verify_refusal_invariants().is_err());

        let mut neg_spent = valid_refusal.clone();
        neg_spent.spent_usd = -0.1;
        assert!(neg_spent.verify_refusal_invariants().is_err());

        let mut projected_below_spent = valid_refusal.clone();
        projected_below_spent.spent_usd = 6.0;
        projected_below_spent.projected_usd = 5.5;
        assert!(projected_below_spent.verify_refusal_invariants().is_err());

        let mut projected_within_cap = valid_refusal.clone();
        projected_within_cap.projected_usd = 5.0;
        assert!(projected_within_cap.verify_refusal_invariants().is_err());

        let valid_intent = SpendIntent {
            provider: "openai-gpt-4o",
            expected_usd: 0.25,
            mission: "m-1",
        };
        assert!(valid_intent.verify_intent_invariants().is_ok());

        let blank_provider = SpendIntent {
            provider: "  ",
            expected_usd: 0.25,
            mission: "m-1",
        };
        assert!(blank_provider.verify_intent_invariants().is_err());

        let neg_expected = SpendIntent {
            provider: "openai-gpt-4o",
            expected_usd: -0.01,
            mission: "m-1",
        };
        assert!(neg_expected.verify_intent_invariants().is_err());

        let inf_expected = SpendIntent {
            provider: "openai-gpt-4o",
            expected_usd: f64::INFINITY,
            mission: "m-1",
        };
        assert!(inf_expected.verify_intent_invariants().is_err());
    }
}
