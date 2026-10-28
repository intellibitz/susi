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
//! `SUSI_HARNESS_BUDGET_<PROVIDER>_USD` is the total credited to the provider,
//! and what the ledger shows spent there is subtracted from it. After a
//! top-up, raise it by the amount added. (It is not the balance the console
//! shows: that figure already has the spend taken off, and subtracting the
//! ledger's spend again turned a topped-up provider off.)
//!
//! With no balance known, the provider is active while it answers its models
//! endpoint with the key. A model the endpoint no longer lists is left off on
//! its own ([`ProviderBudget::absent`]); the provider's other models keep
//! running.
//!
//! Credit can be time-bound. No provider's balance API says when it expires
//! (DeepSeek splits granted from topped-up credit, Moonshot vouchers from
//! cash, neither with a date), so the day is configured:
//! `SUSI_HARNESS_CREDIT_EXPIRES_<PROVIDER>=YYYY-MM-DD`. An active provider
//! with dated credit is spent first, and a run alerts in the last
//! [`EXPIRY_ALERT_DAYS`] days.
//!
//! Keys are read through [`crate::secrets`] and sent only in an
//! `authorization` header. Nothing here prints or stores a key.

use crate::money::MicroUsd;
use crate::providers::{ProviderRecord, Registry};
use crate::secrets;
use crate::verify::check_provider;
use crate::verify::Fetch;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

/// A provider is active while it holds at least this much.
pub const ACTIVE_FLOOR: MicroUsd = MicroUsd::from_micros(1_000_000);

/// The `<PROVIDER>` part of a per-provider variable name for the registry id
/// `id`: upper case, with every character that is not a letter or a digit
/// written as `_`.
///
/// A variable name may only hold `A-Z`, `0-9` and `_` ([`crate::secrets`]
/// refuses any other), so an overlay provider `together-ai` is configured as
/// `SUSI_HARNESS_BUDGET_TOGETHER_AI_USD`. The budget used to be read as
/// `..._TOGETHER-AI_USD`, a name no scope can hold, while `susi-harness env`
/// advertised the underscore form, so the setting was silently ignored.
#[must_use]
pub fn env_suffix(id: &str) -> String {
    id.chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

/// The variable holding the total credited to provider `id`.
#[must_use]
pub fn budget_variable(id: &str) -> String {
    format!("SUSI_HARNESS_BUDGET_{}_USD", env_suffix(id))
}

/// The variable holding the day provider `id`'s credit expires.
#[must_use]
pub fn expiry_variable(id: &str) -> String {
    format!("SUSI_HARNESS_CREDIT_EXPIRES_{}", env_suffix(id))
}

/// Where a remaining balance came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The provider's own balance endpoint.
    Api,
    /// `SUSI_HARNESS_BUDGET_<PROVIDER>_USD` (the total credited), less the
    /// ledger's spend.
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
    /// Whether the provider answered its models endpoint with the key (or
    /// has none the harness can derive, in which case the calls decide).
    /// Consulted only when no balance is known: a provider that works is
    /// active even though its spend cannot be costed.
    pub working: bool,
    /// Registry model ids whose backend the provider's model list, read with
    /// the key, no longer advertises: retired, renamed, or not open to this
    /// key. Each is left off the fleet on its own; one drifted id used to
    /// mark the whole provider inactive and take every model of it off.
    pub absent: Vec<String>,
    /// The day the provider's credit expires, in days since the Unix epoch,
    /// from `SUSI_HARNESS_CREDIT_EXPIRES_<PROVIDER>`. No provider API
    /// publishes it, so it is configured.
    pub expires_day: Option<u64>,
    /// Whether the provider refused the key (HTTP 401 or 403) when it was
    /// checked: the key is wrong, revoked or a placeholder, and no top-up
    /// will help.
    pub key_refused: bool,
}

impl ProviderBudget {
    /// Whether the provider's models may run. A known balance decides by the
    /// one-dollar floor. With no costing, a provider that works is active.
    #[must_use]
    pub fn active(&self) -> bool {
        match self.remaining {
            Some(left) => left >= ACTIVE_FLOOR,
            None => self.working,
        }
    }

    /// The expiry day when the provider's credit should be spent first: it
    /// is active and its credit has not yet expired on `today`. Credit that
    /// expires is lost when unused, so spending it costs nothing that would
    /// otherwise be kept.
    #[must_use]
    pub fn use_first(&self, today: u64) -> Option<u64> {
        self.expires_day
            .filter(|day| *day >= today && self.active())
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

    /// Whether `model` (a registry id or alias) is one its provider's model
    /// list no longer advertises ([`ProviderBudget::absent`]).
    #[must_use]
    pub fn absent(&self, model: &str) -> bool {
        let id = crate::policy::canonical(model);
        self.providers
            .iter()
            .any(|entry| entry.absent.contains(&id))
    }

    /// The alert for models their provider no longer lists, or `None` when
    /// every listed provider advertises all of its models.
    #[must_use]
    pub fn absent_alert(&self) -> Option<String> {
        let parts = self
            .providers
            .iter()
            .filter(|entry| !entry.absent.is_empty())
            .map(|entry| format!("{}: {}", entry.provider, entry.absent.join(", ")))
            .collect::<Vec<_>>();
        (!parts.is_empty()).then(|| {
            format!(
                "not listed by their provider, left off: {}; the provider's other models still run",
                parts.join(" | ")
            )
        })
    }

    /// The expiry day of `provider`'s credit when it should be spent first
    /// today. See [`ProviderBudget::use_first`].
    #[must_use]
    pub fn use_first(&self, provider: &str) -> Option<u64> {
        let today = today();
        self.providers
            .iter()
            .find(|entry| entry.provider == provider)
            .and_then(|entry| entry.use_first(today))
    }

    /// The expiry alert: active credit that expires within
    /// [`EXPIRY_ALERT_DAYS`] of `today`, or `None` when there is none.
    #[must_use]
    pub fn expiry_alert(&self, today: u64) -> Option<String> {
        let parts = self
            .providers
            .iter()
            .filter_map(|entry| {
                let day = entry.use_first(today)?;
                let left = day - today;
                (left <= EXPIRY_ALERT_DAYS).then(|| {
                    let amount = entry.remaining.map_or_else(
                        || "balance unknown".to_string(),
                        |value| format!("{value} left"),
                    );
                    format!(
                        "{}: {amount}, expires {} ({left} day(s)); its models run first",
                        entry.provider,
                        format_day(day)
                    )
                })
            })
            .collect::<Vec<_>>();
        (!parts.is_empty()).then(|| format!("credit expiring: {}", parts.join(" | ")))
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
            let _ = write!(line, " {} {amount} {state}", entry.provider);
            if let Some(day) = entry.expires_day {
                let _ = write!(line, " expires {}", format_day(day));
            }
            line.push_str(" ·");
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
        let key_names = |id: &str| {
            registry
                .providers()
                .iter()
                .find(|provider| provider.id == id)
                .map(|provider| provider.key_envs.join(" or "))
                .unwrap_or_default()
        };
        let refused = needed
            .iter()
            .filter(|entry| entry.key_refused)
            .map(|entry| {
                let models = models_of(registry, &entry.provider).join(", ");
                format!(
                    "{} (the provider refused the key; check or replace {}; models off: {models})",
                    entry.provider,
                    key_names(&entry.provider)
                )
            })
            .collect::<Vec<_>>();
        // A provider that does not answer, with no balance known, is not a
        // budget problem: telling the operator to top it up sent them looking
        // in the wrong place. It is named for what it is.
        let silent = needed
            .iter()
            .filter(|entry| !entry.key_refused && entry.remaining.is_none())
            .map(|entry| {
                let models = models_of(registry, &entry.provider).join(", ");
                format!(
                    "{} ({}; models off: {models}; they return when the provider answers)",
                    entry.provider, entry.note
                )
            })
            .collect::<Vec<_>>();
        let parts = needed
            .iter()
            .filter(|entry| !entry.key_refused && entry.remaining.is_some())
            .map(|entry| {
                let models = models_of(registry, &entry.provider).join(", ");
                // The variable is the total credited, and the ledger's spend
                // is taken off it. Setting it to what the console shows left
                // took that spend off twice and kept a topped-up provider off.
                let hint = format!(
                    "top up, then raise {} by the amount added (it is the total credited; the ledger's spend is subtracted)",
                    budget_variable(&entry.provider)
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
        let mut alerts = Vec::new();
        if !refused.is_empty() {
            alerts.push(format!("key refused: {}", refused.join(" | ")));
        }
        if !silent.is_empty() {
            alerts.push(format!("not answering: {}", silent.join(" | ")));
        }
        if !parts.is_empty() {
            alerts.push(format!("topup needed: {}", parts.join(" | ")));
        }
        Some(alerts.join(" · "))
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
        let mut entry = budget_for(&provider.id, &provider.key_envs, fetch, spent);
        if entry.remaining.is_none() {
            let check = works(provider, fetch);
            entry.working = check.working;
            entry.key_refused = check.refused;
            entry.absent = check.absent;
            if !check.working || !entry.absent.is_empty() {
                entry.note = format!("{}; {}", entry.note, check.note);
            }
        }
        let name = expiry_variable(&provider.id);
        if let Some(text) = secrets::setting(&name) {
            apply_expiry(&mut entry, &name, &text, today());
        }
        providers.push(entry);
    }
    Budgets { providers }
}

/// Days before expiry from which the run alerts on unspent credit.
pub const EXPIRY_ALERT_DAYS: u64 = 7;

/// Record the credit expiry `text` (from the variable `name`) on `entry`.
///
/// A configured budget whose credit has expired is spent: nothing is left. A
/// balance a provider reports is its own answer and is left as it is. A value
/// that is not a `YYYY-MM-DD` date is named in the note and ignored.
pub fn apply_expiry(entry: &mut ProviderBudget, name: &str, text: &str, today: u64) {
    let Some(day) = parse_day(text.trim()) else {
        let _ = write!(entry.note, "; {name} is not a YYYY-MM-DD date");
        return;
    };
    entry.expires_day = Some(day);
    if day < today && entry.source == Source::Configured {
        entry.remaining = Some(MicroUsd::ZERO);
        let _ = write!(entry.note, "; credit expired {}", format_day(day));
    }
}

/// Today, in days since the Unix epoch (UTC).
#[must_use]
pub fn today() -> u64 {
    crate::lifecycle::now_unix() / 86_400
}

/// A `YYYY-MM-DD` date as days since the Unix epoch, or `None` when it is not
/// a real date on or after 1970-01-01.
#[must_use]
pub fn parse_day(text: &str) -> Option<u64> {
    let mut parts = text.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: u32 = parts.next()?.parse().ok()?;
    let day: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || day == 0 {
        return None;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let length = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day > length {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm), proleptic Gregorian.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let month_index = i64::from((month + 9) % 12);
    let day_of_year = (153 * month_index + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    u64::try_from(era * 146_097 + day_of_era - 719_468).ok()
}

/// Days since the Unix epoch as `YYYY-MM-DD`.
#[must_use]
pub fn format_day(days: u64) -> String {
    let z = i64::try_from(days).unwrap_or(i64::MAX - 719_468) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
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
                working: false,
                absent: Vec::new(),
                expires_day: None,
                key_refused: false,
                note: "balance from the provider".to_string(),
            },
            Err(reason) => ProviderBudget {
                provider: id.to_string(),
                remaining: None,
                source: Source::Unknown,
                working: false,
                absent: Vec::new(),
                expires_day: None,
                key_refused: false,
                note: reason,
            },
        };
    }
    let name = budget_variable(id);
    let Some(configured) =
        secrets::setting(&name).and_then(|text| MicroUsd::parse_usd(text.trim()))
    else {
        return ProviderBudget {
            provider: id.to_string(),
            remaining: None,
            source: Source::Unknown,
            working: false,
            absent: Vec::new(),
            expires_day: None,
            key_refused: false,
            note: format!("no balance API; {name} is not set"),
        };
    };
    configured_budget(id, configured, spent)
}

/// A configured budget less what the ledger shows spent on the provider. A
/// budget already spent is zero, never negative.
///
/// `configured` is the total credited to the provider
/// (`SUSI_HARNESS_BUDGET_<PROVIDER>_USD`), not what is left: the ledger's
/// spend is what turns the one into the other.
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
        working: false,
        absent: Vec::new(),
        expires_day: None,
        key_refused: false,
        note: format!("credited {configured}, spent {used}"),
    }
}

/// Reads one provider's balance document into micro-dollars.
type BalanceParser = fn(&str) -> Result<MicroUsd, String>;

/// What a provider's models endpoint says, read with the key.
struct Check {
    /// The provider answers, or the harness cannot ask it (the calls decide).
    working: bool,
    /// The provider refused the key (HTTP 401 or 403).
    refused: bool,
    /// Registry model ids the provider's list does not advertise.
    absent: Vec<String>,
    /// The check's own account of itself.
    note: String,
}

/// Whether the provider answers its models endpoint with the key: the live
/// proof that its models work, used when no balance can be costed.
///
/// A 2xx is an answer, whatever the list holds: a backend the list leaves
/// out is that model's problem, not the provider's, and is named in
/// [`Check::absent`]. A provider whose models endpoint
/// cannot be derived from its chat endpoint is not refused for it either:
/// nothing was asked, so a real call is the first evidence there is.
fn works(provider: &ProviderRecord, fetch: &Fetch<'_>) -> Check {
    let report = check_provider(provider, fetch);
    let refused = report.note.contains("HTTP 401") || report.note.contains("HTTP 403");
    let not_asked = report.ok && !report.reachable;
    let absent = provider
        .models
        .iter()
        .filter(|model| report.missing.contains(&model.backend))
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    let note = if absent.is_empty() {
        report.note
    } else {
        format!("{}: {}", report.note, absent.join(", "))
    };
    Check {
        working: report.reachable || not_asked,
        refused,
        absent,
        note,
    }
}

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
    // Read as the ledger's other readers do: a torn row costs only itself. A
    // `read_to_string` here turned one invalid UTF-8 byte into "nothing spent"
    // for every provider, and a provider's budget into its full amount again.
    let (rows, _) = crate::ledger::read_rows_lenient::<Value>(ledger);
    for entry in rows {
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
            working: false,
            absent: Vec::new(),
            expires_day: None,
            key_refused: false,
        }
    }

    /// A one-provider registry whose key lives in `key_env`, a variable name
    /// no other test reads, so setting it cannot leak into another test.
    fn keyed_registry(id: &str, key_env: &str) -> Registry {
        Registry::parse(&format!(
            r#"{{"providers":[{{"id":"{id}","wire":"openai",
                "endpoint":"https://{id}.example/v1/chat/completions",
                "key_env":["{key_env}"],
                "models":[
                  {{"id":"{id}-flash","backend":"flash-1","tools":true,"tier":"worker"}},
                  {{"id":"{id}-pro","backend":"pro-1","tools":true,"tier":"rescue"}}
                ]}}]}}"#
        ))
        .unwrap()
    }

    /// The observed failure: Google retired one backend, its list still
    /// answered 200 without it, and every Gemini model went off the fleet.
    #[test]
    fn a_provider_that_answers_without_one_model_stays_active_and_drops_only_that_one() {
        const KEY: &str = "SUSI_HARNESS_TEST_KEY_F2_PARTIAL_LIST";
        let registry = keyed_registry("partial", KEY);
        std::env::set_var(KEY, "test-key");
        let fetch =
            |_: &str, _: &[(&str, &str)]| Ok((200, r#"{"data":[{"id":"flash-1"}]}"#.to_string()));
        let budgets = collect(&registry, &fetch, &BTreeMap::new());
        std::env::remove_var(KEY);
        assert!(budgets.is_active("partial"), "{budgets:?}");
        assert!(budgets.absent("partial-pro"));
        assert!(!budgets.absent("partial-flash"));
        assert!(
            budgets.topup_alert(&registry).is_none(),
            "the provider answered"
        );
        let alert = budgets.absent_alert().unwrap();
        assert!(alert.contains("partial: partial-pro"), "{alert}");
    }

    #[test]
    fn a_refused_key_or_a_silent_provider_is_still_inactive() {
        const KEY: &str = "SUSI_HARNESS_TEST_KEY_F2_REFUSED";
        let registry = keyed_registry("refusing", KEY);
        std::env::set_var(KEY, "test-key");
        let refused = |_: &str, _: &[(&str, &str)]| Ok((401, "bad key".to_string()));
        let silent = |_: &str, _: &[(&str, &str)]| Err("connection refused".to_string());
        let refused = collect(&registry, &refused, &BTreeMap::new());
        let silent = collect(&registry, &silent, &BTreeMap::new());
        std::env::remove_var(KEY);
        assert!(!refused.is_active("refusing"));
        assert!(refused.providers[0].key_refused);
        assert!(!silent.is_active("refusing"));
    }

    #[test]
    fn an_overlay_provider_id_with_a_dash_reads_its_budget_under_underscores() {
        const KEY: &str = "SUSI_HARNESS_TEST_KEY_F31_DASHED";
        assert_eq!(env_suffix("together-ai"), "TOGETHER_AI");
        assert_eq!(env_suffix("acme.v2"), "ACME_V2");
        let registry = keyed_registry("susi-test-f31", KEY);
        std::env::set_var(KEY, "test-key");
        std::env::set_var("SUSI_HARNESS_BUDGET_SUSI_TEST_F31_USD", "12.50");
        std::env::set_var("SUSI_HARNESS_CREDIT_EXPIRES_SUSI_TEST_F31", "2999-01-01");
        let unreachable = |_: &str, _: &[(&str, &str)]| Err("not asked".to_string());
        let budgets = collect(&registry, &unreachable, &BTreeMap::new());
        std::env::remove_var(KEY);
        std::env::remove_var("SUSI_HARNESS_BUDGET_SUSI_TEST_F31_USD");
        std::env::remove_var("SUSI_HARNESS_CREDIT_EXPIRES_SUSI_TEST_F31");
        let entry = &budgets.providers[0];
        assert_eq!(entry.source, Source::Configured, "{entry:?}");
        assert_eq!(entry.remaining, Some(usd("12.50")));
        assert_eq!(entry.expires_day, parse_day("2999-01-01"));
    }

    /// The alert used to say "set it to the remaining amount" while the code
    /// subtracted every dollar the ledger ever recorded from it: a user who
    /// topped up and followed the hint stayed off.
    #[test]
    fn following_the_topup_hint_after_a_top_up_brings_the_provider_back() {
        let spent = BTreeMap::from([("gemini".to_string(), usd("24.50"))]);
        let low = configured_budget("gemini", usd("25.00"), &spent);
        assert!(!low.active());
        let alert = Budgets {
            providers: vec![low],
        }
        .topup_alert(Registry::builtin())
        .unwrap();
        assert!(
            alert.contains("raise SUSI_HARNESS_BUDGET_GEMINI_USD by the amount added"),
            "{alert}"
        );
        // A $20 top-up, as the hint says: 25 + 20.
        let topped = configured_budget("gemini", usd("45.00"), &spent);
        assert_eq!(topped.remaining, Some(usd("20.50")));
        assert!(topped.active());
    }

    #[test]
    fn a_provider_is_active_at_one_dollar_and_not_below() {
        assert!(entry("a", Some("1.00")).active());
        assert!(entry("a", Some("18.19")).active());
        assert!(!entry("a", Some("0.99")).active());
        assert!(
            !entry("a", None).active(),
            "no costing and not working is inactive"
        );
        let mut working = entry("a", None);
        working.working = true;
        assert!(
            working.active(),
            "a working model is active without costing"
        );
    }

    #[test]
    fn dates_round_trip_through_days_since_the_epoch() {
        assert_eq!(parse_day("1970-01-01"), Some(0));
        assert_eq!(parse_day("2000-03-01"), Some(11_017));
        assert_eq!(format_day(11_017), "2000-03-01");
        let day = parse_day("2026-12-31").unwrap();
        assert_eq!(format_day(day), "2026-12-31");
        assert_eq!(
            parse_day("2028-02-29").map(format_day).as_deref(),
            Some("2028-02-29")
        );
        assert_eq!(parse_day("2026-02-29"), None, "not a leap year");
        assert_eq!(parse_day("2026-13-01"), None);
        assert_eq!(parse_day("31/12/2026"), None);
        assert_eq!(parse_day("1969-12-31"), None);
    }

    #[test]
    fn active_dated_credit_is_used_first_until_it_expires() {
        let today = parse_day("2026-10-28").unwrap();
        let mut anthropic = entry("anthropic", Some("40.00"));
        apply_expiry(&mut anthropic, "X", "2026-11-30", today);
        assert_eq!(anthropic.use_first(today), parse_day("2026-11-30"));
        // A thin balance is not worth reordering the ladder for.
        let mut thin = entry("anthropic", Some("0.50"));
        apply_expiry(&mut thin, "X", "2026-11-30", today);
        assert_eq!(thin.use_first(today), None);
        // Undated credit keeps the normal order.
        assert_eq!(entry("deepseek", Some("18.00")).use_first(today), None);
    }

    #[test]
    fn an_expired_configured_credit_is_spent() {
        let today = parse_day("2026-10-28").unwrap();
        let mut gemini = configured_budget("gemini", usd("25.00"), &BTreeMap::new());
        apply_expiry(&mut gemini, "X", "2026-10-01", today);
        assert_eq!(gemini.remaining, Some(MicroUsd::ZERO));
        assert!(!gemini.active());
        assert!(gemini.note.contains("credit expired 2026-10-01"));
    }

    #[test]
    fn a_malformed_expiry_is_named_and_ignored() {
        let mut gemini = entry("gemini", Some("5.00"));
        apply_expiry(&mut gemini, "SUSI_HARNESS_CREDIT_EXPIRES_GEMINI", "soon", 0);
        assert_eq!(gemini.expires_day, None);
        assert!(gemini.note.contains("SUSI_HARNESS_CREDIT_EXPIRES_GEMINI"));
    }

    #[test]
    fn credit_expiring_within_a_week_raises_an_alert_and_shows_in_the_report() {
        let today = parse_day("2026-10-28").unwrap();
        let mut soon = entry("anthropic", Some("40.00"));
        apply_expiry(&mut soon, "X", "2026-11-02", today);
        let mut later = entry("moonshot", Some("9.00"));
        apply_expiry(&mut later, "X", "2027-01-01", today);
        let budgets = Budgets {
            providers: vec![soon, later],
        };
        let alert = budgets.expiry_alert(today).unwrap();
        assert!(
            alert.contains("anthropic: $40.000000 left, expires 2026-11-02 (5 day(s))"),
            "{alert}"
        );
        assert!(!alert.contains("moonshot"), "{alert}");
        let line = budgets.report_line(MicroUsd::ZERO, Registry::builtin());
        assert!(
            line.contains("anthropic $40.000000 active expires 2026-11-02"),
            "{line}"
        );
        assert!(!line.contains('\n'));
    }

    /// Anthropic publishes no balance API: its expiring credit is known by
    /// its date alone, and the alert must say so in words.
    #[test]
    fn expiring_credit_with_no_balance_api_reads_as_balance_unknown() {
        let today = parse_day("2026-10-28").unwrap();
        let mut anthropic = entry("anthropic", None);
        anthropic.working = true;
        apply_expiry(&mut anthropic, "X", "2026-11-04", today);
        let budgets = Budgets {
            providers: vec![anthropic],
        };
        let alert = budgets.expiry_alert(today).unwrap();
        assert_eq!(
            alert,
            "credit expiring: anthropic: balance unknown, expires 2026-11-04 (7 day(s)); \
             its models run first"
        );
    }

    #[test]
    fn a_provider_that_does_not_answer_is_not_told_to_top_up() {
        let budgets = Budgets {
            providers: vec![entry("gemini", None), entry("deepseek", Some("0.50"))],
        };
        let alert = budgets.topup_alert(Registry::builtin()).unwrap();
        assert!(alert.contains("not answering: gemini"), "{alert}");
        assert!(alert.contains("topup needed: deepseek"), "{alert}");
        assert!(!alert.contains("topup needed: gemini"), "{alert}");
    }

    #[test]
    fn a_refused_key_says_so_instead_of_asking_for_a_top_up() {
        let mut refused = entry("openai", None);
        refused.key_refused = true;
        let budgets = Budgets {
            providers: vec![refused, entry("gemini", None)],
        };
        let alert = budgets.topup_alert(Registry::builtin()).unwrap();
        assert!(alert.contains("key refused: openai"), "{alert}");
        assert!(alert.contains("check or replace OPENAI_API_KEY"), "{alert}");
        assert!(!alert.contains("SUSI_HARNESS_BUDGET_OPENAI_USD"), "{alert}");
        assert!(alert.contains("not answering: gemini"), "{alert}");
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
        // No balance known and not answering: named for what it is.
        assert!(alert.starts_with("not answering: gemini"), "{alert}");
        assert!(alert.contains("gemini-flash"), "{alert}");
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
