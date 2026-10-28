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
    evidence: &Evidence,
) -> EaiResult<Fleet> {
    let registry = Registry::builtin();
    if spec.trim() != "auto" {
        let mut models = Vec::new();
        let mut left_off = Vec::new();
        for model in spec.split(',').map(str::trim).filter(|m| !m.is_empty()) {
            let provider = registry.resolve(model).map(|resolved| resolved.provider);
            // The real calls decide first: a model whose last call failed is
            // inactive, whatever its provider's budget says.
            if evidence.failed.contains_key(model) {
                left_off.push(format!("{model} left off: its last real call failed"));
                continue;
            }
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
            latency: &|model| evidence.latency.get(model).copied(),
            live: &|model| !evidence.failed.contains_key(model),
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

/// How long a model whose last real call failed waits before a run probes it
/// again. A model that is down is not called on every run.
pub const PROBE_AFTER_MS: u64 = 5 * 60 * 1000;

/// What the real calls say about the models: the ledger's record, refreshed
/// by probes where it is stale.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Evidence {
    /// Each model's mean provider-call latency, in milliseconds.
    pub latency: BTreeMap<String, u64>,
    /// Models whose most recent real call failed, with when (ms since the
    /// epoch). They are inactive, whatever their budget or balance says.
    pub failed: BTreeMap<String, u64>,
    /// Models probed by this run, and whether each answered.
    pub probed: Vec<(String, bool)>,
}

/// The evidence a run decides on. The actual API calls are the source of
/// truth: a model whose last call failed is inactive. With `probe`, every such
/// model that failed at least [`PROBE_AFTER_MS`] ago and whose provider has a
/// key is called once now ([`probe`]), all at once, and the answer replaces the
/// old failure; a model that answers is active again.
#[must_use]
pub fn evidence(ledger: &Path, catalogue: Option<&PriceCatalogue>, probe_due: bool) -> Evidence {
    let entries = crate::ledger::read_entries(ledger).unwrap_or_default();
    let mut failed = crate::policy::failed_models(&entries);
    let mut probed = Vec::new();
    if probe_due {
        let now = now_ms();
        let registry = Registry::builtin();
        let keyed = |model: &str| {
            registry.resolve(model).is_some_and(|resolved| {
                resolved.key_envs.is_empty()
                    || resolved
                        .key_envs
                        .iter()
                        .any(|name| crate::secrets::is_set(name))
            })
        };
        let due = failed
            .iter()
            .filter(|(model, at)| now.saturating_sub(**at) >= PROBE_AFTER_MS && keyed(model))
            .map(|(model, _)| model.clone())
            .collect::<Vec<_>>();
        probed = std::thread::scope(|scope| {
            let handles = due
                .iter()
                .map(|model| scope.spawn(move || (model.clone(), probe(model, ledger, catalogue))))
                .collect::<Vec<_>>();
            handles
                .into_iter()
                .filter_map(|handle| handle.join().ok())
                .collect::<Vec<_>>()
        });
        for (model, answered) in &probed {
            if *answered {
                failed.remove(model);
            } else {
                failed.insert(model.clone(), now_ms());
            }
        }
    }
    Evidence {
        latency: crate::policy::latencies(&entries),
        failed,
        probed,
    }
}

/// One real call to `model`, the smallest that proves it answers: a one-line
/// prompt and a few output tokens, sent the way the gateway sends calls (the
/// same route, key and wire translation). It is priced and appended to
/// `ledger` like any other call, so its answer is evidence for the next run as
/// well. Returns whether the model answered.
pub fn probe(model: &str, ledger: &Path, catalogue: Option<&PriceCatalogue>) -> bool {
    use crate::ledger::{append_entry, Dispatch, FailureClass, LedgerEntry, Outcome};
    let started = now_ms();
    let provider = Registry::builtin()
        .resolve(model)
        .map_or_else(|| "unknown".to_string(), |resolved| resolved.provider);
    let price = catalogue
        .and_then(|catalogue| catalogue.price(model))
        .unwrap_or_default();
    let record = |usage: crate::pricing::TokenUsage, outcome: Outcome| {
        let entry = LedgerEntry::settle(Dispatch {
            mission: "probe".to_string(),
            task: "probe".to_string(),
            attempt: 1,
            model: model.to_string(),
            provider: provider.clone(),
            usage,
            price,
            started_unix_ms: started,
            ended_unix_ms: now_ms(),
            outcome,
        });
        let _ = append_entry(ledger, &entry);
    };
    let failed = |class: FailureClass| {
        record(
            crate::pricing::TokenUsage::default(),
            Outcome::ModelFailed { class },
        );
        false
    };
    let Ok(route) = crate::router::resolve_route(model, None, None) else {
        return failed(FailureClass::NoCredit);
    };
    let request = serde_json::json!({
        "model": route.model_id,
        "messages": [{"role": "user", "content": "Reply with the single word: ok"}],
        "max_tokens": 16,
    });
    let body = if route.is_anthropic {
        match susi_adapters_llm::inference_wire::openai_to_anthropic_messages(&request) {
            Ok(body) => body,
            Err(_) => return failed(FailureClass::Other),
        }
    } else {
        request
    };
    let headers = route
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&body).unwrap_or_default();
    let call = match susi_http_transport::http_call_with_body(
        "POST",
        &route.url,
        &headers,
        Some(&bytes),
        60,
        0,
    ) {
        Ok(call) => call,
        Err(error) => return failed(crate::gateway::transport_failure(&error)),
    };
    let status = call.status;
    let answer = call.into_bytes(1 << 20).unwrap_or_default();
    if !(200..300).contains(&status) {
        // A probe asks nothing unusual, so any refusal is the model failing.
        return failed(
            crate::gateway::provider_failure(status, &answer).unwrap_or(FailureClass::Other),
        );
    }
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&answer) else {
        return failed(FailureClass::MalformedOutput);
    };
    if route.is_anthropic {
        match susi_adapters_llm::inference_wire::anthropic_to_openai_chat(&value, model) {
            Ok(translated) => value = translated,
            Err(_) => return failed(FailureClass::MalformedOutput),
        }
    }
    match crate::usage::token_usage_from_json(&value) {
        Ok(usage) => {
            record(usage, Outcome::Completed);
            true
        }
        Err(_) => failed(FailureClass::MalformedOutput),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
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

/// The ledger a command uses when none is named: `SUSI_HARNESS_LEDGER` when
/// set, else `out/ledger.jsonl` under `cwd` when that file exists (a susi
/// workspace keeps its own), else `harness/ledger.jsonl` in the data home
/// ([`crate::secrets::data_home`]). The data home is the default so that what
/// the harness learns (spend, latency, failures) follows the user from one
/// directory to the next.
#[must_use]
pub fn default_ledger(cwd: &Path) -> std::path::PathBuf {
    if let Some(path) = crate::secrets::setting("SUSI_HARNESS_LEDGER") {
        return std::path::PathBuf::from(path);
    }
    let workspace = cwd.join("out").join("ledger.jsonl");
    if workspace.exists() {
        return workspace;
    }
    crate::secrets::data_home()
        .join("harness")
        .join("ledger.jsonl")
}

/// The keyed providers that have a key, and those that do not.
#[must_use]
pub fn providers_by_key() -> (
    Vec<&'static crate::providers::ProviderRecord>,
    Vec<&'static crate::providers::ProviderRecord>,
) {
    Registry::builtin()
        .providers()
        .iter()
        .filter(|provider| !provider.key_envs.is_empty())
        .partition(|provider| {
            provider
                .key_envs
                .iter()
                .any(|name| crate::secrets::is_set(name))
        })
}

/// What a first run with no key says: one key from any provider is enough,
/// where to put it, and where each provider hands one out.
#[must_use]
pub fn onboarding() -> String {
    let (_, missing) = providers_by_key();
    let mut text = String::from(
        "No API key found. Bring your own key from any one provider below; one is enough.\n\
Set it in the environment, or put NAME=key in a .env file here or in ",
    );
    text.push_str(
        &crate::secrets::data_home()
            .join(".env")
            .display()
            .to_string(),
    );
    text.push_str(", then run susi-harness again.\n\n");
    for provider in missing {
        let names = provider.key_envs.join(" or ");
        let url = provider.key_url.as_deref().unwrap_or("");
        text.push_str(&format!("  {:<12} {names:<38} {url}\n", provider.id));
    }
    text
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
    for (name, purpose) in [
        (
            "SUSI_HOME",
            "the data home: ledger and .env; default ~/.susi",
        ),
        (
            "SUSI_HARNESS_LEDGER",
            "the ledger file; default out/ledger.jsonl in a susi workspace, else <data home>/harness/ledger.jsonl",
        ),
    ] {
        let value = crate::secrets::setting(name);
        variables.push(Variable {
            name: name.to_string(),
            purpose: purpose.to_string(),
            set: value.is_some(),
            value,
        });
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
    evidence: &Evidence,
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
                latency: &|model| evidence.latency.get(model).copied(),
                live: &|model| !evidence.failed.contains_key(model),
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
            "latency_ms": &evidence.latency,
            "failed": auto.failed,
            "rescue": auto.rescue,
            "unpriced": auto.unpriced,
            "ladder": auto.ladder(),
        })),
        "probed": evidence
            .probed
            .iter()
            .map(|(model, answered)| serde_json::json!({"model": model, "answered": answered}))
            .collect::<Vec<_>>(),
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
                    key_refused: false,
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
            &Evidence::default(),
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
        let error = resolve_fleet(
            "gemini-flash",
            || unreachable!(),
            &gate,
            &Evidence::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("inactive"));
    }

    #[test]
    fn a_model_whose_last_call_failed_is_inactive_whatever_its_budget() {
        // DeepSeek has plenty of budget, but deepseek-flash's last real call
        // failed: the call is the truth.
        let gate = budgets(&[("deepseek", Some(50_000_000))]);
        let mut evidence = Evidence::default();
        evidence.failed.insert("deepseek-flash".to_string(), 1);
        let listed = resolve_fleet(
            "deepseek-flash,deepseek-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &evidence,
        )
        .unwrap();
        assert_eq!(listed.ladder, vec!["deepseek-pro"]);
        assert!(listed.left_off[0].contains("last real call failed"));
        let auto = Registry::builtin().auto_config_ranked(
            &|name| name == "DEEPSEEK_API_KEY",
            &PriceCatalogue::builtin(),
            UsageProfile::agent(),
            &ProviderGate {
                active: &|_| true,
                use_first: &|_| None,
                latency: &|_| None,
                live: &|model| !evidence.failed.contains_key(model),
            },
        );
        assert_eq!(auto.ladder(), vec!["deepseek-pro"]);
        assert_eq!(auto.failed, vec!["deepseek-flash"]);
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
