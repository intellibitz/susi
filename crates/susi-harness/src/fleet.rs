//! The fleet a run uses, resolved from the live environment.
//!
//! One place answers "which models, in what order, and who drives", for
//! every front end: the `susi harness` commands and the standalone
//! `susi-harness` binary call the same functions, so the harness behaves the
//! same whether or not the rest of susi builds.
//!
//! Every answer is read now: keys from [`crate::secrets`], balances and
//! credit expiry from [`crate::balance`], prices from the catalogue, health
//! and latency from the ledger. Nothing is cached between runs.

use crate::balance::{self, Budgets};
use crate::catalog::PriceCatalogue;
use crate::money::MicroUsd;
use crate::policy::UsageProfile;
use crate::providers::{Capability, ProviderGate, Registry};
use std::collections::BTreeMap;
use std::path::Path;
use susi_error::{EaiError, EaiResult};

/// The orchestrator and the worker ladder a run uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fleet {
    /// The model that plans and drives. `None` only when there is no model.
    pub orchestrator: Option<String>,
    /// Every model the run may use, in escalation order.
    pub ladder: Vec<String>,
    /// Models named in an explicit list but left off, each with the reason.
    pub left_off: Vec<String>,
}

/// The fleet for `spec`.
///
/// `auto` builds it from the API keys set in the environment
/// ([`Registry::auto_config_ranked`]): expiring credit first, then cache,
/// budget, capability and latency. Any other value is an explicit
/// comma-separated list of model ids whose first model drives; a model whose
/// provider is inactive is left off and named, so it cannot spend a dollar
/// it lacks. The catalogue is loaded only for `auto`.
///
/// # Errors
///
/// Returns `EaiError::config` when no model is usable, naming every key
/// variable the registry reads and what needs a top-up.
pub fn resolve_fleet(
    spec: &str,
    catalogue: impl FnOnce() -> EaiResult<PriceCatalogue>,
    budgets: &Budgets,
    latency: &BTreeMap<String, u64>,
) -> EaiResult<Fleet> {
    let registry = Registry::builtin();
    if spec.trim() != "auto" {
        let mut models = Vec::new();
        let mut left_off = Vec::new();
        for model in spec.split(',').map(str::trim).filter(|m| !m.is_empty()) {
            let provider = registry.resolve(model).map(|resolved| resolved.provider);
            match provider {
                Some(id) if budgets.gated(&id) && !budgets.is_active(&id) => {
                    left_off.push(format!(
                        "{model} left off: provider {id} is inactive (under $1 or unknown)"
                    ));
                }
                _ => models.push(model.to_string()),
            }
        }
        if models.is_empty() {
            return Err(EaiError::config(format!(
                "every model in the list is inactive: {}",
                topup_text(budgets)
            )));
        }
        return Ok(Fleet {
            orchestrator: models.first().cloned(),
            ladder: models,
            left_off,
        });
    }
    let catalogue = catalogue()?;
    let active = |provider: &str| !budgets.gated(provider) || budgets.is_active(provider);
    let auto = registry.auto_config_ranked(
        &crate::secrets::is_set,
        &catalogue,
        UsageProfile::agent(),
        &ProviderGate {
            active: &active,
            use_first: &|provider| budgets.use_first(provider),
            latency: &|model| latency.get(model).copied(),
        },
    );
    let ladder = auto.ladder();
    if ladder.is_empty() {
        return Err(EaiError::config(format!(
            "no tool-capable model with an active budget for the API keys set in the environment ({}): {}; set a key or add budget, or name the models",
            key_names(registry).join(", "),
            topup_text(budgets)
        )));
    }
    Ok(Fleet {
        orchestrator: auto.orchestrator,
        ladder,
        left_off: Vec::new(),
    })
}

/// The orchestrator a run uses: `model` as named, or the fleet's orchestrator
/// when `model` is `auto`.
///
/// # Errors
///
/// Returns `EaiError::config` for `auto` when the fleet has no orchestrator.
pub fn resolve_model(model: &str, fleet: &Fleet) -> EaiResult<String> {
    if model.trim() != "auto" {
        return Ok(model.to_string());
    }
    fleet
        .orchestrator
        .clone()
        .ok_or_else(|| EaiError::config("--model auto found no orchestrator in the fleet"))
}

/// Every key variable the registry reads, in registry order.
#[must_use]
pub fn key_names(registry: &Registry) -> Vec<&str> {
    registry
        .providers()
        .iter()
        .flat_map(|provider| provider.key_envs.iter().map(String::as_str))
        .collect()
}

/// Each model's measured mean provider-call latency, from the ledger. A
/// ledger that cannot be read has measured nothing.
#[must_use]
pub fn measured_latency(ledger: &Path) -> BTreeMap<String, u64> {
    let entries = crate::ledger::read_entries(ledger).unwrap_or_default();
    crate::policy::latencies(&entries)
}

/// Each keyed provider's budget, and the ledger's spend per provider, read
/// now. Balances come from the providers' own endpoints where they publish
/// one, and from `SUSI_HARNESS_BUDGET_<PROVIDER>_USD` otherwise.
#[must_use]
pub fn gather_budgets(ledger: &Path) -> (Budgets, BTreeMap<String, MicroUsd>) {
    let registry = Registry::builtin();
    let spent = balance::spent_by_provider(ledger, registry);
    let budgets = balance::collect(registry, &crate::verify::fetch_http, &spent);
    (budgets, spent)
}

/// Total spend across providers.
#[must_use]
pub fn total_spent(spent: &BTreeMap<String, MicroUsd>) -> MicroUsd {
    spent
        .values()
        .fold(MicroUsd::ZERO, |sum, value| sum.saturating_add(*value))
}

/// The top-up alert as text, or a plain statement when nothing needs one.
#[must_use]
pub fn topup_text(budgets: &Budgets) -> String {
    budgets
        .topup_alert(Registry::builtin())
        .unwrap_or_else(|| "no provider needs a top-up".to_string())
}

/// The alerts a run prints before it starts: providers that need a top-up
/// (`⚠`) and credit about to expire (`⏳`).
#[must_use]
pub fn alerts(budgets: &Budgets) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(alert) = budgets.topup_alert(Registry::builtin()) {
        lines.push(format!("⚠ {alert}"));
    }
    if let Some(alert) = budgets.expiry_alert(balance::today()) {
        lines.push(format!("⏳ {alert}"));
    }
    lines
}

/// One environment variable the harness reads.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Variable {
    /// The variable's name.
    pub name: String,
    /// What it does.
    pub purpose: String,
    /// Whether any scope (process, user, machine) sets it.
    pub set: bool,
    /// Its value, for a setting that is not a secret. Always `None` for a key.
    pub value: Option<String>,
}

/// Every variable the harness reads, and whether each is set: the provider
/// keys from the registry (reported as set or not, never shown), each keyed
/// provider's budget, credit expiry and rate caps, and the registry overlay.
#[must_use]
pub fn environment() -> Vec<Variable> {
    let registry = Registry::builtin();
    let mut variables = Vec::new();
    for provider in registry.providers() {
        for name in &provider.key_envs {
            variables.push(Variable {
                name: name.clone(),
                purpose: format!("API key for {}", provider.id),
                set: crate::secrets::is_set(name),
                value: None,
            });
        }
        if provider.key_envs.is_empty() {
            continue;
        }
        let upper = provider.id.to_ascii_uppercase().replace('-', "_");
        let settings = [
            (
                format!("SUSI_HARNESS_BUDGET_{upper}_USD"),
                format!(
                    "dollars left at {} when it publishes no balance",
                    provider.id
                ),
            ),
            (
                format!("SUSI_HARNESS_CREDIT_EXPIRES_{upper}"),
                format!(
                    "YYYY-MM-DD the {} credit expires; spent first until then",
                    provider.id
                ),
            ),
            (
                format!("SUSI_HARNESS_RPM_{upper}"),
                format!("requests per minute the gateway sends to {}", provider.id),
            ),
            (
                format!("SUSI_HARNESS_TPM_{upper}"),
                format!("tokens per minute the gateway lets {} serve", provider.id),
            ),
        ];
        for (name, purpose) in settings {
            let value = crate::secrets::setting(&name);
            variables.push(Variable {
                name,
                purpose,
                set: value.is_some(),
                value,
            });
        }
    }
    let overlay = crate::secrets::setting(crate::providers::OVERLAY_ENV);
    variables.push(Variable {
        name: crate::providers::OVERLAY_ENV.to_string(),
        purpose: "path of a registry file whose providers and models are added".to_string(),
        set: overlay.is_some(),
        value: overlay,
    });
    variables
}

/// The `models --available` report: what the keys in the environment make
/// usable, and the fleet `auto` would run, with the evidence it used.
#[must_use]
pub fn available_report(
    catalogue: Option<&PriceCatalogue>,
    budgets: &Budgets,
    latency: &BTreeMap<String, u64>,
) -> serde_json::Value {
    let registry = Registry::builtin();
    let capable =
        registry.available_capable_models(&[Capability::Tools], catalogue, UsageProfile::agent());
    let active = |provider: &str| !budgets.gated(provider) || budgets.is_active(provider);
    let auto = catalogue.map(|catalogue| {
        registry.auto_config_ranked(
            &crate::secrets::is_set,
            catalogue,
            UsageProfile::agent(),
            &ProviderGate {
                active: &active,
                use_first: &|provider| budgets.use_first(provider),
                latency: &|model| latency.get(model).copied(),
            },
        )
    });
    serde_json::json!({
        "orchestrator": Registry::primary_orchestrator(),
        "worker_ladder": Registry::default_worker_ladder(),
        "available_capable_models_sorted_by_cost": capable,
        "auto": auto.map(|auto| serde_json::json!({
            "orchestrator": auto.orchestrator,
            "workers": auto.workers,
            "use_first": auto.use_first,
            "latency_ms": latency,
            "rescue": auto.rescue,
            "unpriced": auto.unpriced,
            "ladder": auto.ladder(),
        })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balance::{ProviderBudget, Source};

    fn budgets(entries: &[(&str, Option<u64>)]) -> Budgets {
        Budgets {
            providers: entries
                .iter()
                .map(|(provider, micros)| ProviderBudget {
                    provider: (*provider).to_string(),
                    remaining: micros.map(MicroUsd::from_micros),
                    source: Source::Api,
                    note: "test".to_string(),
                    working: false,
                    expires_day: None,
                })
                .collect(),
        }
    }

    #[test]
    fn an_explicit_list_drives_with_its_first_model_and_drops_inactive_ones() {
        let gate = budgets(&[("deepseek", Some(5_000_000)), ("gemini", Some(10))]);
        let fleet = resolve_fleet(
            "gemini-flash, deepseek-flash,deepseek-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(fleet.orchestrator.as_deref(), Some("deepseek-flash"));
        assert_eq!(fleet.ladder, vec!["deepseek-flash", "deepseek-pro"]);
        assert_eq!(fleet.left_off.len(), 1);
        assert!(fleet.left_off[0].contains("gemini-flash"));
    }

    #[test]
    fn an_explicit_list_with_nothing_active_is_an_error() {
        let gate = budgets(&[("gemini", Some(10))]);
        let error =
            resolve_fleet("gemini-flash", || unreachable!(), &gate, &BTreeMap::new()).unwrap_err();
        assert!(error.to_string().contains("inactive"));
    }

    #[test]
    fn auto_model_takes_the_fleets_orchestrator() {
        let fleet = Fleet {
            orchestrator: Some("deepseek-pro".to_string()),
            ladder: vec!["deepseek-pro".to_string()],
            left_off: Vec::new(),
        };
        assert_eq!(resolve_model("auto", &fleet).unwrap(), "deepseek-pro");
        assert_eq!(resolve_model("claude-opus", &fleet).unwrap(), "claude-opus");
        assert!(resolve_model("auto", &Fleet::default()).is_err());
    }

    #[test]
    fn key_names_come_from_the_registry() {
        let names = key_names(Registry::builtin());
        for name in [
            "DEEPSEEK_API_KEY",
            "GEMINI_API_KEY",
            "MOONSHOT_API_KEY",
            "ANTHROPIC_API_KEY",
        ] {
            assert!(names.contains(&name), "{name} missing from {names:?}");
        }
    }

    #[test]
    fn the_environment_report_never_shows_a_key() {
        let variables = environment();
        let keys = key_names(Registry::builtin());
        for variable in &variables {
            if keys.contains(&variable.name.as_str()) {
                assert_eq!(variable.value, None, "{} must not be shown", variable.name);
            }
        }
        for name in [
            "SUSI_HARNESS_BUDGET_ANTHROPIC_USD",
            "SUSI_HARNESS_CREDIT_EXPIRES_ANTHROPIC",
            "SUSI_HARNESS_RPM_DEEPSEEK",
            "SUSI_HARNESS_TPM_GEMINI",
            crate::providers::OVERLAY_ENV,
        ] {
            assert!(variables.iter().any(|v| v.name == name), "{name} missing");
        }
    }

    #[test]
    fn the_builtin_catalogue_prices_the_ladder() {
        let catalogue = PriceCatalogue::builtin();
        assert!(!catalogue.is_empty());
        for model in Registry::default_worker_ladder() {
            assert!(catalogue.price(model).is_some(), "{model} unpriced");
        }
        assert!(
            PriceCatalogue::load_or_builtin(Path::new("no/such/prices.json"))
                .unwrap()
                .len()
                == catalogue.len()
        );
    }
}
