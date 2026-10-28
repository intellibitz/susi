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
    /// The strong model that plans and drives. `None` only when there is no
    /// model.
    pub orchestrator: Option<String>,
    /// The light models that do the work: every task starts on one, and
    /// tasks are spread across them.
    pub workers: Vec<String>,
    /// The strong models that fix what a worker could not: a task whose
    /// worker failed, or produced work that did not pass, goes to them next.
    pub fixers: Vec<String>,
    /// Every model the run may use: the workers, then the fixers.
    pub ladder: Vec<String>,
    /// Models named in an explicit list but left off, each with the reason.
    pub left_off: Vec<String>,
}

impl Fleet {
    /// The models tasks are assigned to: the light workers, or the whole
    /// ladder when the fleet has no light model (one strong model alone still
    /// does the work).
    #[must_use]
    pub fn task_workers(&self) -> Vec<String> {
        if self.workers.is_empty() {
            self.ladder.clone()
        } else {
            self.workers.clone()
        }
    }
}

/// The fleet for `spec`.
///
/// `auto` builds it from the API keys set in the environment
/// ([`Registry::auto_config_ranked`]): expiring credit first, then cache,
/// budget, capability and latency, with a model the catalogue cannot price
/// last in its tier. Any other value is an explicit comma-separated list of
/// model ids whose first model drives; a model whose provider has no key set
/// or is inactive, whose last real call was refused, or that its provider no
/// longer lists, is left off and named, so it cannot spend a dollar it lacks
/// or burn a task's attempts on a call that cannot be sent. A model whose
/// last call failed transiently (a rate limit, a timeout, a 5xx) stays: one
/// such answer must not empty a fleet of one model. The catalogue is loaded
/// only for `auto`.
///
/// # Errors
///
/// Returns `EaiError::config` when no model is usable, naming every key
/// variable the registry reads, why each model is off and what needs a
/// top-up.
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
            let resolved = registry.resolve(model);
            // The real calls decide first: a model whose last call was
            // refused is inactive, whatever its provider's budget says. The
            // evidence is keyed by registry id, and the list may use an alias.
            if evidence
                .failed
                .contains_key(&crate::policy::canonical(model))
            {
                left_off.push(format!(
                    "{model} left off: its last real call was refused (key refused or no credit); a probe re-admits it once the refusal is {} minutes old",
                    PROBE_AFTER_MS / 60_000
                ));
                continue;
            }
            match resolved {
                // A keyed provider whose key is not set has no budget entry
                // ([`balance::collect`] skips it). Its model was kept, the
                // gateway answered every call 502 "none of ... are set"
                // without a ledger row, and the worker read the 502 as
                // transient and spent two of a task's attempts on it.
                Some(resolved)
                    if !resolved.key_envs.is_empty() && !budgets.gated(&resolved.provider) =>
                {
                    left_off.push(format!(
                        "{model} left off: none of {} is set",
                        resolved.key_envs.join(" / ")
                    ));
                }
                Some(resolved)
                    if budgets.gated(&resolved.provider)
                        && !budgets.is_active(&resolved.provider) =>
                {
                    left_off.push(format!(
                        "{model} left off: provider {} is inactive (under $1 or unknown)",
                        resolved.provider
                    ));
                }
                Some(resolved) if budgets.absent(model) => {
                    left_off.push(format!(
                        "{model} left off: provider {} no longer lists it",
                        resolved.provider
                    ));
                }
                _ => models.push(model.to_string()),
            }
        }
        if models.is_empty() {
            return Err(EaiError::config(format!(
                "every model in the list is inactive ({}): {}",
                left_off.join("; "),
                topup_text(budgets)
            )));
        }
        // Roles come from the registry: a rescue model is strong (a fixer,
        // and the orchestrator), anything else is a light worker.
        let strong =
            |model: &String| registry.tier_of(model) == Some(crate::providers::Tier::Rescue);
        let (fixers, workers): (Vec<String>, Vec<String>) =
            models.iter().cloned().partition(|model| strong(model));
        return Ok(Fleet {
            orchestrator: fixers.first().or_else(|| models.first()).cloned(),
            workers,
            fixers,
            ladder: models,
            left_off,
        });
    }
    let catalogue = catalogue()?;
    let auto = unpriced_last(auto_config(registry, &catalogue, budgets, evidence));
    let ladder = auto.ladder();
    if ladder.is_empty() {
        let refused = if auto.failed.is_empty() {
            String::new()
        } else {
            format!(
                "; left off (last real call refused, or no longer listed by the provider): {}",
                auto.failed.join(", ")
            )
        };
        return Err(EaiError::config(format!(
            "no tool-capable model with an active budget for the API keys set in the environment ({}): {}{refused}; set a key or add budget, or name the models",
            key_names(registry).join(", "),
            topup_text(budgets)
        )));
    }
    Ok(Fleet {
        orchestrator: auto.orchestrator,
        workers: auto.workers,
        fixers: auto.rescue,
        ladder,
        left_off: Vec::new(),
    })
}

/// The `auto` ranking for the keys set now, gated by `budgets` and by the
/// real calls: a model whose last call was refused, or that its provider no
/// longer lists, is left out (and named in [`crate::providers::AutoConfig`]'s
/// `failed`). A model whose last call failed transiently stays in.
fn auto_config(
    registry: &Registry,
    catalogue: &PriceCatalogue,
    budgets: &Budgets,
    evidence: &Evidence,
) -> crate::providers::AutoConfig {
    let active = |provider: &str| !budgets.gated(provider) || budgets.is_active(provider);
    registry.auto_config_ranked(
        &crate::secrets::is_set,
        catalogue,
        UsageProfile::agent(),
        &ProviderGate {
            active: &active,
            use_first: &|provider| budgets.use_first(provider),
            latency: &|model| evidence.latency.get(model).copied(),
            live: &|model| !evidence.failed.contains_key(model) && !budgets.absent(model),
        },
    )
}

/// `auto` with every model the catalogue cannot price moved to the end of
/// its tier, the rest in their order.
///
/// The ranking compares cache before price, and an unpriced model is ranked
/// at an infinite cost: one that declares a cache therefore sorted ahead of
/// every priced model without one, and became the model tasks start on,
/// although its spend is recorded at zero and no budget cap can see it. A
/// working model is still never dropped for lack of a price; it runs after
/// the priced ones. The orchestrator moves with it, unless it is on expiring
/// credit or is the primary.
fn unpriced_last(mut auto: crate::providers::AutoConfig) -> crate::providers::AutoConfig {
    if auto.unpriced.is_empty() {
        return auto;
    }
    let unpriced = auto.unpriced.clone();
    // A stable sort: within the priced and within the unpriced, the order holds.
    auto.workers.sort_by_key(|model| unpriced.contains(model));
    auto.rescue.sort_by_key(|model| unpriced.contains(model));
    let reselect = auto.orchestrator.as_ref().is_some_and(|model| {
        unpriced.contains(model)
            && !auto.use_first.contains(model)
            && model != Registry::primary_orchestrator()
    });
    if reselect {
        auto.orchestrator = auto
            .rescue
            .first()
            .or_else(|| auto.workers.first())
            .cloned();
    }
    auto
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
/// by probes where it is stale. Every map is keyed by registry id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Evidence {
    /// Each model's mean provider-call latency, in milliseconds.
    pub latency: BTreeMap<String, u64>,
    /// Models whose most recent real call failed definitively (the provider
    /// refused the key or has no credit for it), with when (ms since the
    /// epoch). They are inactive, whatever their budget or balance says,
    /// until a probe answers.
    pub failed: BTreeMap<String, u64>,
    /// Models whose most recent real call failed transiently (a rate limit, a
    /// timeout, a 5xx, a dropped connection) in the last
    /// [`crate::policy::COOLDOWN_MS`], with when. They stay in the fleet: a
    /// task tries them after the others ([`crate::policy::arrange`]).
    pub cooling: BTreeMap<String, u64>,
    /// Models probed by this run, and whether each answered.
    pub probed: Vec<(String, bool)>,
}

/// The evidence a run decides on. The actual API calls are the source of
/// truth: a model whose last call failed definitively is inactive. With
/// `probe_due`, every such model that failed at least [`PROBE_AFTER_MS`] ago
/// and that a call can reach ([`routable`]) is called once now ([`probe`]),
/// all at once, and the answer replaces the old failure; a model that answers
/// is active again.
#[must_use]
pub fn evidence(ledger: &Path, catalogue: Option<&PriceCatalogue>, probe_due: bool) -> Evidence {
    let entries = crate::ledger::read_entries_reporting(ledger);
    let now = now_ms();
    let failures = crate::policy::in_force(crate::policy::failed_models(&entries), now);
    let (mut failed, mut cooling): (BTreeMap<String, u64>, BTreeMap<String, u64>) =
        (BTreeMap::new(), BTreeMap::new());
    for (model, failure) in &failures {
        let side = if failure.definitive() {
            &mut failed
        } else {
            &mut cooling
        };
        side.insert(model.clone(), failure.at_unix_ms);
    }
    let mut probed = Vec::new();
    if probe_due {
        let due = due_for_probe(&failures, now, &|model| {
            routable(model, Registry::builtin(), &crate::secrets::is_set)
        });
        let answers = probe_all(&due, ledger, catalogue);
        for (model, answer) in &answers {
            failed.remove(model);
            cooling.remove(model);
            if let Err(class) = answer {
                let failure = crate::policy::ModelFailure {
                    at_unix_ms: now_ms(),
                    class: *class,
                };
                let side = if failure.definitive() {
                    &mut failed
                } else {
                    &mut cooling
                };
                side.insert(model.clone(), failure.at_unix_ms);
            }
        }
        probed = answers
            .into_iter()
            .map(|(model, answer)| (model, answer.is_ok()))
            .collect();
    }
    Evidence {
        latency: crate::policy::latencies(&entries),
        failed,
        cooling,
        probed,
    }
}

/// One line for each model [`evidence`] probed, saying what the answer means
/// for the run.
///
/// Only a refused probe keeps a model inactive ([`Evidence::failed`]). A probe
/// that failed transiently (a rate limit, a timeout, a 5xx, a 400 or 404 the
/// gateway does not class) leaves the model on the ladder behind the others
/// ([`Evidence::cooling`]). Both front ends printed "stays inactive" for every
/// failed probe, which named as off a model the next task still ran.
#[must_use]
pub fn probe_report(evidence: &Evidence) -> Vec<String> {
    evidence
        .probed
        .iter()
        .map(|(model, answered)| {
            if *answered {
                format!("probe: {model} answered; active again")
            } else if evidence.failed.contains_key(model) {
                format!("probe: {model} failed again; it stays inactive")
            } else {
                format!(
                    "probe: {model} failed transiently; it stays on the ladder, tried after the others"
                )
            }
        })
        .collect()
}

/// The models of `failures` a probe should call now: each definitive failure
/// at least [`PROBE_AFTER_MS`] old that a call can reach. A transient failure
/// is not probed; it cools down on its own ([`crate::policy::in_force`]).
#[must_use]
pub fn due_for_probe(
    failures: &BTreeMap<String, crate::policy::ModelFailure>,
    now_unix_ms: u64,
    routable: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    failures
        .iter()
        .filter(|(model, failure)| {
            failure.definitive()
                && now_unix_ms.saturating_sub(failure.at_unix_ms) >= PROBE_AFTER_MS
                && routable(model)
        })
        .map(|(model, _)| model.clone())
        .collect()
}

/// Whether a call to `model` can be sent: the gateway's route for it has the
/// key it needs. A registry model needs one of its provider's keys (a keyless
/// provider is local and needs none). A name the registry cannot resolve goes
/// to OpenAI ([`crate::router::resolve_route`]'s fallback), so it needs
/// `OPENAI_API_KEY`. Only a model without a route is never probed: probing it
/// could only record another refusal.
#[must_use]
pub fn routable(model: &str, registry: &Registry, key_set: &dyn Fn(&str) -> bool) -> bool {
    registry.resolve(model).map_or_else(
        || key_set("OPENAI_API_KEY"),
        |resolved| resolved.key_envs.is_empty() || resolved.key_envs.iter().any(|n| key_set(n)),
    )
}

/// Re-admit a refused model inside a running mission: probe each of
/// `candidates` whose last real call failed definitively at least
/// [`PROBE_AFTER_MS`] before `now_unix_ms`, as a run does when it starts. A
/// refused key or an empty account was otherwise off for the rest of the
/// mission even after it was fixed or topped up. Each probe appends its row
/// to `ledger`, so the caller re-reads it. Returns the models probed and
/// whether each answered.
pub fn readmit(
    entries: &[crate::ledger::LedgerEntry],
    ledger: &Path,
    catalogue: Option<&PriceCatalogue>,
    candidates: &[String],
    now_unix_ms: u64,
) -> Vec<(String, bool)> {
    let failures = crate::policy::failed_models(entries);
    let wanted = candidates
        .iter()
        .map(|model| crate::policy::canonical(model))
        .collect::<std::collections::BTreeSet<_>>();
    let failures = failures
        .into_iter()
        .filter(|(model, _)| wanted.contains(model))
        .collect::<BTreeMap<_, _>>();
    let due = due_for_probe(&failures, now_unix_ms, &|model| {
        routable(model, Registry::builtin(), &crate::secrets::is_set)
    });
    probe_all(&due, ledger, catalogue)
        .into_iter()
        .map(|(model, answer)| (model, answer.is_ok()))
        .collect()
}

/// Models a probe is calling now, in this process. Tasks prepared in
/// parallel each re-admit their own ladder; one model is probed once, not
/// once per task.
static PROBING: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Probe every model of `due` at once, skipping one another thread is
/// already probing. Returns each probed model and its answer.
fn probe_all(
    due: &[String],
    ledger: &Path,
    catalogue: Option<&PriceCatalogue>,
) -> Vec<(String, Result<(), crate::ledger::FailureClass>)> {
    let mine = due
        .iter()
        .filter(|model| {
            PROBING
                .lock()
                .map(|mut probing| probing.insert((*model).clone()))
                .unwrap_or(false)
        })
        .cloned()
        .collect::<Vec<_>>();
    let answers = std::thread::scope(|scope| {
        let handles = mine
            .iter()
            .map(|model| scope.spawn(move || (model.clone(), probe_call(model, ledger, catalogue))))
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect::<Vec<_>>()
    });
    if let Ok(mut probing) = PROBING.lock() {
        for model in &mine {
            probing.remove(model);
        }
    }
    answers
}

/// One real call to `model`, the smallest that proves it answers: a one-line
/// prompt and a few output tokens, sent the way the gateway sends calls (the
/// same route, key and wire translation). It is priced and appended to
/// `ledger` like any other call, so its answer is evidence for the next run as
/// well. Returns whether the model answered.
pub fn probe(model: &str, ledger: &Path, catalogue: Option<&PriceCatalogue>) -> bool {
    probe_call(model, ledger, catalogue).is_ok()
}

/// [`probe`], with how the call failed when it did.
fn probe_call(
    model: &str,
    ledger: &Path,
    catalogue: Option<&PriceCatalogue>,
) -> Result<(), crate::ledger::FailureClass> {
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
        Err(class)
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
        // A refusal the gateway does not class (a 400 or 404) is recorded as
        // `Other`, which is transient: it may be this probe's own request
        // shape the provider objects to, and a definitive class would keep a
        // model a task's call could use off the ladder for good.
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
            Ok(())
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
    let entries = crate::ledger::read_entries_reporting(ledger);
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
/// (`⚠`), models their provider no longer lists (`⚠`), and credit about to
/// expire (`⏳`).
#[must_use]
pub fn alerts(budgets: &Budgets) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(alert) = budgets.topup_alert(Registry::builtin()) {
        lines.push(format!("⚠ {alert}"));
    }
    if let Some(alert) = budgets.absent_alert() {
        lines.push(format!("⚠ {alert}"));
    }
    if let Some(alert) = budgets.expiry_alert(balance::today()) {
        lines.push(format!("⏳ {alert}"));
    }
    lines
}

/// The ledger a command uses when none is named: `SUSI_HARNESS_LEDGER` when
/// set, else `out/ledger.jsonl` under `cwd` in a susi workspace
/// ([`is_workspace`]), else `harness/ledger.jsonl` in the data home
/// ([`crate::secrets::data_home`]). The data home is the default so that what
/// the harness learns (spend, latency, failures) follows the user from one
/// directory to the next.
#[must_use]
pub fn default_ledger(cwd: &Path) -> std::path::PathBuf {
    ledger_for(
        cwd,
        crate::secrets::setting("SUSI_HARNESS_LEDGER"),
        &crate::secrets::data_home(),
    )
}

/// [`default_ledger`] with the setting and the data home given.
#[must_use]
pub fn ledger_for(cwd: &Path, configured: Option<String>, data_home: &Path) -> std::path::PathBuf {
    if let Some(path) = configured {
        return std::path::PathBuf::from(path);
    }
    if is_workspace(cwd) {
        return cwd.join("out").join("ledger.jsonl");
    }
    data_home.join("harness").join("ledger.jsonl")
}

/// Whether `cwd` is a susi workspace, whose ledger is its own
/// `out/ledger.jsonl`: one that already keeps that ledger, or a susi
/// checkout (it carries `deploy/harness/prices.json` or
/// `.agents/roadmap.json`).
///
/// `out/` is not tracked, so a fresh checkout has no ledger yet. Judging by
/// the ledger alone sent a standalone gateway started there to the data
/// home's ledger, while `susi harness run` in the same checkout read
/// `out/ledger.jsonl`, which stayed empty: its budget caps saw nothing spent
/// and its fleet saw no failure.
#[must_use]
pub fn is_workspace(cwd: &Path) -> bool {
    cwd.join("out").join("ledger.jsonl").exists()
        || cwd
            .join("deploy")
            .join("harness")
            .join("prices.json")
            .exists()
        || cwd.join(".agents").join("roadmap.json").exists()
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
        let upper = balance::env_suffix(&provider.id);
        let settings = [
            (
                balance::budget_variable(&provider.id),
                format!(
                    "total dollars credited to {} when it publishes no balance; the ledger's spend is subtracted, so raise it by each top-up",
                    provider.id
                ),
            ),
            (
                balance::expiry_variable(&provider.id),
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
    let auto = catalogue
        .map(|catalogue| unpriced_last(auto_config(registry, catalogue, budgets, evidence)));
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
            "cooling": &evidence.cooling,
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
                    absent: Vec::new(),
                    expires_day: None,
                    key_refused: false,
                })
                .collect(),
        }
    }

    /// The observed failure: `--workers gemini-flash,deepseek-flash,...` with
    /// only DEEPSEEK_API_KEY set kept gemini-flash, and every task on it lost
    /// two attempts to the gateway's 502 before a DeepSeek model ran.
    #[test]
    fn an_explicit_list_leaves_off_a_model_whose_provider_has_no_key() {
        // Only DeepSeek is keyed: Gemini has no budget entry at all.
        let gate = budgets(&[("deepseek", Some(5_000_000))]);
        let fleet = resolve_fleet(
            "gemini-flash,deepseek-flash,deepseek-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &Evidence::default(),
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["deepseek-flash", "deepseek-pro"]);
        assert_eq!(fleet.task_workers(), vec!["deepseek-flash"]);
        assert!(
            fleet.left_off[0].contains("gemini-flash left off: none of GEMINI_API_KEY"),
            "{:?}",
            fleet.left_off
        );
    }

    /// One 429 used to drop the model from an explicit list until a probe
    /// five minutes later; with one model, `up` and `run` refused to start.
    #[test]
    fn a_transient_failure_keeps_the_model_and_a_refusal_through_an_alias_drops_it() {
        let gate = budgets(&[("deepseek", Some(50_000_000))]);
        let mut evidence = Evidence::default();
        evidence.cooling.insert("deepseek-flash".to_string(), 1);
        let kept = resolve_fleet(
            "deepseek-flash",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &evidence,
        )
        .unwrap();
        assert_eq!(kept.ladder, vec!["deepseek-flash"]);
        let mut refused = Evidence::default();
        refused.failed.insert("deepseek-flash".to_string(), 1);
        let error = resolve_fleet(
            "coder",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &refused,
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("coder left off: its last real call was refused"),
            "{error}"
        );
    }

    #[test]
    fn an_explicit_list_leaves_off_a_model_its_provider_no_longer_lists() {
        let mut gate = budgets(&[("gemini", None)]);
        gate.providers[0].working = true;
        gate.providers[0].absent = vec!["gemini-pro".to_string()];
        let fleet = resolve_fleet(
            "gemini-flash,gemini-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &Evidence::default(),
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["gemini-flash"]);
        assert!(
            fleet.left_off[0].contains("no longer lists"),
            "{:?}",
            fleet.left_off
        );
    }

    /// A model the registry cannot resolve goes to OpenAI; once refused, it
    /// was never probed again, so it stayed off every explicit list.
    #[test]
    fn a_refused_unregistered_model_is_probed_once_the_refusal_is_old() {
        let registry = Registry::builtin();
        let openai_only = |name: &str| name == "OPENAI_API_KEY";
        assert!(routable("gpt-4o", registry, &openai_only));
        assert!(!routable("gpt-4o", registry, &|_| false));
        assert!(routable("gpt-luna", registry, &openai_only));
        assert!(!routable("deepseek-flash", registry, &openai_only));
        let failures = BTreeMap::from([
            (
                "gpt-4o".to_string(),
                crate::policy::ModelFailure {
                    at_unix_ms: 1_000,
                    class: crate::ledger::FailureClass::NoCredit,
                },
            ),
            (
                "busy".to_string(),
                crate::policy::ModelFailure {
                    at_unix_ms: 1_000,
                    class: crate::ledger::FailureClass::RateLimited,
                },
            ),
        ]);
        let reach = |model: &str| routable(model, registry, &openai_only);
        assert!(due_for_probe(&failures, 1_000 + PROBE_AFTER_MS - 1, &reach).is_empty());
        assert_eq!(
            due_for_probe(&failures, 1_000 + PROBE_AFTER_MS, &reach),
            vec!["gpt-4o".to_string()],
            "a transient failure cools down on its own and is not probed"
        );
    }

    /// A probe that hit a rate limit leaves the model eligible, so the report
    /// must not call it inactive; only a refused probe keeps a model off.
    #[test]
    fn a_probe_that_failed_transiently_is_not_reported_as_inactive() {
        let evidence = Evidence {
            failed: BTreeMap::from([("refused".to_string(), 1)]),
            cooling: BTreeMap::from([("busy".to_string(), 1)]),
            probed: vec![
                ("healed".to_string(), true),
                ("refused".to_string(), false),
                ("busy".to_string(), false),
            ],
            ..Evidence::default()
        };
        assert_eq!(
            probe_report(&evidence),
            vec![
                "probe: healed answered; active again",
                "probe: refused failed again; it stays inactive",
                "probe: busy failed transiently; it stays on the ladder, tried after the others",
            ]
        );
    }

    #[test]
    fn an_unpriced_model_runs_last_in_its_tier_even_when_it_caches() {
        // deepseek-flash caches; kimi-flash does not. With deepseek-flash
        // unpriced, the cache key sorted it first.
        let mut catalogue = PriceCatalogue::builtin();
        catalogue.models.remove("deepseek-flash");
        let auto = Registry::builtin().auto_config_ranked(
            &|name| name == "DEEPSEEK_API_KEY" || name == "MOONSHOT_API_KEY",
            &catalogue,
            UsageProfile::agent(),
            &ProviderGate {
                active: &|_| true,
                use_first: &|_| None,
                latency: &|_| None,
                live: &|_| true,
            },
        );
        assert_eq!(auto.unpriced, vec!["deepseek-flash"]);
        let ranked = unpriced_last(auto);
        assert_eq!(
            ranked.workers.last().map(String::as_str),
            Some("deepseek-flash")
        );
        assert_eq!(
            ranked.workers.first().map(String::as_str),
            Some("kimi-flash")
        );
        assert_eq!(ranked.orchestrator.as_deref(), Some("deepseek-pro"));
    }

    #[test]
    fn a_fresh_susi_checkout_keeps_its_own_ledger_before_the_first_write() {
        let home = tempfile::tempdir().unwrap();
        let checkout = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(checkout.path().join("deploy/harness")).unwrap();
        std::fs::write(checkout.path().join("deploy/harness/prices.json"), "{}").unwrap();
        assert_eq!(
            ledger_for(checkout.path(), None, home.path()),
            checkout.path().join("out").join("ledger.jsonl")
        );
        let elsewhere = tempfile::tempdir().unwrap();
        assert_eq!(
            ledger_for(elsewhere.path(), None, home.path()),
            home.path().join("harness").join("ledger.jsonl")
        );
        assert_eq!(
            ledger_for(checkout.path(), Some("x.jsonl".to_string()), home.path()),
            std::path::PathBuf::from("x.jsonl")
        );
    }

    #[test]
    fn an_explicit_list_drives_with_its_strong_model_and_drops_inactive_ones() {
        let gate = budgets(&[("deepseek", Some(5_000_000)), ("gemini", Some(10))]);
        let fleet = resolve_fleet(
            "gemini-flash, deepseek-flash,deepseek-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &Evidence::default(),
        )
        .unwrap();
        assert_eq!(fleet.orchestrator.as_deref(), Some("deepseek-pro"));
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
        assert!(listed.left_off[0].contains("last real call was refused"));
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
    fn an_explicit_list_takes_its_roles_from_the_registry() {
        let gate = budgets(&[("deepseek", Some(50_000_000)), ("gemini", Some(50_000_000))]);
        let fleet = resolve_fleet(
            "gemini-flash,deepseek-flash,deepseek-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &Evidence::default(),
        )
        .unwrap();
        assert_eq!(fleet.orchestrator.as_deref(), Some("deepseek-pro"));
        assert_eq!(fleet.workers, vec!["gemini-flash", "deepseek-flash"]);
        assert_eq!(fleet.fixers, vec!["deepseek-pro"]);
        assert_eq!(fleet.task_workers(), vec!["gemini-flash", "deepseek-flash"]);
    }

    #[test]
    fn auto_model_takes_the_fleets_orchestrator() {
        let fleet = Fleet {
            orchestrator: Some("deepseek-pro".to_string()),
            ladder: vec!["deepseek-pro".to_string()],
            ..Fleet::default()
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
