//! Provider budgets: what each keyed provider can still spend, and whether its
//! models may run.
//!
//! The rule is one dollar. A provider with at least [`ACTIVE_FLOOR`] left is
//! active, and its models may run. Below that, or when the remaining amount is
//! unknown, the provider is inactive: its models are left off the ladder and
//! the run raises a top-up alert. Any number of providers can carry keys; each
//! one is judged on its own.
//!
//! Where a provider publishes its balance, that answer is authoritative:
//! DeepSeek (`/user/balance`) and Moonshot (`/v1/users/me/balance`). The
//! others publish none that a key can read (Gemini bills through Google Cloud,
//! Anthropic through its console), so their budget is configured per provider:
//! `SUSI_HARNESS_BUDGET_<PROVIDER>_USD`, less what the ledger shows spent.
//!
//! Keys are read through [`crate::secrets`] and sent only in an
//! `authorization` header. Nothing here prints or stores a key.

use crate::money::MicroUsd;
use crate::providers::Registry;
use crate::secrets;
use crate::verify::Fetch;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// A provider is active while it holds at least this much.
pub const ACTIVE_FLOOR: MicroUsd = MicroUsd::from_micros(1_000_000);

/// Where a remaining balance came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The provider's own balance endpoint.
    Api,
    /// `SUSI_HARNESS_BUDGET_<PROVIDER>_USD`, less the ledger's spend.
    Configured,
    /// Neither source could answer. The provider is treated as inactive.
    Unknown,
}

/// One keyed provider's budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderBudget {
    /// Registry provider id.
    pub provider: String,
    /// What is left to spend, when known.
    pub remaining: Option<MicroUsd>,
    /// Where the figure came from.
    pub source: Source,
    /// Why the figure is missing or how it was obtained, for the alert.
    pub note: String,
}

impl ProviderBudget {
    /// Whether the provider's models may run.
    #[must_use]
    pub fn active(&self) -> bool {
        self.remaining.is_some_and(|left| left >= ACTIVE_FLOOR)
    }
}

/// Every keyed provider's budget, in registry order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct Budgets {
    /// One entry per keyed provider.
    pub providers: Vec<ProviderBudget>,
}

impl Budgets {
    /// Whether `provider` may run. A provider with no keyed entry is not
    /// active: its models cannot be budgeted.
    #[must_use]
    pub fn is_active(&self, provider: &str) -> bool {
        self.providers
            .iter()
            .find(|entry| entry.provider == provider)
            .is_some_and(ProviderBudget::active)
    }

    /// Whether `provider` carries a keyed budget at all. A keyless provider is
    /// local and is never gated.
    #[must_use]
    pub fn gated(&self, provider: &str) -> bool {
        self.providers
            .iter()
            .any(|entry| entry.provider == provider)
    }

    /// Keyed providers below the floor, which need a top-up.
    #[must_use]
    pub fn topup_needed(&self) -> Vec<&ProviderBudget> {
        self.providers
            .iter()
            .filter(|entry| !entry.active())
            .collect()
    }

    /// The per-run budget report: one line, no newline inside it.
    #[must_use]
    pub fn report_line(&self, run_spend: MicroUsd, registry: &Registry) -> String {
        let mut line = String::from("budget:");
        for entry in &self.providers {
            let state = if entry.active() { "active" } else { "INACTIVE" };
            let amount = entry
                .remaining
                .map_or_else(|| "unknown".to_string(), |left| left.to_string());
            let _ = write!(line, " {} {amount} {state} ·", entry.provider);
        }
        if self.providers.is_empty() {
            line.push_str(" no keyed providers ·");
        }
        let _ = write!(
            line,
            " run spend {run_spend} · models active {}",
            active_model_count(self, registry)
        );
        line
    }

    /// The top-up alert, or `None` when every keyed provider is active.
    #[must_use]
    pub fn topup_alert(&self, registry: &Registry) -> Option<String> {
        let needed = self.topup_needed();
        if needed.is_empty() {
            return None;
        }
        let parts = needed
            .iter()
            .map(|entry| {
                let models = models_of(registry, &entry.provider).join(", ");
                let hint = format!(
                    "set SUSI_HARNESS_BUDGET_{}_USD to the remaining amount",
                    entry.provider.to_ascii_uppercase()
                );
                let hint = match entry.source {
                    Source::Api => "top up the account".to_string(),
                    Source::Configured | Source::Unknown => hint,
                };
                format!(
                    "{} ({}; models off: {models}; {hint})",
                    entry.provider, entry.note
                )
            })
            .collect::<Vec<_>>();
        Some(format!("topup needed: {}", parts.join(" | ")))
    }
}

/// Collect every keyed provider's budget.
///
/// `fetch` is the HTTP GET used for the balance endpoints. `spent` is the
/// ledger's spend per provider, which only the configured budgets subtract.
#[must_use]
pub fn collect(
    registry: &Registry,
    fetch: &Fetch<'_>,
    spent: &BTreeMap<String, MicroUsd>,
) -> Budgets {
    let mut providers = Vec::new();
    for provider in registry.providers() {
        // A provider with no key variable is local and has no budget to check.
        if provider.key_envs.is_empty()
            || !provider.key_envs.iter().any(|name| secrets::is_set(name))
        {
            continue;
        }
        providers.push(budget_for(&provider.id, &provider.key_envs, fetch, spent));
    }
    Budgets { providers }
}

fn budget_for(
    id: &str,
    key_envs: &[String],
    fetch: &Fetch<'_>,
    spent: &BTreeMap<String, MicroUsd>,
) -> ProviderBudget {
    if let Some(answer) = balance_endpoint(id, key_envs, fetch) {
        return match answer {
            Ok(left) => ProviderBudget {
                provider: id.to_string(),
                remaining: Some(left),
                source: Source::Api,
                note: "balance from the provider".to_string(),
            },
            Err(reason) => ProviderBudget {
                provider: id.to_string(),
                remaining: None,
                source: Source::Unknown,
                note: reason,
            },
        };
    }
    let name = format!("SUSI_HARNESS_BUDGET_{}_USD", id.to_ascii_uppercase());
    let Some(configured) = std::env::var(&name)
        .ok()
        .and_then(|text| MicroUsd::parse_usd(text.trim()))
    else {
        return ProviderBudget {
            provider: id.to_string(),
            remaining: None,
            source: Source::Unknown,
            note: format!("no balance API; {name} is not set"),
        };
    };
    configured_budget(id, configured, spent)
}

/// A configured budget less what the ledger shows spent on the provider. A
/// budget already spent is zero, never negative.
#[must_use]
pub fn configured_budget(
    id: &str,
    configured: MicroUsd,
    spent: &BTreeMap<String, MicroUsd>,
) -> ProviderBudget {
    let used = spent.get(id).copied().unwrap_or(MicroUsd::ZERO);
    let left = configured.checked_sub(used).unwrap_or(MicroUsd::ZERO);
    ProviderBudget {
        provider: id.to_string(),
        remaining: Some(left),
        source: Source::Configured,
        note: format!("configured {configured}, spent {used}"),
    }
}

/// Reads one provider's balance document into micro-dollars.
type BalanceParser = fn(&str) -> Result<MicroUsd, String>;

/// The provider's own balance answer, for the providers that publish one.
/// `None` for a provider with no balance endpoint.
fn balance_endpoint(
    id: &str,
    key_envs: &[String],
    fetch: &Fetch<'_>,
) -> Option<Result<MicroUsd, String>> {
    let (url, parse): (&str, BalanceParser) = match id {
        "deepseek" => ("https://api.deepseek.com/user/balance", parse_deepseek),
        "moonshot" => (
            "https://api.moonshot.ai/v1/users/me/balance",
            parse_moonshot,
        ),
        _ => return None,
    };
    let key = key_envs.iter().find_map(|name| secrets::lookup(name))?;
    let auth = format!("Bearer {}", key.expose());
    let fetched = fetch(
        url,
        &[
            ("accept", "application/json"),
            ("authorization", auth.as_str()),
        ],
    );
    Some(match fetched {
        Ok((200, body)) => parse(&body),
        Ok((status, _)) => Err(format!("balance endpoint answered HTTP {status}")),
        Err(error) => Err(format!("balance endpoint unreachable: {error}")),
    })
}

/// DeepSeek: `balance_infos` lists one entry per currency; only USD is read.
///
/// # Errors
///
/// Returns the reason when the body is not the expected shape or holds no USD
/// balance. A non-USD balance is refused rather than converted.
pub fn parse_deepseek(body: &str) -> Result<MicroUsd, String> {
    let value: Value = serde_json::from_str(body).map_err(|e| format!("balance not JSON: {e}"))?;
    let infos = value
        .get("balance_infos")
        .and_then(Value::as_array)
        .ok_or_else(|| "balance_infos missing".to_string())?;
    let usd = infos
        .iter()
        .find(|info| info.get("currency").and_then(Value::as_str) == Some("USD"))
        .ok_or_else(|| "no USD balance (a non-USD balance is not converted)".to_string())?;
    let total = usd
        .get("total_balance")
        .and_then(Value::as_str)
        .ok_or_else(|| "total_balance missing".to_string())?;
    MicroUsd::parse_usd(total).ok_or_else(|| format!("total_balance `{total}` is not an amount"))
}

/// Moonshot: `data.available_balance` is the spendable amount in USD.
///
/// # Errors
///
/// Returns the reason when the body is not the expected shape.
pub fn parse_moonshot(body: &str) -> Result<MicroUsd, String> {
    let value: Value = serde_json::from_str(body).map_err(|e| format!("balance not JSON: {e}"))?;
    let available = value
        .get("data")
        .and_then(|data| data.get("available_balance"))
        .ok_or_else(|| "data.available_balance missing".to_string())?;
    let text = match available {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => {
            return Err("data.available_balance is not an amount".to_string());
        }
    };
    MicroUsd::parse_usd(&text).ok_or_else(|| format!("available_balance `{text}` is not an amount"))
}

/// Ledger spend per registry provider, for the configured budgets.
#[must_use]
pub fn spent_by_provider(ledger: &Path, registry: &Registry) -> BTreeMap<String, MicroUsd> {
    let mut spent: BTreeMap<String, MicroUsd> = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(ledger) else {
        return spent;
    };
    for entry in text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let Some(model) = entry.get("model").and_then(Value::as_str) else {
            continue;
        };
        let Some(cost) = entry.get("cost").and_then(Value::as_u64) else {
            continue;
        };
        let provider = registry
            .resolve(model)
            .map_or_else(|| "unknown".to_string(), |resolved| resolved.provider);
        let slot = spent.entry(provider).or_insert(MicroUsd::ZERO);
        *slot = slot.saturating_add(MicroUsd::from_micros(cost));
    }
    spent
}

fn models_of(registry: &Registry, provider: &str) -> Vec<String> {
    registry
        .providers()
        .iter()
        .filter(|record| record.id == provider)
        .flat_map(|record| record.models.iter().map(|model| model.id.clone()))
        .collect()
}

fn active_model_count(budgets: &Budgets, registry: &Registry) -> usize {
    registry
        .providers()
        .iter()
        .filter(|record| budgets.is_active(&record.id))
        .map(|record| record.models.iter().filter(|m| m.tier.is_some()).count())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usd(text: &str) -> MicroUsd {
        MicroUsd::parse_usd(text).unwrap()
    }

    fn entry(provider: &str, remaining: Option<&str>) -> ProviderBudget {
        ProviderBudget {
            provider: provider.to_string(),
            remaining: remaining.map(usd),
            source: if remaining.is_some() {
                Source::Api
            } else {
                Source::Unknown
            },
            note: "test".to_string(),
        }
    }

    #[test]
    fn a_provider_is_active_at_one_dollar_and_not_below() {
        assert!(entry("a", Some("1.00")).active());
        assert!(entry("a", Some("18.19")).active());
        assert!(!entry("a", Some("0.99")).active());
        assert!(!entry("a", None).active(), "an unknown balance is inactive");
    }

    #[test]
    fn deepseek_balance_is_read_in_usd_and_other_currencies_are_refused() {
        let usd_body =
            r#"{"is_available":true,"balance_infos":[{"currency":"USD","total_balance":"18.19"}]}"#;
        assert_eq!(parse_deepseek(usd_body).unwrap(), usd("18.19"));
        let cny = r#"{"balance_infos":[{"currency":"CNY","total_balance":"120.00"}]}"#;
        assert!(parse_deepseek(cny).unwrap_err().contains("not converted"));
        assert!(parse_deepseek("{}").is_err());
    }

    #[test]
    fn moonshot_available_balance_is_read_as_a_number() {
        let body = r#"{"code":0,"data":{"available_balance":24.9094,"cash_balance":19.95}}"#;
        assert_eq!(parse_moonshot(body).unwrap(), usd("24.9094"));
        assert!(parse_moonshot(r#"{"data":{}}"#).is_err());
    }

    #[test]
    fn topup_alert_names_inactive_providers_and_their_models() {
        let budgets = Budgets {
            providers: vec![entry("deepseek", Some("18.19")), entry("gemini", None)],
        };
        let registry = Registry::builtin();
        assert!(budgets.is_active("deepseek"));
        assert!(!budgets.is_active("gemini"));
        assert!(budgets.gated("gemini"));
        assert!(
            !budgets.gated("local"),
            "a provider with no budget is not gated"
        );
        let alert = budgets.topup_alert(registry).unwrap();
        assert!(alert.starts_with("topup needed: gemini"), "{alert}");
        assert!(alert.contains("gemini-flash"), "{alert}");
        assert!(alert.contains("SUSI_HARNESS_BUDGET_GEMINI_USD"), "{alert}");
        assert!(!alert.contains("deepseek"), "{alert}");
    }

    #[test]
    fn budget_report_is_one_line() {
        let budgets = Budgets {
            providers: vec![entry("deepseek", Some("18.19")), entry("gemini", None)],
        };
        let line = budgets.report_line(usd("0.000247"), Registry::builtin());
        assert!(!line.contains('\n'));
        assert!(
            line.starts_with("budget: deepseek $18.190000 active"),
            "{line}"
        );
        assert!(line.contains("gemini unknown INACTIVE"), "{line}");
        assert!(line.contains("run spend $0.000247"), "{line}");
    }

    #[test]
    fn no_alert_when_every_keyed_provider_is_active() {
        let budgets = Budgets {
            providers: vec![entry("deepseek", Some("18.19"))],
        };
        assert!(budgets.topup_alert(Registry::builtin()).is_none());
    }

    #[test]
    fn configured_budget_is_the_budget_less_the_ledger_spend() {
        let spent = BTreeMap::from([("gemini".to_string(), usd("1.60"))]);
        let left = configured_budget("gemini", usd("2.50"), &spent);
        assert_eq!(left.remaining, Some(usd("0.90")));
        assert!(!left.active(), "0.90 is under the one-dollar floor");
        let spent_past_budget = BTreeMap::from([("gemini".to_string(), usd("9.00"))]);
        let zero = configured_budget("gemini", usd("2.50"), &spent_past_budget);
        assert_eq!(zero.remaining, Some(MicroUsd::ZERO));
    }
}
