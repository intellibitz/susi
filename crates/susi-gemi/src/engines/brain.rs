//! Evidence-driven brain selection. The router no longer trusts a hand-typed
//! provider ordering: it learns, per provider and per kind of task, how often
//! a model actually answered and how fast, and ranks candidates by that.
//!
//! * **Local is the floor, cloud is the ceiling.** A class prior nudges hard
//!   work (code, reasoning) toward cloud models and reflex-sized work toward
//!   local engines, but only until evidence says otherwise.
//! * **Evidence beats priors.** Success rate is Laplace-smoothed and weighted
//!   by sample count, so a handful of calls barely moves a provider while a
//!   history of failures (a dry account, a retired model) sinks it.
//! * **Constraints stay elsewhere.** Privacy, budget and cooldowns are the
//!   router's hard filters and run before ranking; this module only orders
//!   what survives.
use super::cost::{self, Budget};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

/// What kind of work a prompt is, for ranking purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskClass {
    /// Short, simple turns where latency matters more than depth.
    Reflex,
    Chat,
    Code,
    /// Long or analytical prompts that need the strongest model available.
    Reasoning,
}

impl TaskClass {
    pub const ALL: [Self; 4] = [Self::Reflex, Self::Chat, Self::Code, Self::Reasoning];

    pub fn label(self) -> &'static str {
        match self {
            Self::Reflex => "reflex",
            Self::Chat => "chat",
            Self::Code => "code",
            Self::Reasoning => "reasoning",
        }
    }

    /// Cheap, deterministic classification from the prompt text alone.
    pub fn classify(prompt: &str) -> Self {
        let p = prompt.to_ascii_lowercase();
        let words = p.split_whitespace().count();
        let code_markers = [
            "```",
            "fn ",
            "def ",
            "class ",
            "refactor",
            "implement",
            "compile",
            "stack trace",
            "unit test",
            "bug",
            "function",
            "regex",
            "sql",
            "cargo ",
            "git ",
        ];
        if code_markers.iter().any(|m| p.contains(m)) {
            return Self::Code;
        }
        let deep_markers = [
            "prove",
            "analy",
            "design",
            "architecture",
            "trade-off",
            "tradeoff",
            "step by step",
            "compare",
            "plan ",
            "why does",
            "strategy",
        ];
        if p.len() > 600 || deep_markers.iter().any(|m| p.contains(m)) {
            return Self::Reasoning;
        }
        if words <= 12 {
            Self::Reflex
        } else {
            Self::Chat
        }
    }
}

/// Outcome history for one (provider, class).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub ok: u32,
    pub fail: u32,
    /// Exponential moving average of successful-call latency.
    pub ema_ms: f32,
}

impl Record {
    fn samples(&self) -> u32 {
        self.ok + self.fail
    }
}

/// Above this many samples both counters are halved so old history cannot
/// pin a provider forever after it recovers (or degrades).
const HISTORY_CAP: u32 = 200;
/// Samples at which evidence carries half the weight of the prior.
const HALF_WEIGHT_SAMPLES: f32 = 5.0;
const NEUTRAL_PRIOR: f32 = 0.6;
const LATENCY_EMA_ALPHA: f32 = 0.3;

/// Why a provider call failed, as far as the error text reveals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// No credit / billing problem (HTTP 402, "insufficient balance").
    Funds,
    /// The key is rejected (HTTP 401/403).
    Auth,
    /// Throttled (HTTP 429); transient.
    RateLimit,
    /// The model id is retired or unknown.
    Gone,
    /// Network or endpoint trouble; transient.
    Transport,
    Other,
}

impl FailureKind {
    /// Funds and auth problems persist until an operator acts, so retrying on
    /// a short timer only burns failed calls.
    pub fn needs_operator(self) -> bool {
        matches!(self, Self::Funds | Self::Auth)
    }
}

/// Classify a provider error string. Funds outranks auth: a 402 body often
/// also says "unauthorized".
pub fn classify_error(error: &str) -> FailureKind {
    let e = error.to_ascii_lowercase();
    let any = |pats: &[&str]| pats.iter().any(|p| e.contains(p));
    if any(&[
        "http 402",
        "insufficient",
        "credit",
        "balance",
        "billing",
        "payment required",
        "quota exceeded",
        "exceeded your current quota",
    ]) {
        FailureKind::Funds
    } else if any(&[
        "http 401",
        "http 403",
        "unauthorized",
        "forbidden",
        "invalid api key",
        "incorrect api key",
        "authentication",
    ]) {
        FailureKind::Auth
    } else if any(&["http 429", "rate limit", "too many requests"]) {
        FailureKind::RateLimit
    } else if any(&[
        "http 404",
        "no endpoints found",
        "model not found",
        "does not exist",
        "no longer available",
        "is not found for api version",
    ]) {
        FailureKind::Gone
    } else if any(&[
        "connection refused",
        "tcp connect error",
        "timed out",
        "error sending request",
        "unreachable",
    ]) {
        FailureKind::Transport
    } else {
        FailureKind::Other
    }
}

/// Quarantine length after `consecutive` operator-needed failures: 10 min,
/// 1 h, then 6 h (capped — a topped-up account must be re-probed soon).
/// `None` for kinds that use the router's normal short cooldowns.
pub fn quarantine_secs(kind: FailureKind, consecutive: u32) -> Option<u64> {
    kind.needs_operator().then_some(match consecutive {
        0 | 1 => 600,
        2 => 3_600,
        _ => QUARANTINE_CAP_SECS,
    })
}

/// Longest quarantine, and how long a failure keeps a provider "recently
/// failed" for fitness purposes.
pub const QUARANTINE_CAP_SECS: u64 = 21_600;

/// Failure streak for a provider (or `vendor:<scope>` for credential-scoped
/// kinds).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Health {
    pub kind: FailureKind,
    pub consecutive: u32,
    pub last_unix: u64,
}

/// Evidence-rate below which a recently failing provider is unfit.
const UNFIT_RATE: f32 = 0.34;
const UNFIT_MIN_SAMPLES: u32 = 3;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    /// Key: `<provider>|<class>`.
    records: BTreeMap<String, Record>,
    /// Failure streaks; see [`Health`].
    #[serde(default)]
    health: BTreeMap<String, Health>,
    /// Mission ids whose verified outcome was already applied (bounded), so a
    /// re-read of the trace file never double-counts a mission.
    #[serde(default)]
    applied: Vec<String>,
}

/// Applied-mission ids kept; older missions fall out and are never re-read
/// because traces are only scanned newest-first.
const APPLIED_CAP: usize = 500;

/// The receipt tool name that tags a mission with the provider that answered.
pub fn receipt_tool(provider: &str, class: TaskClass) -> String {
    format!("brain:{}", key(provider, class))
}

fn parse_served(entry: &str) -> Option<(&str, TaskClass)> {
    let (provider, class) = entry.rsplit_once('|')?;
    let class = TaskClass::ALL.into_iter().find(|c| c.label() == class)?;
    (!provider.is_empty()).then_some((provider, class))
}

fn key(provider: &str, class: TaskClass) -> String {
    format!("{provider}|{}", class.label())
}

/// True for HTTPS remotes / vendor names — the "ceiling" tier.
fn is_cloud(provider: &str) -> bool {
    super::routing::InferenceRouter::is_cloud_provider_name(provider)
}

fn class_prior(provider: &str, class: TaskClass) -> f32 {
    let cloud = is_cloud(provider);
    let bump = match (class, cloud) {
        (TaskClass::Code | TaskClass::Reasoning, true) => 0.05,
        (TaskClass::Reflex, false) => 0.05,
        _ => 0.0,
    };
    NEUTRAL_PRIOR + bump
}

/// Rank score in `[0, 1]`-ish; higher is better. Quality evidence first,
/// then a cost penalty scaled by task class and the budget dial.
fn score(provider: &str, class: TaskClass, rec: Option<&Record>, budget: Budget) -> f32 {
    let cost = cost::penalty(cost::tier_of(provider), class, budget);
    let prior = class_prior(provider, class);
    let Some(rec) = rec.filter(|r| r.samples() > 0) else {
        return prior - cost;
    };
    let n = rec.samples() as f32;
    let rate = (rec.ok as f32 + 1.0) / (n + 2.0);
    let w = n / (n + HALF_WEIGHT_SAMPLES);
    let mut s = (1.0 - w) * prior + w * rate;
    if class == TaskClass::Reflex && rec.ok > 0 {
        // Slow answers cost reflex-sized work; capped so latency never
        // outweighs reliability.
        s -= (rec.ema_ms / 20_000.0).min(0.25) * w;
    }
    s - cost
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ranked {
    pub provider: String,
    /// Failing right now with poor evidence: sorted behind every fit provider.
    pub unfit: bool,
    /// Kind and streak of the most recent failure inside the fitness window.
    pub last_failure: Option<(FailureKind, u32)>,
    pub score: f32,
    /// Marginal-cost tier that fed the score (`free`/`low`/`mid`/`high`).
    pub cost_tier: &'static str,
    pub samples: u32,
    pub success_rate: Option<f32>,
    pub avg_latency_ms: Option<u32>,
}

impl Store {
    pub fn record(&mut self, provider: &str, class: TaskClass, ok: bool, latency_ms: u64) {
        let rec = self.records.entry(key(provider, class)).or_default();
        if ok {
            rec.ok += 1;
            let ms = latency_ms as f32;
            rec.ema_ms = if rec.ok == 1 {
                ms
            } else {
                rec.ema_ms + LATENCY_EMA_ALPHA * (ms - rec.ema_ms)
            };
        } else {
            rec.fail += 1;
        }
        if rec.samples() > HISTORY_CAP {
            rec.ok /= 2;
            rec.fail /= 2;
        }
    }

    /// Note a failure under `key`; returns the consecutive count.
    pub fn note_failure(&mut self, key: &str, kind: FailureKind, now: u64) -> u32 {
        let h = self.health.entry(key.to_string()).or_insert(Health {
            kind,
            consecutive: 0,
            last_unix: now,
        });
        h.kind = kind;
        h.consecutive = h.consecutive.saturating_add(1);
        h.last_unix = now;
        h.consecutive
    }

    /// A success clears the streak (a working call proves the account/key).
    pub fn note_success(&mut self, key: &str) -> bool {
        self.health.remove(key).is_some()
    }

    fn recent_failure(&self, provider: &str, now: u64) -> Option<&Health> {
        let vendor = super::routing::vendor_scope(provider).map(|s| format!("vendor:{s}"));
        std::iter::once(provider.to_string())
            .chain(vendor)
            .filter_map(|k| self.health.get(&k))
            .filter(|h| now.saturating_sub(h.last_unix) <= QUARANTINE_CAP_SECS)
            .max_by_key(|h| h.last_unix)
    }

    /// A provider that keeps failing right now must not be the brain: with
    /// enough samples and a poor success rate *and* a recent failure it sorts
    /// behind every fit provider (and ahead of nothing, so a total outage
    /// still tries it). After the window it is probed again.
    pub fn is_unfit(&self, provider: &str, class: TaskClass, now: u64) -> bool {
        let poor = self.records.get(&key(provider, class)).is_some_and(|r| {
            r.samples() >= UNFIT_MIN_SAMPLES && (r.ok as f32 / r.samples() as f32) < UNFIT_RATE
        });
        poor && self.recent_failure(provider, now).is_some()
    }

    /// Fold a mission's verified outcome into the providers that answered it.
    /// Counts one sample per provider without touching latency. Returns
    /// whether anything was applied (a mission is applied at most once).
    pub fn apply_mission(&mut self, mission_id: &str, served: &[String], succeeded: bool) -> bool {
        if self.applied.iter().any(|id| id == mission_id) {
            return false;
        }
        let mut any = false;
        for entry in served {
            let Some((provider, class)) = parse_served(entry) else {
                continue;
            };
            let rec = self.records.entry(key(provider, class)).or_default();
            if succeeded {
                rec.ok += 1;
            } else {
                rec.fail += 1;
            }
            if rec.samples() > HISTORY_CAP {
                rec.ok /= 2;
                rec.fail /= 2;
            }
            any = true;
        }
        self.applied.push(mission_id.to_string());
        if self.applied.len() > APPLIED_CAP {
            self.applied.remove(0);
        }
        any
    }

    /// Best first. Ties keep the caller's order (stable), so the router's
    /// static preference remains the tiebreaker for providers with no data.
    pub fn rank(&self, providers: &[String], class: TaskClass) -> Vec<Ranked> {
        self.rank_with_budget(providers, class, Budget::from_env())
    }

    pub fn rank_with_budget(
        &self,
        providers: &[String],
        class: TaskClass,
        budget: Budget,
    ) -> Vec<Ranked> {
        let mut out: Vec<Ranked> = providers
            .iter()
            .map(|p| {
                let rec = self.records.get(&key(p, class));
                let samples = rec.map_or(0, Record::samples);
                Ranked {
                    provider: p.clone(),
                    unfit: self.is_unfit(p, class, unix_now()),
                    last_failure: self
                        .recent_failure(p, unix_now())
                        .map(|h| (h.kind, h.consecutive)),
                    score: score(p, class, rec, budget),
                    cost_tier: cost::tier_of(p).label(),
                    samples,
                    success_rate: rec
                        .filter(|r| r.samples() > 0)
                        .map(|r| r.ok as f32 / r.samples() as f32),
                    avg_latency_ms: rec.filter(|r| r.ok > 0).map(|r| r.ema_ms as u32),
                }
            })
            .collect();
        out.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    /// The best provider that is neither quarantined nor failing right now,
    /// or `None` when everything is (the caller then leaves routing alone and
    /// the local model stays the floor).
    pub fn best_healthy(
        &self,
        providers: &[String],
        class: TaskClass,
        now: u64,
        is_cooled: &dyn Fn(&str) -> bool,
    ) -> Option<String> {
        self.rank(providers, class)
            .into_iter()
            .find(|r| !is_cooled(&r.provider) && !self.is_unfit(&r.provider, class, now))
            .map(|r| r.provider)
    }

    /// Failure streaks (provider or `vendor:<scope>` keys), for operator views.
    pub fn failure_streaks(&self) -> Vec<(String, Health)> {
        self.health
            .iter()
            .map(|(k, h)| (k.clone(), h.clone()))
            .collect()
    }

    pub fn providers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .records
            .keys()
            .filter_map(|k| k.rsplit_once('|').map(|(p, _)| p.to_string()))
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// True when `provider` is failing right now (see [`Store::is_unfit`]).
pub fn is_unfit(provider: &str, class: TaskClass) -> bool {
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(load)
        .is_unfit(provider, class, unix_now())
}

/// [`Store::best_healthy`] over the persisted evidence.
pub fn best_healthy(
    providers: &[String],
    class: TaskClass,
    is_cooled: &dyn Fn(&str) -> bool,
) -> Option<String> {
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(load)
        .best_healthy(providers, class, unix_now(), is_cooled)
}

/// Record a failure streak entry and return its consecutive count.
pub fn note_failure(key: &str, kind: FailureKind) -> u32 {
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(load);
    let n = store.note_failure(key, kind, unix_now());
    persist(store);
    n
}

/// Clear failure streaks (success, or an operator repaired the account).
pub fn note_success(keys: &[String]) {
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(load);
    let mut changed = false;
    for k in keys {
        changed |= store.note_success(k);
    }
    if changed {
        persist(store);
    }
}

fn store_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SUSI_BRAIN_EVIDENCE_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    susi_paths::SusiDirs::config_dir().join("brain_evidence.json")
}

fn global() -> &'static Mutex<Option<Store>> {
    static STORE: Mutex<Option<Store>> = Mutex::new(None);
    &STORE
}

/// Evidence as persisted (fresh read; CLI and daemon share the file).
pub fn load() -> Store {
    // Under test, never seed the process-global store from the host's real
    // evidence file: routing assertions would then depend on the developer's
    // machine, and Mandate 52 forbids touching it. Tests that exercise
    // persistence point `SUSI_BRAIN_EVIDENCE_FILE` at their own file, exactly
    // as `persist` already requires to write.
    if cfg!(test) && std::env::var_os("SUSI_BRAIN_EVIDENCE_FILE").is_none() {
        return Store::default();
    }
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Record one live outcome and persist it.
pub fn record_outcome(provider: &str, class: TaskClass, ok: bool, latency_ms: u64) {
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(load);
    store.record(provider, class, ok, latency_ms);
    persist(store);
}

fn persist(store: &Store) {
    // Unit tests that drive live routing must not persist into the host's
    // real evidence; only an explicit file override writes under test.
    if cfg!(test) && std::env::var_os("SUSI_BRAIN_EVIDENCE_FILE").is_none() {
        return;
    }
    if let Ok(bytes) = serde_json::to_vec_pretty(store) {
        let _ = crate::susi_config::atomic_write_bytes(&store_path(), &bytes);
    }
}

/// Rank `providers` for `class` from persisted evidence.
pub fn rank(providers: &[String], class: TaskClass) -> Vec<Ranked> {
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    guard.get_or_insert_with(load).rank(providers, class)
}

/// Learn from the verified outcomes of this workspace's recent missions.
/// Governance blocks are not capability outcomes and are skipped. Cheap when
/// nothing is new: the trace file is only re-read when its mtime changes.
pub fn apply_mission_verdicts(workspace: &std::path::Path) {
    use std::time::SystemTime;
    static SEEN: Mutex<BTreeMap<PathBuf, Option<SystemTime>>> = Mutex::new(BTreeMap::new());
    let traces = workspace.join(".susi").join("mission_traces.jsonl");
    let mtime = std::fs::metadata(&traces).and_then(|m| m.modified()).ok();
    {
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if seen.get(workspace) == Some(&mtime) {
            return;
        }
        seen.insert(workspace.to_path_buf(), mtime);
    }
    let mut guard = global().lock().unwrap_or_else(|e| e.into_inner());
    let store = guard.get_or_insert_with(load);
    let mut changed = false;
    for trace in crate::susi_core::mission_trace::read_all(workspace)
        .iter()
        .rev()
        .take(APPLIED_CAP)
    {
        if trace.brain_served.is_empty()
            || !trace.capability_outcome()
            || trace.outcome == "BLOCKED"
        {
            continue;
        }
        changed |= store.apply_mission(&trace.mission_id, &trace.brain_served, trace.succeeded());
    }
    if changed {
        persist(store);
    }
}

/// Forget all evidence (a new account, a changed model lineup).
pub fn reset() -> std::io::Result<()> {
    *global().lock().unwrap_or_else(|e| e.into_inner()) = None;
    match std::fs::remove_file(store_path()) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn classification_is_deterministic_by_shape() {
        assert_eq!(TaskClass::classify("hi there"), TaskClass::Reflex);
        assert_eq!(
            TaskClass::classify("fix this bug in my function"),
            TaskClass::Code
        );
        assert_eq!(
            TaskClass::classify("```rust\nfn main(){}\n```"),
            TaskClass::Code
        );
        assert_eq!(
            TaskClass::classify("compare the trade-offs of these two designs"),
            TaskClass::Reasoning
        );
        assert_eq!(
            TaskClass::classify(
                "tell me about the history of the roman empire and its many emperors please"
            ),
            TaskClass::Chat
        );
        assert_eq!(
            TaskClass::classify(&"word ".repeat(200)),
            TaskClass::Reasoning
        );
    }

    #[test]
    fn no_evidence_keeps_the_callers_order_within_a_tier() {
        let s = Store::default();
        let r = s.rank(
            &names(&["ollama-a", "ollama-b", "ollama-c"]),
            TaskClass::Chat,
        );
        let order: Vec<_> = r.iter().map(|x| x.provider.as_str()).collect();
        assert_eq!(order, ["ollama-a", "ollama-b", "ollama-c"]);
    }

    #[test]
    fn hard_work_prefers_cloud_and_reflex_prefers_local_without_evidence() {
        let s = Store::default();
        let providers = names(&["ollama-llama", "openai-gpt-4o"]);
        assert_eq!(
            s.rank(&providers, TaskClass::Code)[0].provider,
            "openai-gpt-4o"
        );
        assert_eq!(
            s.rank(&providers, TaskClass::Reflex)[0].provider,
            "ollama-llama"
        );
    }

    #[test]
    fn a_failing_provider_sinks_below_an_untried_one() {
        let mut s = Store::default();
        for _ in 0..8 {
            s.record("openrouter-x", TaskClass::Code, false, 0);
        }
        let r = s.rank(&names(&["openrouter-x", "groq-y"]), TaskClass::Code);
        assert_eq!(r[0].provider, "groq-y");
        assert_eq!(r[1].success_rate, Some(0.0));
    }

    #[test]
    fn a_proven_provider_beats_an_untried_prior() {
        let mut s = Store::default();
        for _ in 0..10 {
            s.record("groq-y", TaskClass::Code, true, 400);
        }
        let r = s.rank(&names(&["openai-z", "groq-y"]), TaskClass::Code);
        assert_eq!(r[0].provider, "groq-y");
        assert_eq!(r[0].avg_latency_ms, Some(400));
    }

    #[test]
    fn a_few_samples_barely_move_the_prior() {
        let mut s = Store::default();
        s.record("groq-y", TaskClass::Chat, false, 0);
        let r = s.rank(&names(&["openai-z", "groq-y"]), TaskClass::Chat);
        // One failure is weak evidence: the untried provider leads but the
        // gap stays small, not a cliff.
        assert_eq!(r[0].provider, "openai-z");
        assert!(r[0].score - r[1].score < 0.15);
    }

    #[test]
    fn slowness_only_matters_for_reflex_work() {
        let mut s = Store::default();
        for _ in 0..10 {
            s.record("slow", TaskClass::Reflex, true, 20_000);
            s.record("slow", TaskClass::Code, true, 20_000);
            s.record("fast", TaskClass::Reflex, true, 200);
            s.record("fast", TaskClass::Code, true, 200);
        }
        let p = names(&["slow", "fast"]);
        assert_eq!(s.rank(&p, TaskClass::Reflex)[0].provider, "fast");
        let code = s.rank(&p, TaskClass::Code);
        assert!((code[0].score - code[1].score).abs() < 1e-6);
    }

    #[test]
    fn history_is_bounded_so_recovery_is_possible() {
        let mut s = Store::default();
        for _ in 0..1000 {
            s.record("p", TaskClass::Chat, false, 0);
        }
        let rec = s.records.get("p|chat").unwrap();
        assert!(rec.samples() <= HISTORY_CAP + 1);
        for _ in 0..HISTORY_CAP {
            s.record("p", TaskClass::Chat, true, 100);
        }
        assert!(
            s.rank(&names(&["p"]), TaskClass::Chat)[0]
                .success_rate
                .unwrap()
                > 0.4
        );
    }

    #[test]
    fn mission_verdicts_teach_the_brain_once() {
        let mut s = Store::default();
        let served = vec![receipt_tool("groq-y", TaskClass::Code)
            .strip_prefix("brain:")
            .unwrap()
            .to_string()];
        assert!(s.apply_mission("m1", &served, false));
        assert!(
            !s.apply_mission("m1", &served, false),
            "a mission counts once"
        );
        let r = &s.rank(&names(&["groq-y"]), TaskClass::Code)[0];
        assert_eq!((r.samples, r.success_rate), (1, Some(0.0)));
        assert_eq!(r.avg_latency_ms, None, "verdicts must not invent latency");
        assert!(s.apply_mission("m2", &served, true));
        assert_eq!(
            s.rank(&names(&["groq-y"]), TaskClass::Code)[0].success_rate,
            Some(0.5)
        );
    }

    #[test]
    fn malformed_served_entries_are_ignored_and_applied_ids_are_bounded() {
        let mut s = Store::default();
        assert!(!s.apply_mission(
            "m",
            &["nope".into(), "p|bogus".into(), "|chat".into()],
            true
        ));
        for i in 0..(APPLIED_CAP + 10) {
            s.apply_mission(&format!("m{i}"), &[], true);
        }
        assert_eq!(s.applied.len(), APPLIED_CAP);
    }

    #[test]
    fn a_failed_mission_lowers_the_provider_that_answered_it() {
        let dir = std::env::temp_dir().join(format!("susi-brain-join-{}", std::process::id()));
        let ws = dir.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let file = dir.join("e.json");
        let _g = crate::engines::env_test_lock();
        std::env::set_var("SUSI_BRAIN_EVIDENCE_FILE", &file);
        let _ = reset();

        let served = receipt_tool("groq-join", TaskClass::Code)
            .strip_prefix("brain:")
            .unwrap()
            .to_string();
        for (id, outcome) in [("j1", "FAILED"), ("j2", "FAILED"), ("j3", "BLOCKED")] {
            let mut t = crate::susi_core::mission_trace::MissionTrace::new(
                id,
                "goal",
                outcome,
                "fast-path",
            );
            t.brain_served = vec![served.clone()];
            t.emit(&ws).unwrap();
        }
        apply_mission_verdicts(&ws);
        apply_mission_verdicts(&ws); // idempotent: mtime unchanged, ids applied
        let r = load().rank(&names(&["groq-join"]), TaskClass::Code)[0].clone();
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
        assert_eq!(
            (r.samples, r.success_rate),
            (2, Some(0.0)),
            "two failed missions count; the governance block does not"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cheaper_provider_wins_chat_when_evidence_is_equal() {
        let s = Store::default();
        let p = names(&["anthropic-claude-opus-4", "openai-gpt-4o-mini"]);
        let r = s.rank_with_budget(&p, TaskClass::Chat, Budget::Balanced);
        assert_eq!(r[0].provider, "openai-gpt-4o-mini");
        assert_eq!((r[0].cost_tier, r[1].cost_tier), ("low", "high"));
    }

    #[test]
    fn hard_work_still_reaches_the_expensive_model_unless_budget_is_low() {
        let s = Store::default();
        let p = names(&["ollama-llama", "anthropic-claude-opus-4"]);
        let top = |b| {
            s.rank_with_budget(&p, TaskClass::Reasoning, b)[0]
                .provider
                .clone()
        };
        assert_eq!(top(Budget::Balanced), "anthropic-claude-opus-4");
        assert_eq!(top(Budget::Max), "anthropic-claude-opus-4");
        // Low budget doubles the (small) reasoning penalty but the cloud
        // prior still leads; code work is where a frugal budget flips it.
        let code = |b| {
            s.rank_with_budget(&p, TaskClass::Code, b)[0]
                .provider
                .clone()
        };
        assert_eq!(code(Budget::Balanced), "anthropic-claude-opus-4");
        assert_eq!(code(Budget::Low), "ollama-llama");
    }

    #[test]
    fn max_budget_ignores_cost_entirely() {
        let s = Store::default();
        let p = names(&["anthropic-claude-opus-4", "openai-gpt-4o-mini"]);
        let r = s.rank_with_budget(&p, TaskClass::Chat, Budget::Max);
        assert!((r[0].score - r[1].score).abs() < 1e-6);
    }

    #[test]
    fn evidence_outweighs_cost() {
        let mut s = Store::default();
        for _ in 0..30 {
            s.record("anthropic-claude-opus-4", TaskClass::Chat, true, 300);
            s.record("openai-gpt-4o-mini", TaskClass::Chat, false, 0);
        }
        let p = names(&["openai-gpt-4o-mini", "anthropic-claude-opus-4"]);
        assert_eq!(
            s.rank_with_budget(&p, TaskClass::Chat, Budget::Low)[0].provider,
            "anthropic-claude-opus-4",
            "a cheap provider that keeps failing must not win on price"
        );
    }

    #[test]
    fn errors_are_classified_with_funds_ahead_of_auth() {
        use FailureKind::*;
        for (e, want) in [
            ("Provider 'x': HTTP 402: Insufficient credits", Funds),
            ("Insufficient Balance (request_id: a)", Funds),
            ("HTTP 402 unauthorized", Funds),
            ("You exceeded your current quota", Funds),
            ("HTTP 401: invalid api key", Auth),
            ("HTTP 403 Forbidden", Auth),
            ("HTTP 429 Too Many Requests", RateLimit),
            ("HTTP 404 model not found", Gone),
            ("tcp connect error: connection refused", Transport),
            ("something odd happened", Other),
        ] {
            assert_eq!(classify_error(e), want, "{e}");
        }
    }

    #[test]
    fn only_operator_needed_failures_escalate_and_cap() {
        assert_eq!(quarantine_secs(FailureKind::Funds, 1), Some(600));
        assert_eq!(quarantine_secs(FailureKind::Funds, 2), Some(3_600));
        assert_eq!(
            quarantine_secs(FailureKind::Auth, 3),
            Some(QUARANTINE_CAP_SECS)
        );
        assert_eq!(
            quarantine_secs(FailureKind::Funds, 99),
            Some(QUARANTINE_CAP_SECS)
        );
        assert_eq!(quarantine_secs(FailureKind::RateLimit, 5), None);
        assert_eq!(quarantine_secs(FailureKind::Transport, 5), None);
    }

    #[test]
    fn failure_streaks_count_and_a_success_clears_them() {
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
        let mut s = Store::default();
        assert_eq!(s.note_failure("vendor:acme", FailureKind::Funds, 100), 1);
        assert_eq!(s.note_failure("vendor:acme", FailureKind::Funds, 200), 2);
        assert!(s.note_success("vendor:acme"));
        assert_eq!(s.note_failure("vendor:acme", FailureKind::Funds, 300), 1);
        assert!(!s.note_success("vendor:other"));
    }

    #[test]
    fn a_dry_provider_is_unfit_only_with_poor_evidence_and_a_recent_failure() {
        // The store is process-global and its path comes from a
        // process-global env var that other tests set and clear, so readers
        // take the same lock the writers do.
        let _env = crate::engines::env_test_lock();
        let mut s = Store::default();
        let p = "acme-big-model";
        let now = unix_now();
        // Poor evidence alone is not enough…
        for _ in 0..4 {
            s.record(p, TaskClass::Chat, false, 0);
        }
        assert!(!s.is_unfit(p, TaskClass::Chat, now));
        // …a recent credential-scope failure makes it unfit…
        s.note_failure("vendor:acme", FailureKind::Funds, now);
        assert!(s.is_unfit(p, TaskClass::Chat, now + 100));
        // …classes without evidence stay fit…
        assert!(!s.is_unfit(p, TaskClass::Code, now + 100));
        // …and after the window it is probed again.
        assert!(!s.is_unfit(p, TaskClass::Chat, now + QUARANTINE_CAP_SECS + 1));
        // A proven provider under the same vendor failure is not unfit.
        for _ in 0..10 {
            s.record("acme-good", TaskClass::Chat, true, 100);
        }
        assert!(!s.is_unfit("acme-good", TaskClass::Chat, now + 100));
        let r = s.rank_with_budget(&names(&[p, "acme-good"]), TaskClass::Chat, Budget::Max);
        let by = |n: &str| r.iter().find(|x| x.provider == n).unwrap();
        assert!(by(p).unfit);
        assert_eq!(by(p).last_failure, Some((FailureKind::Funds, 1)));
        assert!(!by("acme-good").unfit);
        assert_eq!(r[0].provider, "acme-good");
    }

    #[test]
    fn persistence_round_trips_through_the_evidence_file() {
        let dir = std::env::temp_dir().join(format!("susi-brain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("e.json");
        let _g = crate::engines::env_test_lock();
        std::env::set_var("SUSI_BRAIN_EVIDENCE_FILE", &file);
        let _ = reset();
        record_outcome("groq-y", TaskClass::Code, true, 300);
        record_outcome("groq-y", TaskClass::Code, false, 0);
        let loaded = load();
        std::env::remove_var("SUSI_BRAIN_EVIDENCE_FILE");
        assert_eq!(loaded.providers(), ["groq-y"]);
        let r = &loaded.rank(&names(&["groq-y"]), TaskClass::Code)[0];
        assert_eq!((r.samples, r.success_rate), (2, Some(0.5)));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
