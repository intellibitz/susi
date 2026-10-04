//! Track real token usage per provider so cost tiers become measured.
//!
//! `Provider::generate` returns only `String`, so cost is today a coarse
//! tier table. This module carries vendor-reported token usage back into
//! the brain evidence: every routed call records a [`UsageOutcome`], the
//! ledger aggregates per provider + task class, and with an optional
//! price table the summary reports *measured* cost-per-success.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use susi_error::{EaiError, EaiResult};

/// Token usage as reported by the vendor (absent for engines that don't
/// report it).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

impl TokenUsage {
    #[must_use]
    pub fn total(self) -> u64 {
        self.prompt_tokens + self.completion_tokens
    }
}

/// One routed generation's measured outcome.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageOutcome {
    pub provider: String,
    /// Brain task class ("code", "chat", …); empty = unclassified.
    pub task_class: String,
    #[serde(default)]
    pub usage: TokenUsage,
    pub success: bool,
    #[serde(default)]
    pub latency_ms: u64,
    #[serde(default)]
    pub unix_ms: u64,
    /// Agent identity that spent this call (SUSI_AGENT; "standalone" when
    /// unset) — spend ceilings attribute every USD to an agent.
    #[serde(default)]
    pub agent: String,
    /// Mission id the call was spent on (EvidenceSession id; empty = no
    /// mission context reached the cascade).
    #[serde(default)]
    pub mission: String,
    /// Estimated USD this call cost — the expected task cost at dispatch
    /// time, recorded whether the call succeeded or not (a failed call is
    /// still billed by the vendor).
    #[serde(default)]
    pub usd: f64,
}

/// USD price per million tokens for one provider.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenPrice {
    pub input_per_million: f64,
    pub output_per_million: f64,
}

impl TokenPrice {
    #[must_use]
    pub fn cost(self, u: TokenUsage) -> f64 {
        (u.prompt_tokens as f64) * self.input_per_million / 1e6
            + (u.completion_tokens as f64) * self.output_per_million / 1e6
    }
}

/// Aggregate for one (provider, task-class) cell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    pub calls: u64,
    pub successes: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Mean tokens per successful call — always measurable.
    pub tokens_per_success: f64,
    /// Mean USD per successful call — `None` when no price is known.
    pub usd_per_success: Option<f64>,
}

/// Append-only evidence ledger; persisted as JSON next to the brain's
/// other evidence.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UsageLedger {
    records: Vec<UsageOutcome>,
}

impl UsageLedger {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&mut self, o: UsageOutcome) {
        self.records.push(o);
    }

    /// Every recorded outcome — spend ceilings scan these for window sums.
    #[must_use]
    pub fn records(&self) -> &[UsageOutcome] {
        &self.records
    }

    /// USD recorded since `since_unix_ms`, optionally scoped to one provider
    /// and/or one mission (the attribution axes a ceiling checks).
    #[must_use]
    pub fn spend_usd(
        &self,
        since_unix_ms: u64,
        provider: Option<&str>,
        mission: Option<&str>,
    ) -> f64 {
        self.records
            .iter()
            .filter(|r| r.unix_ms >= since_unix_ms)
            .filter(|r| provider.is_none_or(|p| r.provider == p))
            .filter(|r| mission.is_none_or(|m| r.mission == m))
            .map(|r| r.usd)
            .sum()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.records.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Aggregate one cell. `task_class` of `""` aggregates all classes.
    #[must_use]
    pub fn summary(
        &self,
        provider: &str,
        task_class: &str,
        price: Option<TokenPrice>,
    ) -> UsageSummary {
        let mut s = UsageSummary::default();
        let mut usd = 0.0;
        for r in &self.records {
            if r.provider != provider || (!task_class.is_empty() && r.task_class != task_class) {
                continue;
            }
            s.calls += 1;
            s.prompt_tokens += r.usage.prompt_tokens;
            s.completion_tokens += r.usage.completion_tokens;
            if r.success {
                s.successes += 1;
                if let Some(p) = price {
                    usd += p.cost(r.usage);
                }
            }
        }
        if s.successes > 0 {
            s.tokens_per_success =
                (s.prompt_tokens + s.completion_tokens) as f64 / s.successes as f64;
            if price.is_some() {
                s.usd_per_success = Some(usd / s.successes as f64);
            }
        }
        s
    }

    /// Measured cost-per-success for every provider, best first — the
    /// number `susi brain` shows next to the tier table.
    #[must_use]
    pub fn measured_cost_per_success(
        &self,
        prices: &BTreeMap<String, TokenPrice>,
    ) -> Vec<(String, f64)> {
        let mut providers: Vec<String> = self.records.iter().map(|r| r.provider.clone()).collect();
        providers.sort();
        providers.dedup();
        let mut out: Vec<(String, f64)> = providers
            .iter()
            .filter_map(|p| {
                let price = prices.get(p).copied();
                self.summary(p, "", price)
                    .usd_per_success
                    .map(|c| (p.clone(), c))
            })
            .collect();
        out.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    pub fn save(&self, path: &std::path::Path) -> EaiResult<()> {
        let text = serde_json::to_string_pretty(self).map_err(|e| EaiError::io(e.to_string()))?;
        std::fs::write(path, text).map_err(|e| EaiError::io(e.to_string()))
    }

    /// Load a ledger, tolerating an absent file (empty ledger).
    ///
    /// # Errors
    /// [`EaiError::io`] on unreadable/corrupt files.
    pub fn load(path: &std::path::Path) -> EaiResult<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| EaiError::io(e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::new()),
            Err(e) => Err(EaiError::io(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(provider: &str, class: &str, p: u64, c: u64, ok: bool) -> UsageOutcome {
        UsageOutcome {
            provider: provider.to_string(),
            task_class: class.to_string(),
            usage: TokenUsage {
                prompt_tokens: p,
                completion_tokens: c,
            },
            success: ok,
            latency_ms: 100,
            unix_ms: 0,
            agent: String::new(),
            mission: String::new(),
            usd: 0.0,
        }
    }

    #[test]
    fn usage_accounting_aggregates_per_provider_class() {
        let mut l = UsageLedger::new();
        l.record(rec("openai", "code", 100, 50, true));
        l.record(rec("openai", "code", 200, 50, true));
        l.record(rec("openai", "chat", 900, 100, true));
        l.record(rec("local", "code", 10, 5, true));
        let s = l.summary("openai", "code", None);
        assert_eq!(s.calls, 2);
        assert_eq!(s.prompt_tokens, 300);
        assert_eq!(s.tokens_per_success, 200.0);
        assert!(s.usd_per_success.is_none());
    }

    #[test]
    fn usage_accounting_measured_cost_per_success() {
        let mut l = UsageLedger::new();
        // openai: 2 successes costing (1000+1000)in + (500+500)out
        l.record(rec("openai", "", 1000, 500, true));
        l.record(rec("openai", "", 1000, 500, true));
        l.record(rec("local", "", 5000, 5000, true));
        let mut prices = BTreeMap::new();
        prices.insert(
            "openai".to_string(),
            TokenPrice {
                input_per_million: 2.0,
                output_per_million: 10.0,
            },
        );
        prices.insert(
            "local".to_string(),
            TokenPrice {
                input_per_million: 0.0,
                output_per_million: 0.0,
            },
        );
        let ranked = l.measured_cost_per_success(&prices);
        // openai per success: (1000*2 + 500*10)/1e6 = 0.007
        assert_eq!(ranked[0].0, "local");
        assert!((ranked[1].1 - 0.007).abs() < 1e-9);
    }

    #[test]
    fn usage_accounting_failures_count_calls_not_success() {
        let mut l = UsageLedger::new();
        l.record(rec("p", "", 100, 50, false));
        l.record(rec("p", "", 100, 50, true));
        let s = l.summary(
            "p",
            "",
            Some(TokenPrice {
                input_per_million: 1.0,
                output_per_million: 1.0,
            }),
        );
        assert_eq!(s.calls, 2);
        assert_eq!(s.successes, 1);
        // cost of only the successful call / 1 success
        assert!((s.usd_per_success.unwrap_or(0.0) - 0.00015).abs() < 1e-9);
    }

    #[test]
    fn usage_accounting_persists_roundtrip() {
        let dir = std::env::temp_dir().join(format!("usage-ledger-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("u.json");
        let mut l = UsageLedger::new();
        l.record(rec("p", "c", 1, 2, true));
        l.save(&path).unwrap();
        let loaded = UsageLedger::load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        // absent file -> empty ledger
        assert!(UsageLedger::load(&dir.join("nope.json"))
            .unwrap()
            .is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn usage_accounting_empty_summary_is_zeroed() {
        let l = UsageLedger::new();
        let s = l.summary("nobody", "code", None);
        assert_eq!(s.calls, 0);
        assert_eq!(s.tokens_per_success, 0.0);
    }
}
