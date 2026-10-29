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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
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
    /// Models left off only because their last real call was refused (the
    /// key was refused or had no credit), and whose key is set here, so a
    /// probe from this process reaches them ([`routable`]). A running
    /// mission keeps them in reserve: it probes them with its ladder once the
    /// refusal is [`PROBE_AFTER_MS`] old ([`readmit`]) and takes each back
    /// when a probe answers. Until then no task's order holds them
    /// ([`crate::policy::arrange`] drops a refused model).
    ///
    /// They were left out of everything a mission held, so a key fixed or an
    /// account topped up after the run started stayed unused until the next
    /// run, against the promise that it comes back without a restart.
    pub standby: Vec<String>,
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
/// such answer must not empty a fleet of one model. A model left off only
/// for its refusal is kept in reserve ([`Fleet::standby`]). The catalogue is
/// loaded only for `auto`.
///
/// A listed model whose provider has no key set in this process is left
/// off: this function cannot ask the gateway that will carry the calls. A
/// command that drives one uses [`resolve_fleet_through`].
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
    resolve_fleet_through(spec, catalogue, budgets, evidence, None)
}

/// How a command asks the gateway it drives whether that gateway answers a
/// call to a model ([`gateway_answers`]): `Err` says why not.
pub type GatewayCheck<'a> = &'a dyn Fn(&str) -> Result<(), String>;

/// [`resolve_fleet`] for a command that drives a gateway, which `gateway`
/// asks ([`gateway_answers`]).
///
/// A listed model whose provider has no key set in this process is asked
/// about there before it is left off. A gateway on an explicit `--upstream`
/// (a proxy that holds the keys) routes every model without a key here, and
/// so does one started with its own key (`--api-key-env`, or another
/// environment); every model of such a list was left off with "none of ...
/// is set", and the run refused to start although the gateway would have
/// served every call. A direct gateway missing the same key answers at once
/// that it cannot route the model, without a provider call, and the model is
/// left off as before.
///
/// The same holds for such a model whose last real call was refused, and for
/// a refused name the registry cannot resolve while `OPENAI_API_KEY` is not
/// set here: no probe from this process reaches it ([`routable`] needs the
/// key here), so once its refusal is [`PROBE_AFTER_MS`] old the gateway is
/// asked instead, and the model stays when it answers.
///
/// # Errors
///
/// As [`resolve_fleet`].
pub fn resolve_fleet_through(
    spec: &str,
    catalogue: impl FnOnce() -> EaiResult<PriceCatalogue>,
    budgets: &Budgets,
    evidence: &Evidence,
    gateway: Option<GatewayCheck<'_>>,
) -> EaiResult<Fleet> {
    let registry = Registry::builtin();
    if spec.trim() != "auto" {
        let here = Here {
            gateway,
            key_set: &crate::secrets::is_set,
            now_unix_ms: now_ms(),
        };
        return explicit_fleet(spec, budgets, evidence, &here);
    }
    let catalogue = catalogue()?;
    let auto = unpriced_last(auto_config(registry, &catalogue, budgets, evidence));
    let ladder = auto.ladder();
    let standby = auto_standby(&auto, budgets, evidence);
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
        standby,
    })
}

/// Where a command resolves an explicit list: the gateway it drives, if any,
/// how a key variable is read here, and the time.
struct Here<'a> {
    gateway: Option<GatewayCheck<'a>>,
    key_set: &'a dyn Fn(&str) -> bool,
    now_unix_ms: u64,
}

/// [`resolve_fleet_through`] for an explicit list, run `here`.
fn explicit_fleet(
    spec: &str,
    budgets: &Budgets,
    evidence: &Evidence,
    here: &Here<'_>,
) -> EaiResult<Fleet> {
    let registry = Registry::builtin();
    let mut models = Vec::new();
    let mut left_off = Vec::new();
    let mut standby = Vec::new();
    for model in spec.split(',').map(str::trim).filter(|m| !m.is_empty()) {
        let resolved = registry.resolve(model);
        // The real calls decide first: a model whose last call was refused is
        // inactive, whatever its provider's budget says. The evidence is keyed
        // by registry id, and the list may use an alias.
        let Some(&refused_at) = evidence.failed.get(&crate::policy::canonical(model)) else {
            match explicit_off(model, resolved.as_ref(), budgets, here.gateway) {
                Some(note) => left_off.push(note),
                None => models.push(model.to_string()),
            }
            continue;
        };
        let refused = "its last real call was refused (key refused or no credit)";
        let minutes = PROBE_AFTER_MS / 60_000;
        // A probe from here needs the model's key here ([`routable`]): a
        // model kept only because the gateway answers for it was left off by
        // its first refusal "until a probe re-admits it", and no probe was
        // ever sent, at run start or mid-mission, so it was off for every
        // later run. The gateway that refused it is asked again instead.
        let Some(keys) = unset_here(resolved.as_ref(), budgets, here.key_set) else {
            left_off.push(format!(
                "{model} left off: {refused}; a probe re-admits it once the refusal is {minutes} minutes old"
            ));
            // Off for its refusal alone, it waits in reserve for the probe;
            // one that could not run anyway does not.
            if explicit_off(model, resolved.as_ref(), budgets, None).is_none() {
                standby.push(model.to_string());
            }
            continue;
        };
        let unset = format!("none of {keys} is set here");
        match here.gateway {
            Some(ask) if here.now_unix_ms.saturating_sub(refused_at) >= PROBE_AFTER_MS => {
                // The gateway records the call like a probe's: an answer is
                // the model's last real call now, and supersedes the refusal.
                match ask(model) {
                    Ok(()) => models.push(model.to_string()),
                    Err(why) => {
                        left_off.push(format!("{model} left off: {refused}, {unset}, and {why}"));
                    }
                }
            }
            Some(_) => left_off.push(format!(
                "{model} left off: {refused}, and {unset}, so no probe from here reaches it; a command that drives the gateway asks it once the refusal is {minutes} minutes old"
            )),
            None => left_off.push(format!(
                "{model} left off: {refused}, and {unset}, so no probe from here can re-admit it until the key is set here"
            )),
        }
    }
    if models.is_empty() {
        return Err(EaiError::config(format!(
            "every model in the list is inactive ({}): {}",
            left_off.join("; "),
            topup_text(budgets)
        )));
    }
    // Roles come from the registry: a rescue model is strong (a fixer, and
    // the orchestrator), anything else is a light worker.
    let strong = |model: &String| registry.tier_of(model) == Some(crate::providers::Tier::Rescue);
    let (fixers, workers): (Vec<String>, Vec<String>) =
        models.iter().cloned().partition(|model| strong(model));
    Ok(Fleet {
        orchestrator: fixers.first().or_else(|| models.first()).cloned(),
        workers,
        fixers,
        ladder: models,
        left_off,
        standby,
    })
}

/// The key variables a call to `model` needs and that are not set here, or
/// `None` when a probe from this process reaches it ([`routable`]). A
/// registry model's provider has a budget entry exactly when one of its keys
/// is set here ([`balance::collect`]); a name the registry cannot resolve
/// goes to OpenAI ([`crate::router::resolve_route`]'s fallback).
fn unset_here(
    resolved: Option<&crate::providers::ResolvedModel>,
    budgets: &Budgets,
    key_set: &dyn Fn(&str) -> bool,
) -> Option<String> {
    match resolved {
        Some(resolved) => (!resolved.key_envs.is_empty() && !budgets.gated(&resolved.provider))
            .then(|| resolved.key_envs.join(" / ")),
        None => (!key_set("OPENAI_API_KEY")).then(|| "OPENAI_API_KEY".to_string()),
    }
}

/// The models `auto` left out for their refusal alone ([`Fleet::standby`]).
/// Its `failed` also names a model its provider no longer lists, which no
/// probe brings back, and it is filled before the tools check.
fn auto_standby(
    auto: &crate::providers::AutoConfig,
    budgets: &Budgets,
    evidence: &Evidence,
) -> Vec<String> {
    let registry = Registry::builtin();
    let mut standby = Vec::new();
    for model in &auto.failed {
        if evidence.failed.contains_key(model)
            && !budgets.absent(model)
            && registry.require(model, &[Capability::Tools]).is_ok()
            && !standby.contains(model)
        {
            standby.push(model.clone());
        }
    }
    standby
}

/// Why `model`, named in an explicit list, cannot run, or `None` when it
/// can: its provider has no key set (and `gateway`, when given, does not
/// answer for it either), is inactive, or no longer lists it. A name the
/// registry cannot resolve is kept. A refusal is judged before this.
fn explicit_off(
    model: &str,
    resolved: Option<&crate::providers::ResolvedModel>,
    budgets: &Budgets,
    gateway: Option<GatewayCheck<'_>>,
) -> Option<String> {
    let resolved = resolved?;
    // A keyed provider whose key is not set has no budget entry
    // ([`balance::collect`] skips it). Its model was kept, a direct gateway
    // answered every call 502 "none of ... are set" without a ledger row, and
    // the worker read the 502 as transient and spent two of a task's
    // attempts on it.
    if !resolved.key_envs.is_empty() && !budgets.gated(&resolved.provider) {
        let unset = format!("none of {} is set", resolved.key_envs.join(" / "));
        return match gateway {
            Some(ask) => ask(model)
                .err()
                .map(|why| format!("{model} left off: {unset} here, and {why}")),
            None => Some(format!("{model} left off: {unset}")),
        };
    }
    if budgets.gated(&resolved.provider) && !budgets.is_active(&resolved.provider) {
        return Some(format!(
            "{model} left off: provider {} is inactive (under $1 or unknown)",
            resolved.provider
        ));
    }
    if budgets.absent(model) {
        return Some(format!(
            "{model} left off: provider {} no longer lists it",
            resolved.provider
        ));
    }
    None
}

/// Whether the gateway at `gateway_url` answers a call to `model`: one small
/// call through it, metered and recorded by the gateway like any other
/// ([`resolve_fleet_through`] sends it only for a model whose key is not set
/// in this process).
///
/// # Errors
///
/// Says why not: the gateway did not answer at all, it cannot route the
/// model either (it has no key for it either, so it made no provider call),
/// or the provider refused the call (the key was refused or has no credit).
/// Any other answer, a rate limit or a 5xx included, means the gateway
/// routes the model: one transient failure must not take it off the fleet.
pub fn gateway_answers(gateway_url: &str, model: &str) -> Result<(), String> {
    let url = format!("{}/v1/chat/completions", gateway_url.trim_end_matches('/'));
    // A prompt no earlier call sent: the gateway's response cache would
    // otherwise answer a repeat from disk without routing it, whatever keys
    // the gateway holds now.
    let request = serde_json::json!({
        "model": model,
        "messages": [{
            "role": "user",
            "content": format!(
                "Reply with the single word: ok ({}-{})",
                std::process::id(),
                now_ms()
            ),
        }],
        "max_tokens": 16,
    });
    let bytes = serde_json::to_vec(&request).unwrap_or_default();
    let headers = [
        ("authorization", "Bearer susi:probe:probe:1"),
        ("content-type", "application/json"),
        ("x-susi-mission", "probe"),
        ("x-susi-task", "probe"),
    ];
    let call =
        susi_http_transport::http_call_with_body("POST", &url, &headers, Some(&bytes), 60, 0)
            .map_err(|error| format!("the gateway at {gateway_url} did not answer ({error})"))?;
    let status = call.status;
    if (200..300).contains(&status) {
        return Ok(());
    }
    let body = call.into_bytes(1 << 16).unwrap_or_default();
    // The gateway's own answer when it has no route for the model (no key
    // for its provider): nothing was sent upstream.
    if String::from_utf8_lossy(&body).contains("resolve route") {
        return Err(format!(
            "the gateway at {gateway_url} cannot route it either"
        ));
    }
    match crate::gateway::provider_failure(status, &body) {
        Some(crate::ledger::FailureClass::NoCredit) => Err(format!(
            "the gateway's call to it was refused (HTTP {status}: key refused or no credit)"
        )),
        _ => Ok(()),
    }
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
/// [`PROBE_AFTER_MS`] before `now_unix_ms` and that a call from here can
/// reach ([`routable`]), as a run does when it starts. A refused key or an
/// empty account was otherwise off for the rest of the mission even after it
/// was fixed or topped up. Each probe appends its row to `ledger`, so the
/// caller re-reads it. Returns the models probed and whether each answered,
/// including a model another task was probing at the same moment: its answer
/// is waited for, not probed twice.
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

/// A probe's answer: whether the model answered, or how the call failed.
type ProbeAnswer = Result<(), crate::ledger::FailureClass>;

/// Models a probe is calling now, in this process, each with the slot its
/// answer goes in. Tasks prepared in parallel each re-admit their own ladder;
/// one model is probed once, not once per task.
static PROBING: Mutex<BTreeMap<String, Arc<InFlight>>> = Mutex::new(BTreeMap::new());

/// One probe call in flight, and its answer once the thread making it has
/// one: `Some(None)` when that thread ended without one.
#[derive(Default)]
struct InFlight {
    answer: Mutex<Option<Option<ProbeAnswer>>>,
    done: Condvar,
    /// How many threads wait on this answer.
    waiters: AtomicUsize,
}

impl InFlight {
    /// Store the answer, unless one is already stored, and wake every waiter.
    fn settle(&self, answer: Option<ProbeAnswer>) {
        let mut slot = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(answer);
        }
        self.done.notify_all();
    }

    /// Block until the probing thread settles, and return its answer.
    fn wait(&self) -> Option<ProbeAnswer> {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        let slot = self.answer.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = self
            .done
            .wait_while(slot, |answer| answer.is_none())
            .unwrap_or_else(PoisonError::into_inner);
        (*slot).flatten()
    }
}

/// The probes one thread registered in [`PROBING`]. Dropping it settles any
/// it never answered and unregisters them, so a thread waiting on one of
/// them is never left waiting, even when a probe panicked.
struct Registered<'a>(&'a [(String, Arc<InFlight>)]);

impl Drop for Registered<'_> {
    fn drop(&mut self) {
        let mut probing = PROBING.lock().unwrap_or_else(PoisonError::into_inner);
        for (model, slot) in self.0 {
            slot.settle(None);
            if probing
                .get(model)
                .is_some_and(|current| Arc::ptr_eq(current, slot))
            {
                probing.remove(model);
            }
        }
    }
}

/// Probe every model of `due` at once. Returns each probed model and its
/// answer.
fn probe_all(
    due: &[String],
    ledger: &Path,
    catalogue: Option<&PriceCatalogue>,
) -> Vec<(String, ProbeAnswer)> {
    probe_all_with(due, &|model| probe_call(model, ledger, catalogue))
}

/// [`probe_all`] with the call given. A model another thread of this process
/// is already probing is not called again: this thread waits for that
/// probe's answer and returns it as its own.
///
/// The thread that lost the race used to skip the model and return nothing
/// for it. Its task then ordered its ladder from the ledger it read before
/// the probe, saw every model still refused, and stopped the whole mission
/// "for a top-up" while the other task ran on the model the probe had just
/// re-admitted. The winner's probe row is in the ledger before its answer is
/// settled, so a caller that re-reads the ledger on an answer sees it.
fn probe_all_with(
    due: &[String],
    probe: &(dyn Fn(&str) -> ProbeAnswer + Sync),
) -> Vec<(String, ProbeAnswer)> {
    let mut mine = Vec::new();
    let mut theirs = Vec::new();
    {
        let mut probing = PROBING.lock().unwrap_or_else(PoisonError::into_inner);
        for model in due {
            if let Some(slot) = probing.get(model) {
                theirs.push((model.clone(), Arc::clone(slot)));
            } else {
                let slot = Arc::new(InFlight::default());
                probing.insert(model.clone(), Arc::clone(&slot));
                mine.push((model.clone(), slot));
            }
        }
    }
    let registered = Registered(&mine);
    let mut answers = std::thread::scope(|scope| {
        let handles = mine
            .iter()
            .map(|(model, slot)| {
                scope.spawn(move || {
                    let answer = probe(model);
                    slot.settle(Some(answer));
                    (model.clone(), answer)
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect::<Vec<_>>()
    });
    drop(registered);
    for (model, slot) in theirs {
        if let Some(answer) = slot.wait() {
            answers.push((model, answer));
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

    /// `susi harness gateway --upstream <proxy>` holds the keys, and `run`
    /// executes where no DEEPSEEK_API_KEY is set: every listed model was left
    /// off with "none of DEEPSEEK_API_KEY is set" and the run refused to
    /// start, although the gateway would have served every call.
    #[test]
    fn a_model_without_a_key_here_stays_when_the_gateway_answers_for_it() {
        let no_keys = Budgets::default();
        let asked = Mutex::new(Vec::new());
        let routes = |model: &str| -> Result<(), String> {
            asked.lock().unwrap().push(model.to_string());
            Ok(())
        };
        let fleet = resolve_fleet_through(
            "deepseek-flash,deepseek-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &no_keys,
            &Evidence::default(),
            Some(&routes),
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["deepseek-flash", "deepseek-pro"]);
        assert!(fleet.left_off.is_empty(), "{:?}", fleet.left_off);
        assert_eq!(
            *asked.lock().unwrap(),
            vec!["deepseek-flash", "deepseek-pro"]
        );

        // A direct gateway without the key cannot route it either: the model
        // is left off, as when no gateway is asked.
        let direct = |_: &str| -> Result<(), String> {
            Err("the gateway at http://gw cannot route it either".to_string())
        };
        let error = resolve_fleet_through(
            "deepseek-flash",
            || unreachable!("an explicit list needs no catalogue"),
            &no_keys,
            &Evidence::default(),
            Some(&direct),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(
                "deepseek-flash left off: none of DEEPSEEK_API_KEY is set here, and the gateway at http://gw cannot route it either"
            ),
            "{error}"
        );

        // A model whose key is set here is never asked about.
        let keyed = budgets(&[("deepseek", Some(5_000_000))]);
        let never = |model: &str| -> Result<(), String> {
            panic!("{model} has a key here; the gateway is not asked")
        };
        let fleet = resolve_fleet_through(
            "deepseek-flash",
            || unreachable!("an explicit list needs no catalogue"),
            &keyed,
            &Evidence::default(),
            Some(&never),
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["deepseek-flash"]);
    }

    /// One answer from a stub gateway, after reading the request.
    fn gateway_answering(status: &'static str, body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                let read = stream.read(&mut chunk).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });
        format!("http://{address}/")
    }

    #[test]
    fn the_gateway_is_asked_whether_it_can_route_a_model() {
        let ok = gateway_answering(
            "200 OK",
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":5,"completion_tokens":1}}"#,
        );
        assert_eq!(gateway_answers(&ok, "deepseek-flash"), Ok(()));
        // What a direct gateway answers when it has no key for the model.
        let unrouted = gateway_answering(
            "502 Bad Gateway",
            r#"{"error":{"message":"resolve route: none of DEEPSEEK_API_KEY are set"}}"#,
        );
        let why = gateway_answers(&unrouted, "deepseek-flash").unwrap_err();
        assert!(why.contains("cannot route it either"), "{why}");
        let refused = gateway_answering("401 Unauthorized", r#"{"error":{"message":"bad key"}}"#);
        let why = gateway_answers(&refused, "deepseek-flash").unwrap_err();
        assert!(why.contains("refused"), "{why}");
        // A rate limit means the gateway routed the call: the model stays.
        let busy = gateway_answering(
            "429 Too Many Requests",
            r#"{"error":{"message":"slow down"}}"#,
        );
        assert_eq!(gateway_answers(&busy, "deepseek-flash"), Ok(()));
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        let why = gateway_answers(&closed, "deepseek-flash").unwrap_err();
        assert!(why.contains("did not answer"), "{why}");
    }

    /// A model refused before the run started was off the fleet and out of
    /// every task's ladder, so the before-each-task probe never reached it:
    /// topped up mid-mission, it stayed unused until the next run.
    #[test]
    fn a_model_left_off_for_its_refusal_alone_is_kept_in_reserve() {
        let gate = budgets(&[("deepseek", Some(50_000_000))]);
        let mut evidence = Evidence::default();
        evidence.failed.insert("deepseek-flash".to_string(), 1);
        // Gemini has no key here: refused or not, no probe can reach it.
        evidence.failed.insert("gemini-flash".to_string(), 1);
        let fleet = resolve_fleet(
            "deepseek-flash,gemini-flash,deepseek-pro",
            || unreachable!("an explicit list needs no catalogue"),
            &gate,
            &evidence,
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["deepseek-pro"]);
        assert_eq!(fleet.standby, vec!["deepseek-flash"]);

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
        assert_eq!(
            auto_standby(&auto, &gate, &evidence),
            vec!["deepseek-flash"]
        );
        // A model its provider no longer lists is not waiting for a probe.
        let mut gone = gate.clone();
        gone.providers[0].absent = vec!["deepseek-flash".to_string()];
        assert!(auto_standby(&auto, &gone, &evidence).is_empty());
    }

    /// `run` drives a gateway that holds the DeepSeek key (an explicit
    /// `--upstream`, or `--api-key-env`) from a shell without it. One refused
    /// call left deepseek-flash off "until a probe re-admits it", but a probe
    /// from here needs the key here, so none was ever sent, at run start or
    /// mid-mission, and the model was off for every later run.
    #[test]
    fn a_refused_model_without_a_key_here_comes_back_when_the_gateway_answers() {
        let no_keys = Budgets::default();
        let no_key_here = |_: &str| false;
        let now = 10 * PROBE_AFTER_MS;
        let mut evidence = Evidence::default();
        // Refused long enough ago to be probed, and refused just now.
        evidence
            .failed
            .insert("deepseek-flash".to_string(), now - PROBE_AFTER_MS);
        evidence.failed.insert("deepseek-pro".to_string(), now - 1);
        let asked = Mutex::new(Vec::new());
        let answers = |model: &str| -> Result<(), String> {
            asked.lock().unwrap().push(model.to_string());
            Ok(())
        };
        let fleet = explicit_fleet(
            "deepseek-flash,deepseek-pro",
            &no_keys,
            &evidence,
            &Here {
                gateway: Some(&answers),
                key_set: &no_key_here,
                now_unix_ms: now,
            },
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["deepseek-flash"]);
        assert_eq!(
            *asked.lock().unwrap(),
            vec!["deepseek-flash"],
            "a refusal under five minutes old is not asked about yet"
        );
        assert!(
            fleet.standby.is_empty(),
            "no probe from here reaches either"
        );
        assert_eq!(fleet.left_off.len(), 1, "{:?}", fleet.left_off);
        let young = &fleet.left_off[0];
        assert!(
            young.starts_with("deepseek-pro left off")
                && young.contains("none of DEEPSEEK_API_KEY is set here")
                && young.contains(
                    "a command that drives the gateway asks it once the refusal is 5 minutes old"
                ),
            "{young}"
        );
        assert!(!young.contains("a probe re-admits it"), "{young}");

        // Refused again through the gateway: off, with the gateway's reason,
        // and no promise of a probe that cannot be sent.
        let refuses = |_: &str| -> Result<(), String> {
            Err("the gateway's call to it was refused (HTTP 402: key refused or no credit)".into())
        };
        let error = explicit_fleet(
            "deepseek-flash",
            &no_keys,
            &evidence,
            &Here {
                gateway: Some(&refuses),
                key_set: &no_key_here,
                now_unix_ms: now,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(
                "deepseek-flash left off: its last real call was refused (key refused or no credit), none of DEEPSEEK_API_KEY is set here, and the gateway's call to it was refused"
            ),
            "{error}"
        );
        assert!(!error.contains("a probe re-admits it"), "{error}");

        // With no gateway to ask, nothing this command sends can re-admit it.
        let error = explicit_fleet(
            "deepseek-flash",
            &no_keys,
            &evidence,
            &Here {
                gateway: None,
                key_set: &no_key_here,
                now_unix_ms: now,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("no probe from here can re-admit it until the key is set here"),
            "{error}"
        );
        assert!(!error.contains("a probe re-admits it"), "{error}");
    }

    /// A name the registry cannot resolve goes to OpenAI, so a probe from
    /// here needs OPENAI_API_KEY here: a proxy's own model name, refused once
    /// through a gateway on an explicit upstream, waited in reserve for a
    /// probe that was never sent.
    #[test]
    fn a_refused_unregistered_model_is_probed_from_here_only_with_the_openai_key_here() {
        let no_keys = Budgets::default();
        let now = 10 * PROBE_AFTER_MS;
        let mut evidence = Evidence::default();
        evidence
            .failed
            .insert("proxy-model".to_string(), now - PROBE_AFTER_MS);
        let asked = Mutex::new(Vec::new());
        let answers = |model: &str| -> Result<(), String> {
            asked.lock().unwrap().push(model.to_string());
            Ok(())
        };
        let fleet = explicit_fleet(
            "proxy-model",
            &no_keys,
            &evidence,
            &Here {
                gateway: Some(&answers),
                key_set: &|_| false,
                now_unix_ms: now,
            },
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["proxy-model"]);
        assert_eq!(*asked.lock().unwrap(), vec!["proxy-model"]);

        asked.lock().unwrap().clear();
        let fleet = explicit_fleet(
            "proxy-model,proxy-other",
            &no_keys,
            &evidence,
            &Here {
                gateway: Some(&answers),
                key_set: &|name| name == "OPENAI_API_KEY",
                now_unix_ms: now,
            },
        )
        .unwrap();
        assert_eq!(fleet.ladder, vec!["proxy-other"]);
        assert_eq!(fleet.standby, vec!["proxy-model"]);
        assert!(fleet.left_off[0].contains("a probe re-admits it"));
        assert!(
            asked.lock().unwrap().is_empty(),
            "a probe from here reaches it"
        );
    }

    /// Two tasks of one batch re-admit the same refused model at once. The
    /// one that lost the race to probe it got no answer, ordered its ladder
    /// from the stale ledger, found every model refused and stopped the
    /// mission while the other task ran on the model the probe re-admitted.
    #[test]
    fn a_task_that_loses_the_probe_race_waits_for_the_answer() {
        let model = "probe-race-model".to_string();
        let calls = AtomicUsize::new(0);
        let (started, probing) = std::sync::mpsc::channel::<()>();
        let (release, released) = std::sync::mpsc::channel::<()>();
        let started = Mutex::new(started);
        let released = Mutex::new(released);
        let slow = |_: &str| -> ProbeAnswer {
            calls.fetch_add(1, Ordering::SeqCst);
            started.lock().unwrap().send(()).unwrap();
            released.lock().unwrap().recv().unwrap();
            Err(crate::ledger::FailureClass::NoCredit)
        };
        let fast = |_: &str| -> ProbeAnswer {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        std::thread::scope(|scope| {
            let winner = scope.spawn(|| probe_all_with(std::slice::from_ref(&model), &slow));
            probing.recv().unwrap();
            let loser = scope.spawn(|| probe_all_with(std::slice::from_ref(&model), &fast));
            // Hold the winner's probe until the loser waits on it, or has
            // returned without it.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let waiting = PROBING
                    .lock()
                    .unwrap()
                    .get(&model)
                    .is_some_and(|slot| slot.waiters.load(Ordering::SeqCst) > 0);
                if waiting || loser.is_finished() || std::time::Instant::now() > deadline {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            release.send(()).unwrap();
            let answer = vec![(model.clone(), Err(crate::ledger::FailureClass::NoCredit))];
            assert_eq!(winner.join().unwrap(), answer);
            assert_eq!(
                loser.join().unwrap(),
                answer,
                "the loser takes the in-flight probe's answer"
            );
        });
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one model, one probe");
        assert!(PROBING.lock().unwrap().get(&model).is_none());
    }

    /// One answer per model even when the probing thread gives none: a
    /// waiter is released, and nothing is reported for the model.
    #[test]
    fn a_probe_that_panics_releases_its_waiters() {
        let model = "probe-panic-model".to_string();
        let answers = probe_all_with(std::slice::from_ref(&model), &|_| -> ProbeAnswer {
            panic!("the probe call panicked")
        });
        assert!(answers.is_empty());
        assert!(PROBING.lock().unwrap().get(&model).is_none());
        // The next probe of the model is not stuck behind the lost one.
        assert_eq!(
            probe_all_with(std::slice::from_ref(&model), &|_| Ok(())),
            vec![(model.clone(), Ok(()))]
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
