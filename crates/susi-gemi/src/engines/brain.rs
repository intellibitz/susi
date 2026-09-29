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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Store {
    /// Key: `<provider>|<class>`.
    records: BTreeMap<String, Record>,
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

/// Rank score in `[0, 1]`-ish; higher is better.
fn score(provider: &str, class: TaskClass, rec: Option<&Record>) -> f32 {
    let prior = class_prior(provider, class);
    let Some(rec) = rec.filter(|r| r.samples() > 0) else {
        return prior;
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
    s
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ranked {
    pub provider: String,
    pub score: f32,
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

    /// Best first. Ties keep the caller's order (stable), so the router's
    /// static preference remains the tiebreaker for providers with no data.
    pub fn rank(&self, providers: &[String], class: TaskClass) -> Vec<Ranked> {
        let mut out: Vec<Ranked> = providers
            .iter()
            .map(|p| {
                let rec = self.records.get(&key(p, class));
                let samples = rec.map_or(0, Record::samples);
                Ranked {
                    provider: p.clone(),
                    score: score(p, class, rec),
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
