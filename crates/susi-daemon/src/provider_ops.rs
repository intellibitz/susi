//! Providers, keys and the brain: what `susi openweight|frontier|openrouter`,
//! `susi models probe`, `susi keys list|prefer|check|models`, `susi brain`,
//! `susi benchmark` and `susi verify-download-agent` do, and what the MCP
//! `provider_*`, `keys_*`, `brain_*`, `benchmark_run` and
//! `verify_download_agent` tools do over the `daemon.providers` plane topic
//! (zero-UI plan B4 group 3). One implementation for both: the CLI calls
//! these functions and prints what they return.
//!
//! Secrets never cross: key status reports presence only, and setting or
//! removing a key stays local to the CLI and desktop (`tests/socket_parity.rs`
//! exempts `keys set` and `keys remove`).

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use susi_gemi::engines::brain::{self, TaskClass};
use susi_gemi::engines::cost::Budget;
use susi_gemi::models::frontier::{FrontierManager, FrontierOverride};
use susi_gemi::models::open_weight::{OpenWeightManager, OpenWeightOverride};
use susi_gemi::models::openrouter::OpenRouterManager;

type R<T> = Result<T, String>;

fn msg(error: impl std::fmt::Display) -> String {
    error.to_string()
}

/// The prompt every probe sends when the caller gives none.
pub const DEFAULT_PROBE_PROMPT: &str = "Reply with exactly: ok";

/// A provider control plane the CLI exposes as its own command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vendor {
    /// `susi openweight`: local open-weight models through Ollama or vLLM.
    OpenWeight,
    /// `susi frontier`: the top frontier models.
    Frontier,
    /// `susi openrouter`: the OpenRouter gateway.
    OpenRouter,
}

impl Vendor {
    /// Parse a vendor name (`openweight`, `frontier` or `openrouter`).
    pub fn parse(name: &str) -> R<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "openweight" | "open-weight" | "open_weight" => Ok(Self::OpenWeight),
            "frontier" => Ok(Self::Frontier),
            "openrouter" => Ok(Self::OpenRouter),
            other => Err(format!(
                "unknown provider vendor `{other}` (expected openweight, frontier or openrouter)"
            )),
        }
    }

    fn needs_model(self, action: &str) -> R<()> {
        if self == Self::OpenRouter {
            return Err(format!(
                "openrouter has no per-model {action}; its routes are pinned with provider_prefer"
            ));
        }
        Ok(())
    }
}

fn encode<T: serde::Serialize>(value: Result<T, susi_error::EaiError>) -> R<Value> {
    serde_json::to_value(value.map_err(msg)?).map_err(msg)
}

fn required<'a>(model: Option<&'a str>, what: &str) -> R<&'a str> {
    model
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| format!("{what} is required"))
}

// ── provider control planes ────────────────────────────────────────────────

/// `susi <vendor> list`: the curated models (routes) and their readiness.
pub fn list(vendor: Vendor) -> R<Value> {
    match vendor {
        Vendor::OpenWeight => {
            let manager = OpenWeightManager::new().map_err(msg)?;
            let preferred = manager.preferred();
            let rows: Vec<Value> = OpenWeightManager::catalog()
                .map_err(msg)?
                .into_iter()
                .map(|m| {
                    let effective = manager.effective(&m.id).unwrap_or(m.clone());
                    let readiness = manager.doctor(Some(&m.id));
                    let pulled = manager.model_pulled(&effective.ollama_tag).unwrap_or(false);
                    json!({
                        "model": effective,
                        "preferred": preferred.as_deref() == Some(m.id.as_str()),
                        "prerequisites_present": readiness.is_ok(),
                        "pulled": pulled,
                        "detail": match readiness { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect();
            Ok(json!({ "status": manager.status(), "models": rows }))
        }
        Vendor::Frontier => {
            let manager = FrontierManager::new().map_err(msg)?;
            let preferred = manager.preferred();
            let rows: Vec<Value> = FrontierManager::catalog()
                .map_err(msg)?
                .into_iter()
                .map(|m| {
                    let effective = manager.effective(&m.id).unwrap_or(m.clone());
                    let readiness = manager.preflight(&m.id);
                    json!({
                        "model": effective,
                        "preferred": preferred.as_deref() == Some(m.id.as_str()),
                        "prerequisites_present": readiness.is_ok(),
                        "detail": match readiness { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect();
            Ok(json!({ "status": manager.status(), "models": rows }))
        }
        Vendor::OpenRouter => {
            let manager = OpenRouterManager::new().map_err(msg)?;
            let routes = OpenRouterManager::catalog().map_err(msg)?;
            Ok(json!({ "status": manager.status(), "routes": routes }))
        }
    }
}

/// `susi <vendor> doctor`: the documents the CLI prints, in order. Frontier
/// checks one row per model; `live` (OpenRouter only) also lists a sample
/// of the live model ids, which proves the key works. No paid model is
/// called.
pub fn doctor(vendor: Vendor, model: Option<&str>, live: bool) -> R<Vec<Value>> {
    match vendor {
        Vendor::OpenWeight => {
            let manager = OpenWeightManager::new().map_err(msg)?;
            let detail = manager.doctor(model).map_err(msg)?;
            Ok(vec![json!({
                "vendor": "open-weight",
                "prerequisites_present": true,
                "detail": detail,
            })])
        }
        Vendor::Frontier => {
            let manager = FrontierManager::new().map_err(msg)?;
            let models = match model {
                Some(id) => vec![FrontierManager::definition(id).map_err(msg)?],
                None => FrontierManager::catalog().map_err(msg)?,
            };
            Ok(models
                .into_iter()
                .map(|m| {
                    let result = manager.preflight(&m.id);
                    json!({
                        "model": m.id,
                        "prerequisites_present": result.is_ok(),
                        "detail": match result { Ok(s) => s, Err(e) => e.to_string() }
                    })
                })
                .collect())
        }
        Vendor::OpenRouter => {
            let manager = OpenRouterManager::new().map_err(msg)?;
            let detail = manager.doctor().map_err(msg)?;
            let mut docs = vec![json!({
                "vendor": "openrouter",
                "prerequisites_present": true,
                "detail": detail,
            })];
            if live {
                let ids = susi_gemi::openrouter_ext::list_live(5).map_err(msg)?;
                docs.push(json!({
                    "live_models_sample": ids,
                    "detail": "OpenRouter /models reachable"
                }));
            }
            Ok(docs)
        }
    }
}

/// Whether every doctor document reports its prerequisites present.
pub fn all_ready(docs: &[Value]) -> bool {
    docs.iter()
        .all(|d| d.get("prerequisites_present").and_then(Value::as_bool) != Some(false))
}

/// `susi <vendor> setup`: setup instructions and the effective wiring.
pub fn setup(vendor: Vendor, model: Option<&str>) -> R<Value> {
    match vendor {
        Vendor::OpenWeight => {
            let model = required(model, "model")?;
            OpenWeightManager::new()
                .and_then(|m| m.setup(model))
                .map_err(msg)
        }
        Vendor::Frontier => {
            let model = required(model, "model")?;
            FrontierManager::new()
                .and_then(|m| m.setup(model))
                .map_err(msg)
        }
        Vendor::OpenRouter => Ok(OpenRouterManager::new().map_err(msg)?.setup()),
    }
}

/// `susi <vendor> configure <model> <file>`: store a host override (the
/// override document is the file's JSON).
pub fn configure(vendor: Vendor, model: &str, over: Value) -> R<Value> {
    vendor.needs_model("configure")?;
    match vendor {
        Vendor::OpenWeight => {
            let over: OpenWeightOverride =
                serde_json::from_value(over).map_err(|e| format!("invalid override: {e}"))?;
            encode(OpenWeightManager::new().and_then(|m| m.configure(model, &over)))
        }
        Vendor::Frontier => {
            let over: FrontierOverride =
                serde_json::from_value(over).map_err(|e| format!("invalid override: {e}"))?;
            encode(FrontierManager::new().and_then(|m| m.configure(model, &over)))
        }
        Vendor::OpenRouter => Err("openrouter has no per-model configure".to_string()),
    }
}

/// `susi <vendor> reset <model>`: drop the host override; returns the
/// bundled entry now in effect.
pub fn reset(vendor: Vendor, model: &str) -> R<Value> {
    vendor.needs_model("reset")?;
    match vendor {
        Vendor::OpenWeight => encode(OpenWeightManager::new().and_then(|m| m.reset(model))),
        Vendor::Frontier => encode(FrontierManager::new().and_then(|m| m.reset(model))),
        Vendor::OpenRouter => Err("openrouter has no per-model reset".to_string()),
    }
}

/// `susi openweight pull <model>`: pull the weights through `ollama pull`.
pub fn pull(model: &str) -> R<String> {
    OpenWeightManager::new()
        .and_then(|m| m.pull(model))
        .map_err(msg)
}

/// `susi <vendor> prefer [model] [--clear]` (OpenRouter: `--clear-model`).
pub fn prefer(vendor: Vendor, model: Option<&str>, clear: bool) -> R<String> {
    let model = model.map(str::trim).filter(|m| !m.is_empty());
    if vendor == Vendor::OpenRouter {
        return susi_gemi::openrouter_ext::prefer(model, clear).map_err(msg);
    }
    let set = if vendor == Vendor::OpenWeight {
        susi_gemi::open_weight_ext::prefer
    } else {
        susi_gemi::frontier_ext::prefer
    };
    if clear {
        return set("", true).map_err(msg);
    }
    let model = model.ok_or("model id required (or pass --clear)")?;
    set(model, false).map_err(msg)
}

/// `susi <vendor> probe` and `susi models probe` (`target == "coding"`): a
/// short completion through the model's endpoint. Cloud probes are paid.
pub fn probe(target: &str, model: Option<&str>, prompt: &str) -> R<Value> {
    if target.trim().eq_ignore_ascii_case("coding") {
        let model = required(model, "model")?;
        let output = susi_gemi::coding_models_ext::probe(model, prompt).map_err(msg)?;
        return Ok(json!({ "model": model, "output": output }));
    }
    match Vendor::parse(target)? {
        Vendor::OpenWeight => {
            let model = required(model, "model")?;
            let output = susi_gemi::open_weight_ext::probe(model, prompt).map_err(msg)?;
            let def = OpenWeightManager::new()
                .and_then(|m| m.effective(model))
                .map_err(msg)?;
            Ok(json!({ "model": def.id, "ollama_tag": def.ollama_tag, "output": output }))
        }
        Vendor::Frontier => {
            let model = required(model, "model")?;
            let output = susi_gemi::frontier_ext::probe(model, prompt).map_err(msg)?;
            let def = FrontierManager::new()
                .and_then(|m| m.effective(model))
                .map_err(msg)?;
            Ok(json!({
                "model": def.id,
                "api_id": def.model,
                "engine": def.engine,
                "output": output,
            }))
        }
        Vendor::OpenRouter => {
            let output = susi_gemi::openrouter_ext::probe(prompt).map_err(msg)?;
            let manager = OpenRouterManager::new().map_err(msg)?;
            Ok(json!({
                "vendor": "openrouter",
                "model": manager.effective_model(),
                "output": output,
            }))
        }
    }
}

/// `susi openrouter models [--live] [--limit]`: curated routes, or the live
/// model ids the key can reach.
pub fn openrouter_models(live: bool, limit: usize) -> R<Value> {
    if live {
        let ids = susi_gemi::openrouter_ext::list_live(limit).map_err(msg)?;
        return Ok(json!({ "source": "live", "count": ids.len(), "models": ids }));
    }
    serde_json::to_value(OpenRouterManager::catalog().map_err(msg)?).map_err(msg)
}

// ── keys (presence only) ───────────────────────────────────────────────────

/// `susi keys list`: which vendor keys are set (never their values), the
/// routing preference and the file keys are stored in.
pub fn keys_status() -> Value {
    let keys: Vec<Value> = susi_gemi::engines::http_provider::list_api_key_status()
        .into_iter()
        .map(|(vendor, env, present)| json!({ "vendor": vendor, "env": env, "present": present }))
        .collect();
    let vault = susi_config::vault_store::vault_status(&susi_paths::SusiDirs::config_dir());
    json!({
        "keys": keys,
        "preference": susi_gemi::engines::routing::InferenceRouter::preference_status(),
        "cloud_env_path": susi_gemi::engines::http_provider::cloud_env_path(),
        "vault": vault,
    })
}

/// `susi keys prefer [vendor] [--clear]`: pin, clear or report the
/// preferred cloud vendor.
pub fn keys_prefer(vendor: Option<&str>, clear: bool) -> R<String> {
    use susi_gemi::engines::routing::InferenceRouter;
    if clear {
        return InferenceRouter::clear_preferred_cloud();
    }
    match vendor.map(str::trim).filter(|v| !v.is_empty()) {
        Some(vendor) => InferenceRouter::set_preferred_cloud(vendor),
        None => Ok(InferenceRouter::preference_status()),
    }
}

/// `susi keys check [vendor]`: each key's state against the vendor's model
/// list (one unpaid list call per vendor).
pub fn keys_check(vendor: Option<&str>) -> R<Vec<susi_gemi::models::cloud_manage::KeyCheck>> {
    susi_gemi::models::cloud_manage::check(vendor)
}

/// Whether any checked key was rejected (the CLI exits 2 on it).
pub fn any_key_rejected(rows: &[susi_gemi::models::cloud_manage::KeyCheck]) -> bool {
    rows.iter()
        .any(|r| matches!(r.state, susi_gemi::models::cloud_manage::KeyState::Rejected))
}

/// `susi keys models <vendor>`: the models a valid key reaches.
pub fn keys_models(vendor: &str) -> R<Value> {
    let rows = susi_gemi::models::cloud_manage::check(Some(vendor))?;
    let mut out = Vec::new();
    for r in rows {
        if r.state != susi_gemi::models::cloud_manage::KeyState::Valid {
            return Err(format!(
                "{}: key state {:?}; no model list",
                r.vendor, r.state
            ));
        }
        out.push(json!({
            "vendor": r.vendor,
            "model_count": r.model_count,
            "models": r.models,
        }));
    }
    Ok(Value::Array(out))
}

// ── brain ──────────────────────────────────────────────────────────────────

/// `susi brain status --json`: evidence-ranked providers per task class.
pub fn brain_status(top: usize, class: Option<&str>) -> Value {
    let store = brain::load();
    let providers = store.providers();
    let pref = susi_gemi::engines::routing::InferenceRouter::load_preference();
    let cooled_providers = susi_gemi::engines::routing::InferenceRouter::cooled_providers();
    let mut classes = serde_json::Map::new();
    for c in TaskClass::ALL {
        if class.is_some_and(|target| !c.label().eq_ignore_ascii_case(target)) {
            continue;
        }
        let mut ranked = store.rank(&providers, c);
        ranked.truncate(top);
        classes.insert(
            c.label().to_string(),
            serde_json::to_value(ranked).unwrap_or_default(),
        );
    }
    json!({
        "principle": "local is the floor, cloud is the ceiling; evidence beats priors",
        "budget": Budget::from_env().label(),
        "failure_streaks": store
            .failure_streaks()
            .into_iter()
            .map(|(key, h)| json!({
                "key": key,
                "kind": h.kind,
                "consecutive": h.consecutive,
                "last_unix": h.last_unix,
            }))
            .collect::<Vec<_>>(),
        "providers_with_evidence": providers,
        "ranking_by_task_class": classes,
        "preferred_cloud": pref.preferred_cloud,
        "policy_override": pref.policy_override,
        "auto_switched_from": pref.auto_switched_from,
        "cooled_providers": cooled_providers,
    })
}

/// `susi brain budget` flags.
#[derive(Debug, Default, Clone)]
pub struct BudgetUpdate {
    /// Budget dial: low, balanced or max.
    pub level: Option<String>,
    /// Free inference only (dial low).
    pub free_only: bool,
    /// Authorize paid inference within the limits below.
    pub allow_paid: bool,
    pub max_spend_microusd: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_requests: Option<u64>,
    pub max_parallel: Option<u64>,
    /// Start a new accounting window.
    pub new_window: bool,
}

fn cloud_budget_file_path() -> PathBuf {
    susi_paths::SusiDirs::config_dir().join("cloud_budget.json")
}

fn unix_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn upsert_cloud_budget_env(budget_level: &str) -> R<()> {
    let path = susi_paths::SusiDirs::config_dir().join("cloud.env");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = Vec::new();
    let mut replaced = false;
    for raw in existing.lines() {
        let trimmed = raw.trim();
        let check = trimmed.strip_prefix("export ").unwrap_or(trimmed);
        if let Some((k, _)) = check.split_once('=')
            && k.trim().eq_ignore_ascii_case("SUSI_BUDGET")
        {
            lines.push(format!("SUSI_BUDGET={budget_level}"));
            replaced = true;
            continue;
        }
        lines.push(raw.to_string());
    }
    if !replaced {
        lines.push(format!("SUSI_BUDGET={budget_level}"));
    }
    let body = format!("{}\n", lines.join("\n"));
    susi_config::atomic_write_bytes(&path, body.as_bytes()).map_err(msg)
}

/// `susi brain budget`: set the dial and the shared spend policy, or report
/// them when nothing is set. `lines` are what the CLI prints.
pub fn brain_budget(update: &BudgetUpdate) -> R<Value> {
    if update.free_only && update.allow_paid {
        return Err("free_only and allow_paid conflict".to_string());
    }
    let mut lines = Vec::new();
    if let Some(level) = &update.level {
        let parsed = Budget::parse(level)
            .ok_or_else(|| format!("Invalid budget level '{level}'. Choose: low, balanced, max"))?;
        upsert_cloud_budget_env(parsed.label())?;
        lines.push(format!("Set budget dial to '{}'", parsed.label()));
    }
    let path = cloud_budget_file_path();
    if update.free_only {
        upsert_cloud_budget_env("low")?;
        let body = json!({ "policy": "free_only", "updated_at": unix_now_secs() });
        susi_config::atomic_write_bytes(&path, &serde_json::to_vec_pretty(&body).map_err(msg)?)
            .map_err(msg)?;
        lines.push(format!(
            "Budget configured: policy=free_only, budget_dial=low (saved to {})",
            path.display()
        ));
    } else if update.allow_paid {
        let spend_cap = update.max_spend_microusd.unwrap_or(10_000_000);
        let body = json!({
            "policy": "paid_authorized",
            "max_spend_microusd": spend_cap,
            "max_tokens": update.max_tokens,
            "max_requests": update.max_requests,
            "max_parallel": update.max_parallel,
            "new_window": update.new_window,
            "updated_at": unix_now_secs(),
        });
        susi_config::atomic_write_bytes(&path, &serde_json::to_vec_pretty(&body).map_err(msg)?)
            .map_err(msg)?;
        lines.push(format!(
            "Budget configured: policy=paid_authorized, max_spend_microusd={spend_cap} (saved to {})",
            path.display()
        ));
    } else if update.level.is_none() {
        let policy = if path.exists() {
            std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".into())
        } else {
            "{\"policy\": \"default\"}".into()
        };
        lines.push(format!("Budget dial: {}", Budget::from_env().label()));
        lines.push(format!("Ledger policy: {}", policy.trim()));
    }
    let policy: Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_else(|| json!({ "policy": "default" }));
    Ok(json!({
        "budget_dial": Budget::from_env().label(),
        "ledger_policy": policy,
        "lines": lines,
    }))
}

/// `susi brain classify <prompt>`: the task class a prompt routes as.
pub fn brain_classify(prompt: &str) -> Value {
    json!({ "task_class": TaskClass::classify(prompt).label() })
}

/// `susi brain explain <prompt>`: the routing decision for a prompt.
pub fn brain_explain(prompt: &str) -> R<Value> {
    if prompt.trim().is_empty() {
        return Err("prompt cannot be empty".to_string());
    }
    serde_json::to_value(brain::explain(prompt)).map_err(msg)
}

/// `susi brain export [--out]`: the evidence bundle, or (with `out`) the
/// bundle written to `out` and a summary.
pub fn brain_export(out: Option<&Path>) -> R<Value> {
    let bundle = brain::export();
    let Some(path) = out else {
        return serde_json::to_value(bundle).map_err(msg);
    };
    let bytes = serde_json::to_vec_pretty(&bundle).map_err(msg)?;
    susi_config::atomic_write_bytes(path, &bytes).map_err(msg)?;
    Ok(json!({
        "exported": bundle.entries.len(),
        "out": path,
        "message": format!("Exported {} evidence entries to {}", bundle.entries.len(), path.display()),
    }))
}

/// `susi brain import <file>`: merge an evidence bundle.
pub fn brain_import(file: &Path) -> R<Value> {
    let content = std::fs::read_to_string(file)
        .map_err(|e| format!("Failed to read {}: {e}", file.display()))?;
    let bundle: susi_gemi::brain_export::BrainBundle = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse bundle from {}: {e}", file.display()))?;
    let count = brain::import(bundle).map_err(|e| format!("Import failed: {e}"))?;
    Ok(json!({
        "imported": count,
        "file": file,
        "message": format!(
            "Successfully imported and merged {count} evidence entries from {}",
            file.display()
        ),
    }))
}

/// `susi brain matrix`: the measured capability matrix.
pub fn brain_matrix() -> R<Value> {
    serde_json::to_value(susi_gemi::capability_matrix::build()).map_err(msg)
}

/// `susi brain pareto`: the 3D Pareto frontier, Wilson 95% lower confidence
/// bounds and Bayesian UCB1 indices per task class (`class` filters to one,
/// case-insensitively). Providers come from recorded outcomes, else the
/// capability registry.
pub fn brain_pareto(class: Option<&str>) -> Value {
    let store = brain::load();
    let mut providers = store.providers();
    if providers.is_empty() {
        providers = susi_core::registry::CapabilityRegistry::global().list_providers();
    }
    let mut frontiers = serde_json::Map::new();
    for c in TaskClass::ALL {
        if let Some(target) = class
            && !c.label().eq_ignore_ascii_case(target)
        {
            continue;
        }
        let candidates = store.pareto_frontier(&providers, c);
        let optimal: Vec<String> = candidates
            .iter()
            .filter(|x| x.is_pareto_optimal)
            .map(|x| x.provider.clone())
            .collect();
        let ucb1_leader = candidates
            .iter()
            .max_by(|a, b| {
                a.ucb1_index
                    .partial_cmp(&b.ucb1_index)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|x| x.provider.clone());
        let scores: Vec<f64> = candidates.iter().map(|x| x.score).collect();
        frontiers.insert(
            c.label().to_string(),
            json!({
                "pareto_frontier": optimal,
                "ucb1_leader": ucb1_leader,
                "shannon_entropy_bits": susi_gemi::brain_explain::routing_entropy_bits(&scores),
                "candidates": candidates,
            }),
        );
    }
    json!({
        "budget": Budget::from_env().label(),
        "classes": frontiers,
    })
}

/// `susi brain cache`: the measured prompt-cache hit rate.
pub fn brain_cache() -> Value {
    json!({
        "report": susi_gemi::context_assembly::cache_report(),
        "estimated": true,
        "journal": susi_gemi::context_assembly::default_journal_path(),
    })
}

/// `susi brain reset`: forget every recorded outcome.
pub fn brain_reset() -> R<Value> {
    brain::reset().map_err(msg)?;
    Ok(json!({ "reset": true, "message": "brain evidence cleared" }))
}

/// `susi brain bench [--out]`: the fixed prompt set against the fake engine,
/// recorded as JSONL evidence.
pub fn brain_bench(out: Option<PathBuf>) -> R<Value> {
    use std::time::Duration;
    use susi_gemi::benchmark::BENCHMARK_PROMPTS;
    use susi_gemi::engine_benchmark::{FakeProvider, run_engine_benchmark, write_bench_evidence};
    let fake = FakeProvider {
        engine: "fake-engine".into(),
        model: "fake-model".into(),
        reply: "benchmark reply tokens one two three".into(),
        latency: Duration::from_millis(5),
        fail: false,
    };
    let samples = run_engine_benchmark(&fake, BENCHMARK_PROMPTS);
    let path = out.unwrap_or_else(|| susi_paths::SusiDirs::config_dir().join("brain_bench.jsonl"));
    write_bench_evidence(&path, &samples).map_err(msg)?;
    Ok(json!({ "evidence_path": path, "samples": samples }))
}

/// `susi brain probe <provider> [--no-record]`: the scout benchmark against
/// a registered provider, recorded as evidence unless `no_record`.
pub fn brain_probe(provider: &str, no_record: bool) -> R<Value> {
    let report = susi_gemi::scout_probe::probe_registered(provider)?;
    let recorded = if no_record {
        0
    } else {
        susi_gemi::scout_probe::record_outcomes(&report)
    };
    Ok(json!({ "report": report, "outcomes_recorded": recorded }))
}

/// `susi benchmark`: real local (and configured cloud) inference latency.
pub fn benchmark(workspace: &Path) -> String {
    susi_gemi::benchmark::BenchmarkRunner::run_and_render(workspace)
}

/// `susi verify-download-agent`: network status and the hardware-selected
/// model ladder's provisioning steps; `all_verified` is the CLI's verdict.
/// Verification only: Susi never downloads a model by itself.
pub fn verify_download_agent(workspace: &Path) -> R<Value> {
    let report = susi_gemi::models::ModelManager::verify_model_ladder(workspace).map_err(msg)?;
    let all_verified =
        !report.steps.is_empty() && report.steps.iter().all(|s| s.status.contains("COMPLETED"));
    let mut doc = serde_json::to_value(&report).map_err(msg)?;
    if let Some(map) = doc.as_object_mut() {
        map.insert("all_verified".into(), json!(all_verified));
    }
    Ok(doc)
}

// ── socket dispatch ────────────────────────────────────────────────────────

fn field<'a>(payload: &'a Value, key: &str) -> Option<&'a str> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn flag(payload: &Value, key: &str) -> bool {
    payload.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number(payload: &Value, key: &str) -> Option<u64> {
    payload.get(key).and_then(Value::as_u64)
}

/// An absolute path argument: the socket has no caller working directory.
fn absolute(payload: &Value, key: &str) -> R<Option<PathBuf>> {
    let Some(text) = field(payload, key) else {
        return Ok(None);
    };
    let path = PathBuf::from(text);
    if !path.is_absolute() {
        return Err(format!(
            "{key} must be an absolute path (the socket has no caller working directory)"
        ));
    }
    Ok(Some(path))
}

fn vendor(payload: &Value) -> R<Vendor> {
    Vendor::parse(field(payload, "vendor").ok_or("vendor is required")?)
}

fn text(value: R<String>, key: &str) -> R<Value> {
    value.map(|s| json!({ key: s }))
}

/// Dispatch one `daemon.providers` request: `{action, ..}`. `workspace` is
/// the server workspace for the workspace-scoped actions.
pub fn dispatch(payload: &Value) -> R<Value> {
    let workspace = field(payload, "workspace")
        .map(PathBuf::from)
        .unwrap_or_else(susi_paths::SusiDirs::substrate_home);
    match field(payload, "action").ok_or("action is required")? {
        "list" => list(vendor(payload)?),
        "doctor" => doctor(
            vendor(payload)?,
            field(payload, "model"),
            flag(payload, "live"),
        )
        .map(Value::Array),
        "setup" => setup(vendor(payload)?, field(payload, "model")),
        "configure" => {
            let vendor = vendor(payload)?;
            let model = field(payload, "model").ok_or("model is required")?;
            let over = match (payload.get("override"), absolute(payload, "file")?) {
                (Some(over), None) if over.is_object() => over.clone(),
                (None, Some(file)) => {
                    let bytes = std::fs::read(&file)
                        .map_err(|e| format!("read {}: {e}", file.display()))?;
                    serde_json::from_slice(&bytes)
                        .map_err(|e| format!("{} is not JSON: {e}", file.display()))?
                }
                _ => return Err("pass exactly one of override (object) or file".to_string()),
            };
            configure(vendor, model, over)
        }
        "reset" => reset(
            vendor(payload)?,
            field(payload, "model").ok_or("model is required")?,
        ),
        "pull" => text(
            pull(field(payload, "model").ok_or("model is required")?),
            "message",
        ),
        "prefer" => text(
            prefer(
                vendor(payload)?,
                field(payload, "model"),
                flag(payload, "clear"),
            ),
            "message",
        ),
        "probe" => probe(
            field(payload, "vendor").ok_or("vendor is required")?,
            field(payload, "model"),
            field(payload, "prompt").unwrap_or(DEFAULT_PROBE_PROMPT),
        ),
        "openrouter_models" => openrouter_models(
            flag(payload, "live"),
            number(payload, "limit").map_or(50, |n| n as usize),
        ),
        "keys_status" => Ok(keys_status()),
        "keys_prefer" => text(
            keys_prefer(field(payload, "vendor"), flag(payload, "clear")),
            "message",
        ),
        "keys_check" => {
            let rows = keys_check(field(payload, "vendor"))?;
            Ok(json!({ "rows": rows, "any_rejected": any_key_rejected(&rows) }))
        }
        "keys_models" => keys_models(field(payload, "vendor").ok_or("vendor is required")?),
        "brain_status" => Ok(brain_status(
            number(payload, "top").map_or(5, |n| n as usize),
            field(payload, "class"),
        )),
        "brain_budget" => brain_budget(&BudgetUpdate {
            level: field(payload, "level").map(str::to_string),
            free_only: flag(payload, "free_only"),
            allow_paid: flag(payload, "allow_paid"),
            max_spend_microusd: number(payload, "max_spend_microusd"),
            max_tokens: number(payload, "max_tokens"),
            max_requests: number(payload, "max_requests"),
            max_parallel: number(payload, "max_parallel"),
            new_window: flag(payload, "new_window"),
        }),
        "brain_classify" => Ok(brain_classify(
            field(payload, "prompt").ok_or("prompt is required")?,
        )),
        "brain_explain" => brain_explain(field(payload, "prompt").unwrap_or("")),
        "brain_export" => brain_export(absolute(payload, "out")?.as_deref()),
        "brain_import" => brain_import(&absolute(payload, "file")?.ok_or("file is required")?),
        "brain_matrix" => brain_matrix(),
        "brain_cache" => Ok(brain_cache()),
        "brain_reset" => brain_reset(),
        "brain_bench" => brain_bench(absolute(payload, "out")?),
        "brain_probe" => brain_probe(
            field(payload, "provider").ok_or("provider is required")?,
            flag(payload, "no_record"),
        ),
        "benchmark" => Ok(json!({ "report": benchmark(&workspace) })),
        "verify_download_agent" => verify_download_agent(&workspace),
        other => Err(format!("unknown providers action `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendors_parse_and_refuse_unknown_names() {
        assert_eq!(Vendor::parse("Open-Weight").unwrap(), Vendor::OpenWeight);
        assert_eq!(Vendor::parse("frontier").unwrap(), Vendor::Frontier);
        assert_eq!(Vendor::parse(" openrouter ").unwrap(), Vendor::OpenRouter);
        assert!(
            Vendor::parse("acme")
                .unwrap_err()
                .contains("unknown provider")
        );
    }

    #[test]
    fn dispatch_validates_arguments_before_touching_state() {
        let refused = |payload: Value| dispatch(&payload).unwrap_err();
        assert!(refused(json!({})).contains("action"));
        assert!(refused(json!({"action": "nope"})).contains("unknown providers action"));
        assert!(refused(json!({"action": "list"})).contains("vendor"));
        assert!(refused(json!({"action": "reset", "vendor": "frontier"})).contains("model"));
        assert!(
            refused(json!({"action": "configure", "vendor": "frontier", "model": "m"}))
                .contains("exactly one")
        );
        assert!(
            refused(
                json!({"action": "configure", "vendor": "openrouter", "model": "m",
                "override": {}})
            )
            .contains("no per-model")
        );
        assert!(
            refused(json!({"action": "brain_import", "file": "rel.json"})).contains("absolute")
        );
        assert!(refused(json!({"action": "brain_export", "out": "rel.json"})).contains("absolute"));
        assert!(refused(json!({"action": "brain_explain"})).contains("empty"));
        assert!(refused(json!({"action": "keys_models"})).contains("vendor"));
        assert!(refused(json!({"action": "prefer", "vendor": "frontier"})).contains("model id"));
        assert!(
            refused(json!({"action": "brain_budget", "free_only": true, "allow_paid": true}))
                .contains("conflict")
        );
    }

    #[test]
    fn classify_and_doctor_helpers_report_without_secrets() {
        assert!(brain_classify("fix this rust compile error")["task_class"].is_string());
        assert!(all_ready(&[
            json!({"prerequisites_present": true}),
            json!({"detail": "x"})
        ]));
        assert!(!all_ready(&[json!({"prerequisites_present": false})]));
        let status = keys_status();
        for row in status["keys"].as_array().unwrap() {
            assert!(row["present"].is_boolean());
            assert_eq!(row.as_object().unwrap().len(), 3, "presence only: {row}");
        }
    }

    #[test]
    fn all_ready_empty_and_any_key_rejected_evaluate_deterministically() {
        assert!(all_ready(&[]));
        assert!(!any_key_rejected(&[]));
        let classified = brain_classify("prove formal theorem about prime numbers");
        assert!(
            classified["task_class"]
                .as_str()
                .is_some_and(|s| !s.is_empty())
        );
    }

    #[test]
    fn openrouter_models_offline_and_vendor_needs_model_check() {
        assert!(Vendor::OpenRouter.needs_model("reset").is_err());
        assert!(Vendor::Frontier.needs_model("reset").is_ok());
        assert!(Vendor::OpenWeight.needs_model("configure").is_ok());
        let catalog = openrouter_models(false, 5).expect("offline openrouter catalog");
        assert!(catalog.is_array() || catalog.is_object());
    }
}
