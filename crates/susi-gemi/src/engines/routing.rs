//! Latency-aware inference routing: escalate slow local models to cloud,
//! remember sticky preferences, and ask once when multiple clouds are live.

use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::susi_sandbox::manager::{InferenceRoutingConfig, SusiConfig};

/// Sticky user choice under `~/.susi/routing_preference.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RoutingPreference {
    /// Optional override: `local_only` | `cloud_first` | `auto` | `ask`
    pub policy_override: Option<String>,
    /// Provider name (or substring) to prefer, e.g. `googlegemini-gemini-3.6-flash`
    pub preferred_cloud: Option<String>,
    /// When set in the future, force local until then (escape hatch).
    pub force_local_until_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LocalInferenceStats {
    /// Model the EMA was measured against. A different active model must
    /// not inherit another model's slowness (e.g. CPU-spill 32B → GPU 7B).
    pub model_id: String,
    pub ema_tokens_per_sec: f32,
    pub last_latency_ms: u64,
    pub samples: u64,
}

#[derive(Debug, Clone)]
pub struct CloudEscalation {
    pub provider: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderCooldown {
    pub provider: String,
    pub until_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlacementContract {
    pub schema: String,
    pub decision_id: String,
    pub decided_at_unix: u64,
}

impl PlacementContract {
    fn now() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let decided_at_unix = now_unix();
        Self {
            schema: "susi/placement/v1".to_string(),
            decision_id: format!(
                "placement-{}-{decided_at_unix}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            decided_at_unix,
        }
    }
}

/// Explainable local/cloud placement chosen by the inference router.
///
/// This is the single operator-facing view of the same decision used by the
/// live runtime; callers must not recreate routing policy from config fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementDecision {
    pub contract: PlacementContract,
    pub target: String,
    pub provider: Option<String>,
    pub reason: String,
    pub policy: String,
    pub local_model: Option<String>,
    pub local_ready: bool,
    pub local_stats: LocalInferenceStats,
    pub cloud_candidates: Vec<String>,
    pub cooled_candidates: Vec<ProviderCooldown>,
    pub requires: Option<String>,
    pub max_cost: Option<f64>,
    pub allow_cloud: bool,
}

pub struct InferenceRouter;

/// A provider that just failed is skipped for this long — dead endpoints
/// (e.g. an absent Ollama) otherwise burn a connect timeout on every
/// mission and spam the error sink.
const PROVIDER_DOWN_COOLDOWN_SECS: u64 = 120;

/// Model ID retired or never served (HTTP 404 / "No endpoints found" /
/// "no longer available"). Longer than the generic per-provider cool:
/// a stale catalog entry will not revive without an operator bump.
const MODEL_GONE_COOLDOWN_SECS: u64 = 86_400;

/// Scope-wide failures — credential (HTTP 401/402/403/429) and transport
/// (connect refused, unreachable engine, timeout) — indict the vendor's
/// key/account or the whole endpoint, not the individual model. Every
/// sibling on that scope fails identically, so one failure cools them all.
/// A longer window than the per-provider cooldown: these problems persist
/// until an operator intervenes, and an openrouter catalog alone is ~130
/// names that would each burn a request re-learning the same 402.
const VENDOR_DOWN_COOLDOWN_SECS: u64 = 600;

/// Credential scope for a provider name. `catalog-<vendor>-<model>` and
/// `<vendor>-<model>` registrations both draw from the vendor's endpoint +
/// key, so the scope key is the leading vendor token.
fn vendor_scope(name: &str) -> Option<String> {
    let rest = name.strip_prefix("catalog-").unwrap_or(name);
    if rest.to_ascii_lowercase().starts_with("mcp-") {
        // MCP provider IDs do not encode a reliably separable server/tool
        // boundary. Treat each bridge as its own scope rather than letting
        // one server outage quarantine every MCP-backed model.
        return Some(rest.to_ascii_lowercase());
    }
    rest.split(['-', '/'])
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn cooldowns_path() -> PathBuf {
    // Test/debug override — cooldown unit tests must not persist into the
    // host's live provider_cooldowns.json.
    if let Some(p) = std::env::var_os("SUSI_COOLDOWNS_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    crate::susi_paths::SusiDirs::config_dir().join("provider_cooldowns.json")
}

/// Cooldowns persist across daemon restarts — a vendor whose key is out
/// of credits does not revive because the process bounced, and without
/// persistence the first post-restart mission re-probes every dead scope.
fn load_cooldowns() -> std::collections::HashMap<String, u64> {
    let now = now_unix();
    std::fs::read_to_string(cooldowns_path())
        .ok()
        .and_then(|s| serde_json::from_str::<std::collections::HashMap<String, u64>>(&s).ok())
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, until)| *until > now)
        .collect()
}

fn save_cooldowns(map: &std::collections::HashMap<String, u64>) {
    let now = now_unix();
    let live: std::collections::HashMap<&String, &u64> =
        map.iter().filter(|(_, until)| **until > now).collect();
    let Ok(bytes) = serde_json::to_vec(&live) else {
        return;
    };
    // Unique per writer, not just per process: two threads persisting
    // quarantine at once must not share a staging file.
    let _ = crate::susi_config::atomic_write_bytes(&cooldowns_path(), &bytes);
}

fn provider_down_map() -> &'static std::sync::RwLock<std::collections::HashMap<String, u64>> {
    static MAP: std::sync::OnceLock<std::sync::RwLock<std::collections::HashMap<String, u64>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| std::sync::RwLock::new(load_cooldowns()))
}

impl InferenceRouter {
    /// True while `name` is inside its post-failure cooldown window — either
    /// its own, or the credential-scope cooldown a sibling's auth/quota
    /// failure imposed on the whole vendor.
    pub fn provider_cooled(name: &str) -> bool {
        Self::provider_cooldown_until(name).is_some()
    }

    /// Effective provider-or-vendor quarantine deadline, excluding expired
    /// entries. A vendor-wide deadline wins when it extends beyond the
    /// individual provider deadline.
    pub fn provider_cooldown_until(name: &str) -> Option<u64> {
        let map = provider_down_map()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        let provider = map.get(name).copied().filter(|until| now < *until);
        let vendor = vendor_scope(name).and_then(|scope| {
            map.get(&format!("vendor:{scope}"))
                .copied()
                .filter(|until| now < *until)
        });
        provider.into_iter().chain(vendor).max()
    }

    /// Mark a provider down after a failed attempt; a success clears it via
    /// `record_provider_success`.
    pub fn record_provider_failure(name: &str) {
        let mut map = provider_down_map()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        map.insert(name.to_string(), now_unix() + PROVIDER_DOWN_COOLDOWN_SECS);
        save_cooldowns(&map);
    }

    /// Mark a provider's whole scope down: call when the failure is
    /// credential-scoped (HTTP 401/402/403/429) or endpoint-scoped
    /// (transport errors) rather than model-scoped, so siblings sharing
    /// the key/engine are skipped without probing.
    pub fn record_vendor_failure(name: &str) {
        if let Some(scope) = vendor_scope(name) {
            let mut map = provider_down_map()
                .write()
                .unwrap_or_else(|e| e.into_inner());
            map.insert(
                format!("vendor:{scope}"),
                now_unix() + VENDOR_DOWN_COOLDOWN_SECS,
            );
            save_cooldowns(&map);
        }
    }

    /// Record a failed provider attempt: per-provider cooldown always,
    /// plus the credential/endpoint scope when the error is auth/quota or
    /// transport (siblings sharing the key or engine would fail the same
    /// way). Shared by the primary cascade and the plane-bus endpoint the
    /// swarm recovery loop reports through.
    pub fn record_failure(name: &str, error: &str) {
        Self::record_provider_failure(name);
        // Retired / unknown model IDs are model-scoped: cool this entry
        // longer so a stale catalog does not burn a request every mission.
        if Self::is_model_gone_error(error) {
            let mut map = provider_down_map()
                .write()
                .unwrap_or_else(|e| e.into_inner());
            map.insert(name.to_string(), now_unix() + MODEL_GONE_COOLDOWN_SECS);
            save_cooldowns(&map);
            return;
        }
        let lower = error.to_ascii_lowercase();
        let scoped = [
            "http 401",
            "http 402",
            "http 403",
            "http 429",
            "error sending request",
            "connection refused",
            "tcp connect error",
            "timed out",
        ]
        .iter()
        .any(|pattern| lower.contains(pattern));
        if scoped {
            Self::record_vendor_failure(name);
        }
    }

    fn is_model_gone_error(error: &str) -> bool {
        let lower = error.to_ascii_lowercase();
        [
            "http 404",
            "no endpoints found",
            "model not found",
            "does not exist",
            "no longer available",
            "is not found for api version",
        ]
        .iter()
        .any(|s| lower.contains(s))
    }

    /// Clear a provider's cooldown after a successful call. A live response
    /// also proves the vendor's credential/endpoint works, so the sibling
    /// scope clears too — otherwise one bad model would strand every good
    /// sibling for the full vendor TTL.
    pub fn record_provider_success(name: &str) {
        let mut map = provider_down_map()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut changed = map.remove(name).is_some();
        if let Some(scope) = vendor_scope(name) {
            changed |= map.remove(&format!("vendor:{scope}")).is_some();
        }
        if changed {
            save_cooldowns(&map);
        }
    }

    /// Clear a provider and its vendor-scope quarantine after an operator
    /// has repaired credentials or connectivity. Returns whether state
    /// changed, so control-plane callers can report a no-op honestly.
    pub fn clear_provider_cooldown(name: &str) -> bool {
        if !crate::susi_core::plane_bus::gemi::valid_provider_id(name) {
            return false;
        }
        let mut map = provider_down_map()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut changed = map.remove(name).is_some();
        if let Some(scope) = vendor_scope(name) {
            changed |= map.remove(&format!("vendor:{scope}")).is_some();
        }
        if changed {
            save_cooldowns(&map);
        }
        changed
    }

    fn preference_path() -> PathBuf {
        crate::susi_paths::SusiDirs::config_dir().join("routing_preference.json")
    }

    fn stats_path() -> PathBuf {
        crate::susi_paths::SusiDirs::config_dir().join("local_inference_stats.json")
    }

    pub fn load_preference() -> RoutingPreference {
        let path = Self::preference_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_preference(pref: &RoutingPreference) {
        let path = Self::preference_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(body) = serde_json::to_string_pretty(pref) {
            let _ = std::fs::write(path, body);
        }
    }

    /// Set sticky preferred cloud vendor (e.g. paid `openai` over free tiers).
    /// Accepts a vendor alias (`deepseek`) or full provider id; stored as a
    /// lowercase substring used when ranking / escalating.
    pub fn set_preferred_cloud(vendor: &str) -> Result<String, String> {
        let vendor = vendor.trim();
        if vendor.is_empty() {
            return Err("vendor must not be empty".into());
        }
        let normalized = vendor.to_ascii_lowercase().replace(' ', "-");
        let mut pref = Self::load_preference();
        pref.preferred_cloud = Some(normalized.clone());
        // Preferring a cloud implies we're willing to use cloud when needed.
        if pref.policy_override.as_deref() == Some("local_only") {
            pref.policy_override = Some("auto".to_string());
        }
        Self::save_preference(&pref);
        Ok(format!(
            "Preferred cloud set to `{}` (saved in {}).\n\
             When multiple clouds are available, susi will try this first.\n\
             Clear with: susi keys prefer --clear",
            normalized,
            Self::preference_path().display()
        ))
    }

    pub fn clear_preferred_cloud() -> Result<String, String> {
        let mut pref = Self::load_preference();
        pref.preferred_cloud = None;
        Self::save_preference(&pref);
        Ok(format!(
            "Cleared preferred cloud ({})",
            Self::preference_path().display()
        ))
    }

    /// Human-readable routing preference summary.
    pub fn preference_status() -> String {
        let pref = Self::load_preference();
        let policy = pref
            .policy_override
            .clone()
            .unwrap_or_else(|| "auto (from config)".to_string());
        let preferred = pref
            .preferred_cloud
            .clone()
            .unwrap_or_else(|| "(none — rank / ask)".to_string());
        format!(
            "Cloud routing preference\n  policy:    {}\n  preferred: {}\n  file:      {}",
            policy,
            preferred,
            Self::preference_path().display()
        )
    }

    /// Whether `name` matches the sticky preferred cloud (substring either way).
    pub fn matches_preferred_cloud(name: &str) -> bool {
        let Some(preferred) = Self::load_preference().preferred_cloud else {
            return false;
        };
        let pref_l = preferred.to_ascii_lowercase();
        let name_l = name.to_ascii_lowercase();
        name_l.contains(&pref_l) || pref_l.contains(&name_l)
    }

    pub fn load_stats() -> LocalInferenceStats {
        let path = Self::stats_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Stats for `model_id` only. Legacy unscoped files (empty `model_id`)
    /// and measurements from a different model start cold so a GPU-fit
    /// swap is not punished by a prior CPU-spill EMA.
    pub fn load_stats_for(model_id: &str) -> LocalInferenceStats {
        let stats = Self::load_stats();
        if model_id.is_empty() {
            return stats;
        }
        if stats.model_id.is_empty() || stats.model_id != model_id {
            return LocalInferenceStats {
                model_id: model_id.to_string(),
                ..Default::default()
            };
        }
        stats
    }

    pub fn record_local_sample(model_id: &str, latency: Duration, output_chars: usize) {
        let latency_ms = latency.as_millis() as u64;
        // Rough token estimate (~4 chars/token) — gate only, not billing.
        let approx_tokens = (output_chars / 4).max(1) as f32;
        let secs = latency.as_secs_f32().max(0.001);
        let tps = approx_tokens / secs;

        let mut stats = Self::load_stats_for(model_id);
        if stats.samples == 0 {
            stats.ema_tokens_per_sec = tps;
        } else {
            // EMA so one outlier doesn't permanently lock cloud-on or cloud-off.
            stats.ema_tokens_per_sec = stats.ema_tokens_per_sec * 0.7 + tps * 0.3;
        }
        stats.last_latency_ms = latency_ms;
        stats.samples = stats.samples.saturating_add(1);
        stats.model_id = model_id.to_string();

        let path = Self::stats_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(body) = serde_json::to_string_pretty(&stats) {
            let _ = std::fs::write(path, body);
        }
    }

    pub fn is_interactive() -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal()
    }

    /// Name heuristic for local OpenAI-compat engines (never escalate-as-cloud).
    fn is_local_engine_name(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        lower.contains("ollama")
            || lower.contains("vllm")
            || lower.contains("sglang")
            || lower.contains("llama.cpp")
            || lower.contains("llamacpp")
            || lower.contains("lmstudio")
            || lower.contains("lmdeploy")
            || lower.contains("triton")
            || lower.contains("candle")
    }

    /// True when `name` looks like a cloud / remote vendor (not a local engine).
    /// Prefer [`list_cloud_providers_from_registry`] when a registry is available.
    pub fn is_cloud_provider_name(name: &str) -> bool {
        if Self::is_local_engine_name(name) {
            return false;
        }
        let lower = name.to_ascii_lowercase();
        // MCP-bridged LLM tools (mcp-{server}-{tool})
        if lower.starts_with("mcp-") {
            return true;
        }
        // Common vendors + any non-local registered `vendor-model` style name.
        lower.contains("openai")
            || lower.contains("anthropic")
            || lower.contains("gemini")
            || lower.contains("google")
            || lower.contains("deepseek")
            || lower.contains("minimax")
            || lower.contains("kimi")
            || lower.contains("moonshot")
            || lower.contains("openrouter")
            || lower.contains("mistral")
            || lower.contains("groq")
            || lower.contains("together")
            || lower.contains("fireworks")
            // Config-registered remotes are named `{endpoint}-{model}`.
            || lower.contains('-')
    }

    /// Cloud providers = registered remote HTTPS `HttpProvider`s plus MCP-bridged
    /// LLM tools (`mcp-*`). This is the source of truth for OpenAI-compatible
    /// vendors and MCP-exposed cloud models.
    pub fn list_cloud_providers_from_registry(
        registry: &crate::susi_core::registry::CapabilityRegistry,
    ) -> Vec<String> {
        let mut clouds = Vec::new();
        for name in registry.list_providers() {
            let Some(provider) = registry.get_provider(&name) else {
                continue;
            };
            if let Some(http) = provider
                .as_any()
                .downcast_ref::<crate::http_provider::HttpProvider>()
            {
                if crate::http_provider::HttpProvider::is_remote_cloud(&http.api_base) {
                    clouds.push(name);
                    continue;
                }
            }
            if name.to_ascii_lowercase().starts_with("mcp-") {
                clouds.push(name);
            }
        }
        clouds.sort();
        clouds
    }

    /// One deterministic recovery pass, honoring local-only policy and the
    /// preferred vendor without changing process-wide routing preferences.
    pub fn cloud_failover_order(
        registry: &crate::susi_core::registry::CapabilityRegistry,
    ) -> Vec<String> {
        if crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference() {
            return Vec::new();
        }
        let cfg = SusiConfig::load_global()
            .unwrap_or_default()
            .inference_routing();
        let pref = Self::load_preference();
        if Self::effective_policy(&cfg, &pref) == "local_only" {
            return Vec::new();
        }
        let mut providers = Self::list_cloud_providers_from_registry(registry);
        providers.retain(|name| !Self::provider_cooled(name));
        providers.sort_by_key(|name| {
            let preferred = pref.preferred_cloud.as_ref().is_some_and(|preferred| {
                name.to_ascii_lowercase()
                    .contains(&preferred.to_ascii_lowercase())
            });
            (!preferred, Self::cloud_rank(name), name.clone())
        });
        providers
    }

    pub fn list_cloud_providers(names: &[String]) -> Vec<String> {
        let mut clouds: Vec<String> = names
            .iter()
            .filter(|n| Self::is_cloud_provider_name(n))
            .cloned()
            .collect();
        clouds.sort();
        clouds
    }

    /// PHASE 2: Dynamic Model Router based on requested capabilities and constraints.
    pub fn resolve_model_for_capabilities(
        requires: Option<&str>,
        max_cost: Option<f64>,
        registry: &crate::susi_core::registry::CapabilityRegistry,
    ) -> Option<String> {
        let mut clouds = Self::cloud_failover_order(registry);
        Self::apply_cloud_constraints(&mut clouds, requires, max_cost);

        clouds.first().cloned()
    }

    fn apply_cloud_constraints(
        clouds: &mut Vec<String>,
        requires: Option<&str>,
        max_cost: Option<f64>,
    ) {
        if requires.is_some_and(|value| !Self::supports_requirement(value)) {
            clouds.clear();
            return;
        }
        if requires.is_some_and(|value| value.eq_ignore_ascii_case("vision")) {
            clouds.retain(|name| {
                let name = name.to_ascii_lowercase();
                name.contains("gpt-4o")
                    || name.contains("claude-3-5-sonnet")
                    || name.contains("gemini")
                    || name.contains("vision")
                    || name.contains("vl-")
            });
        }
        if max_cost == Some(0.0) {
            clouds.retain(|name| name.to_ascii_lowercase().contains("free"));
        } else if max_cost.is_some_and(|cost| cost <= 0.01) {
            clouds.retain(|name| {
                let name = name.to_ascii_lowercase();
                !name.contains("opus") && !name.contains("gpt-4-")
            });
        }
    }

    fn remove_cooled_providers(clouds: &mut Vec<String>) {
        clouds.retain(|name| !Self::provider_cooled(name));
    }

    fn supports_requirement(requirement: &str) -> bool {
        matches!(
            requirement.to_ascii_lowercase().as_str(),
            "text" | "chat" | "reasoning" | "code" | "vision"
        )
    }

    fn local_supports_requirement(requires: Option<&str>) -> bool {
        !requires.is_some_and(|value| value.eq_ignore_ascii_case("vision"))
    }

    fn no_cloud_reason(local_ready: bool, has_cooled: bool) -> &'static str {
        match (local_ready, has_cooled) {
            (true, true) => "all matching cloud providers are quarantined",
            (false, true) => {
                "no ready local model is available and all matching cloud providers are quarantined"
            }
            (true, false) => "no ready cloud provider is registered",
            (false, false) => "no ready local model or cloud provider is available",
        }
    }

    fn effective_policy(cfg: &InferenceRoutingConfig, pref: &RoutingPreference) -> String {
        if let Some(until) = pref.force_local_until_unix {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if now < until {
                return "local_only".to_string();
            }
        }
        pref.policy_override
            .clone()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| cfg.policy.clone())
    }

    fn local_is_slow(cfg: &InferenceRoutingConfig, stats: &LocalInferenceStats) -> bool {
        if stats.samples == 0 {
            return false;
        }
        stats.ema_tokens_per_sec < cfg.local_min_tokens_per_sec
            || stats.last_latency_ms > cfg.local_max_latency_ms
    }

    fn cpu_only_host() -> bool {
        let profile = crate::hardware::HardwareProfiler::get_profile();
        profile.gpu_vram_gb == 0
            && !profile.gpu_info.to_ascii_lowercase().contains("cuda")
            && !profile.gpu_info.to_ascii_lowercase().contains("metal")
    }

    fn local_artifact_ready(path: &std::path::Path, minimum_bytes: u64) -> bool {
        use std::io::Read;

        if !path
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.len() >= minimum_bytes)
        {
            return false;
        }
        let Ok(mut file) = std::fs::File::open(path) else {
            return false;
        };
        let mut magic = [0_u8; 4];
        file.read_exact(&mut magic).is_ok() && &magic == b"GGUF"
    }

    fn local_model_ready(model: Option<&str>, cfg: &SusiConfig) -> bool {
        model
            .and_then(crate::models::ModelManager::get_model_path)
            .is_some_and(|path| Self::local_artifact_ready(&path, cfg.model_file_min_bytes()))
    }

    /// Produce the placement decision used by live inference. The result is
    /// deliberately serializable so CLI, MCP, and control-plane surfaces can
    /// explain *why* a request stays local or leaves the host.
    pub fn plan_placement(available_providers: &[String]) -> PlacementDecision {
        Self::plan_placement_for(available_providers, None, None, true)
    }

    /// Workload-aware variant of [`Self::plan_placement`]. Capability and
    /// budget constraints narrow the cloud candidates before policy chooses
    /// a target; the unfiltered method remains the runtime-compatible default.
    pub fn plan_placement_for(
        available_providers: &[String],
        requires: Option<&str>,
        max_cost: Option<f64>,
        allow_cloud: bool,
    ) -> PlacementDecision {
        let global_cfg = SusiConfig::load_global().unwrap_or_default();
        let cfg = global_cfg.inference_routing();
        let pref = Self::load_preference();
        let policy = Self::effective_policy(&cfg, &pref);
        let local_model = crate::models::ModelManager::get_selected_model(None);
        let local_ready = Self::local_model_ready(local_model.as_deref(), &global_cfg);
        let stats = Self::load_stats_for(local_model.as_deref().unwrap_or(""));

        if requires.is_some_and(|value| !Self::supports_requirement(value)) {
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: "unavailable".to_string(),
                provider: None,
                reason: "required capability is not supported by the current routing contract"
                    .to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: Vec::new(),
                cooled_candidates: Vec::new(),
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        // Hard edge-privacy gate: local_only MAC mode never escalates unless
        // an explicit cloud.inference capability token was granted.
        if !allow_cloud
            || crate::susi_core::mac_policy::MacPolicy::global().blocks_cloud_inference()
        {
            let request_blocked = !allow_cloud;
            let unsupported_local_capability = !Self::local_supports_requirement(requires);
            let local_eligible = local_ready && !unsupported_local_capability;
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: if local_eligible {
                    "local"
                } else {
                    "unavailable"
                }
                .to_string(),
                provider: None,
                reason: if request_blocked && unsupported_local_capability {
                    "request prohibits cloud inference and local runtime does not support the required capability"
                } else if request_blocked && !local_ready {
                    "request prohibits cloud inference and no ready local model is available"
                } else if request_blocked {
                    "request prohibits cloud inference"
                } else if unsupported_local_capability {
                    "privacy policy blocks cloud inference and local runtime does not support the required capability"
                } else if local_ready {
                    "privacy policy blocks cloud inference"
                } else {
                    "privacy policy blocks cloud inference and no ready local model is available"
                }
                .to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: Vec::new(),
                cooled_candidates: Vec::new(),
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        // Prefer registry HTTPS remotes (any OpenAI-compat vendor); fall back
        // to name heuristics when only bare names are supplied (tests).
        let mut clouds = Self::list_cloud_providers_from_registry(
            crate::susi_core::registry::CapabilityRegistry::global(),
        );
        if clouds.is_empty() {
            clouds = Self::list_cloud_providers(available_providers);
        }
        let cooled_candidates: Vec<ProviderCooldown> = clouds
            .iter()
            .filter_map(|name| {
                Self::provider_cooldown_until(name).map(|until_unix| ProviderCooldown {
                    provider: name.clone(),
                    until_unix,
                })
            })
            .collect();
        Self::remove_cooled_providers(&mut clouds);
        Self::apply_cloud_constraints(&mut clouds, requires, max_cost);

        if clouds.is_empty() {
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: if local_ready { "local" } else { "unavailable" }.to_string(),
                provider: None,
                reason: Self::no_cloud_reason(local_ready, !cooled_candidates.is_empty())
                    .to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: clouds,
                cooled_candidates,
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        let forced_reason = match policy.as_str() {
            "cloud_first" => Some(("cloud", "cloud_first policy")),
            "ask" => Some(("cloud", "ask policy — using cloud")),
            _ => None,
        };
        if let Some((target, reason)) = forced_reason {
            let provider = (target == "cloud").then(|| Self::pick_cloud(&clouds, &pref, &cfg));
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: target.to_string(),
                provider,
                reason: reason.to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: clouds,
                cooled_candidates,
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        if policy == "local_only" {
            let capability_supported = Self::local_supports_requirement(requires);
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: if local_ready && capability_supported {
                    "local"
                } else {
                    "unavailable"
                }
                .to_string(),
                provider: None,
                reason: if !capability_supported {
                    "local_only policy cannot satisfy the required capability"
                } else if !local_ready {
                    "local_only policy has no ready local model"
                } else {
                    "local_only policy"
                }
                .to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: clouds,
                cooled_candidates,
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        if requires.is_some_and(|value| value.eq_ignore_ascii_case("vision")) {
            let provider = Self::pick_cloud(&clouds, &pref, &cfg);
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: "cloud".to_string(),
                provider: Some(provider),
                reason: "vision capability requires a matching provider".to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: clouds,
                cooled_candidates,
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        if !local_ready {
            let provider = Self::pick_cloud(&clouds, &pref, &cfg);
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: "cloud".to_string(),
                provider: Some(provider),
                reason: "no ready local model is available".to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: clouds,
                cooled_candidates,
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        let slow = Self::local_is_slow(&cfg, &stats);
        let cpu_only = cfg.prefer_cloud_when_cpu_only && Self::cpu_only_host();

        if !slow && !cpu_only {
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: "local".to_string(),
                provider: None,
                reason: "local inference is within policy thresholds".to_string(),
                policy,
                local_model,
                local_ready,
                local_stats: stats,
                cloud_candidates: clouds,
                cooled_candidates,
                requires: requires.map(str::to_string),
                max_cost,
                allow_cloud,
            };
        }

        let reason = if slow {
            format!(
                "local ~{:.1} tok/s (min {:.1}) / last {}ms",
                stats.ema_tokens_per_sec, cfg.local_min_tokens_per_sec, stats.last_latency_ms
            )
        } else {
            "CPU-only host — local GGUF typically too slow".to_string()
        };

        let provider = Self::pick_cloud(&clouds, &pref, &cfg);
        PlacementDecision {
            contract: PlacementContract::now(),
            target: "cloud".to_string(),
            provider: Some(provider),
            reason,
            policy,
            local_model,
            local_ready,
            local_stats: stats,
            cloud_candidates: clouds,
            cooled_candidates,
            requires: requires.map(str::to_string),
            max_cost,
            allow_cloud,
        }
    }

    /// Decide whether to escalate to cloud before local inference.
    /// Returns `None` when local path should proceed as usual.
    pub fn maybe_escalate_to_cloud(available_providers: &[String]) -> Option<CloudEscalation> {
        let decision = Self::plan_placement(available_providers);
        decision.provider.map(|provider| CloudEscalation {
            provider,
            reason: decision.reason,
        })
    }

    fn pick_cloud(
        clouds: &[String],
        pref: &RoutingPreference,
        cfg: &InferenceRoutingConfig,
    ) -> String {
        if let Some(preferred) = pref.preferred_cloud.as_ref() {
            let pref_l = preferred.to_ascii_lowercase();
            if let Some(hit) = clouds.iter().find(|c| {
                c.to_ascii_lowercase().contains(&pref_l) || pref_l.contains(&c.to_ascii_lowercase())
            }) {
                return hit.clone();
            }
            if clouds.iter().any(|c| c == preferred) {
                return preferred.clone();
            }
        }

        if cfg.ask_when_multiple_clouds
            && clouds.len() > 1
            && pref.preferred_cloud.is_none()
            && Self::is_interactive()
        {
            if let Some(chosen) = Self::prompt_cloud_choice(clouds) {
                let mut next = pref.clone();
                next.preferred_cloud = Some(chosen.clone());
                Self::save_preference(&next);
                return chosen;
            }
        }

        // Non-interactive / single cloud / declined prompt: prefer openai→anthropic→gemini order.
        let mut ranked = clouds.to_vec();
        ranked.sort_by_key(|n| Self::cloud_rank(n));
        ranked
            .into_iter()
            .next()
            .unwrap_or_else(|| clouds[0].clone())
    }

    fn cloud_rank(name: &str) -> u8 {
        let lower = name.to_ascii_lowercase();
        // OpenRouter is the zero-config mesh: one key unlocks many upstreams.
        if lower.contains("openrouter") {
            0
        } else if lower.contains("openai") {
            1
        } else if lower.contains("anthropic") {
            2
        } else if lower.contains("gemini") || lower.contains("google") {
            3
        } else if lower.contains("deepseek") {
            4
        } else if lower.contains("kimi") || lower.contains("moonshot") {
            5
        } else if lower.contains("minimax") {
            6
        } else if lower.contains("mistral") {
            7
        } else if lower.contains("groq") {
            8
        } else if lower.starts_with("mcp-") {
            9
        } else {
            10
        }
    }

    fn prompt_cloud_choice(clouds: &[String]) -> Option<String> {
        let _ = writeln!(
            io::stderr(),
            "\n[SUSI ROUTING] Local inference is constrained. Cloud backends available:"
        );
        for (i, name) in clouds.iter().enumerate() {
            let _ = writeln!(io::stderr(), "  [{}] {}", i + 1, name);
        }
        let _ = writeln!(
            io::stderr(),
            "  [Enter] = {} (default)   [0] = keep trying local",
            clouds[0]
        );
        let _ = write!(io::stderr(), "Select cloud backend: ");
        let _ = io::stderr().flush();

        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return Some(clouds[0].clone());
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Some(clouds[0].clone());
        }
        if trimmed == "0" {
            let mut pref = Self::load_preference();
            pref.policy_override = Some("local_only".to_string());
            Self::save_preference(&pref);
            let _ = writeln!(
                io::stderr(),
                "[SUSI ROUTING] Sticky preference set to local_only. Clear ~/.susi/routing_preference.json to reset."
            );
            return None;
        }
        if let Ok(idx) = trimmed.parse::<usize>() {
            if idx >= 1 && idx <= clouds.len() {
                return Some(clouds[idx - 1].clone());
            }
        }
        // Substring match
        let lower = trimmed.to_ascii_lowercase();
        clouds
            .iter()
            .find(|c| c.to_ascii_lowercase().contains(&lower))
            .cloned()
            .or_else(|| Some(clouds[0].clone()))
    }

    /// Announce a glass-box escalation line (once per decision).
    pub fn announce(escalation: &CloudEscalation) {
        let _ = writeln!(
            io::stderr(),
            "[SUSI ROUTING] {} — using cloud `{}`. Say sticky local via routing_preference or pick [0] when prompted.",
            escalation.reason, escalation.provider
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_artifact_readiness_requires_size_and_gguf_magic() {
        let root =
            std::env::temp_dir().join(format!("susi-routing-artifact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let valid = root.join("valid.gguf");
        let wrong = root.join("wrong.gguf");
        std::fs::write(&valid, b"GGUFweights").unwrap();
        std::fs::write(&wrong, b"NOPEweights").unwrap();

        assert!(InferenceRouter::local_artifact_ready(&valid, 8));
        assert!(!InferenceRouter::local_artifact_ready(&valid, 64));
        assert!(!InferenceRouter::local_artifact_ready(&wrong, 8));
        assert!(!InferenceRouter::local_artifact_ready(
            &root.join("missing.gguf"),
            1
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cloud_constraints_share_capability_and_budget_filtering() {
        let mut clouds = vec![
            "openai-gpt-4o-mini".to_string(),
            "anthropic-claude-opus".to_string(),
            "google-gemini-flash".to_string(),
            "local-text-only".to_string(),
        ];
        InferenceRouter::apply_cloud_constraints(&mut clouds, Some("VISION"), Some(0.01));
        assert_eq!(
            clouds,
            vec![
                "openai-gpt-4o-mini".to_string(),
                "google-gemini-flash".to_string()
            ]
        );
    }

    #[test]
    fn cloud_name_detection() {
        assert!(InferenceRouter::is_cloud_provider_name(
            "googlegemini-gemini-3.6-flash"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "openai-gpt-4o-mini"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "anthropic-claude-3-5-haiku-20241022"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "deepseek-deepseek-chat"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "kimi-moonshot-v1-8k"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "minimax-MiniMax-Text-01"
        ));
        assert!(InferenceRouter::is_cloud_provider_name(
            "mcp-openai-bridge-chat"
        ));
        assert!(!InferenceRouter::is_cloud_provider_name("ollama-llama3"));
        assert!(!InferenceRouter::is_cloud_provider_name("vllm-mistral"));
    }

    #[test]
    fn list_clouds_filters_locals() {
        let names = vec![
            "ollama-llama3".into(),
            "openai-gpt-4o-mini".into(),
            "deepseek-deepseek-chat".into(),
            "googlegemini-gemini-3.6-flash".into(),
        ];
        let clouds = InferenceRouter::list_cloud_providers(&names);
        assert_eq!(clouds.len(), 3);
        assert!(clouds
            .iter()
            .all(|c| InferenceRouter::is_cloud_provider_name(c)));
        assert!(!clouds.iter().any(|c| c.contains("ollama")));
    }

    #[test]
    fn local_slow_gate() {
        let cfg = InferenceRoutingConfig {
            local_min_tokens_per_sec: 8.0,
            local_max_latency_ms: 15_000,
            ..Default::default()
        };
        let slow = LocalInferenceStats {
            ema_tokens_per_sec: 2.0,
            last_latency_ms: 1_000,
            samples: 3,
            ..Default::default()
        };
        assert!(InferenceRouter::local_is_slow(&cfg, &slow));
        let fast = LocalInferenceStats {
            ema_tokens_per_sec: 40.0,
            last_latency_ms: 200,
            samples: 3,
            ..Default::default()
        };
        assert!(!InferenceRouter::local_is_slow(&cfg, &fast));
        let cold = LocalInferenceStats::default();
        assert!(!InferenceRouter::local_is_slow(&cfg, &cold));
    }

    #[test]
    fn preferred_cloud_substring_match() {
        // See crate::engines::env_test_lock's doc: this test doesn't mutate
        // HOME/XDG itself, but it reads/writes the real preference file
        // those env vars resolve through, so it must still serialize
        // against tests (in this file and http_provider.rs) that redirect
        // them.
        let _guard = crate::engines::env_test_lock();
        let prev = InferenceRouter::load_preference();
        let pref = RoutingPreference {
            preferred_cloud: Some("deepseek".into()),
            ..Default::default()
        };
        InferenceRouter::save_preference(&pref);
        assert!(InferenceRouter::matches_preferred_cloud(
            "deepseek-deepseek-chat"
        ));
        assert!(!InferenceRouter::matches_preferred_cloud(
            "openai-gpt-4o-mini"
        ));
        InferenceRouter::save_preference(&prev);
    }

    #[test]
    fn request_can_prohibit_cloud_placement() {
        let decision = InferenceRouter::plan_placement_for(
            &["openai-gpt-4o-mini".to_string()],
            None,
            None,
            false,
        );
        assert_eq!(
            decision.target,
            if decision.local_ready {
                "local"
            } else {
                "unavailable"
            }
        );
        assert_eq!(decision.provider, None);
        assert!(decision
            .reason
            .starts_with("request prohibits cloud inference"));
        assert!(!decision.allow_cloud);
        assert!(decision.cloud_candidates.is_empty());
    }

    #[test]
    fn private_vision_request_fails_closed() {
        let decision = InferenceRouter::plan_placement_for(
            &["openai-gpt-4o-mini".to_string()],
            Some("vision"),
            None,
            false,
        );
        assert_eq!(decision.target, "unavailable");
        assert_eq!(decision.provider, None);
        assert!(decision.reason.contains("required capability"));
        assert!(decision.cloud_candidates.is_empty());
    }

    #[test]
    fn local_runtime_capability_gate_is_explicit() {
        assert!(InferenceRouter::local_supports_requirement(None));
        assert!(InferenceRouter::local_supports_requirement(Some("code")));
        assert!(!InferenceRouter::local_supports_requirement(Some("vision")));
    }

    #[test]
    fn unknown_requirement_never_routes_to_an_arbitrary_provider() {
        let decision = InferenceRouter::plan_placement_for(
            &["openai-gpt-4o-mini".to_string()],
            Some("telepathy"),
            None,
            true,
        );
        assert_eq!(decision.target, "unavailable");
        assert_eq!(decision.provider, None);
        assert!(decision.cloud_candidates.is_empty());
        assert!(decision.reason.contains("not supported"));
    }

    #[test]
    fn zero_cost_ceiling_only_keeps_explicitly_free_cloud_routes() {
        let mut clouds = vec![
            "openai-gpt-4o-mini".to_string(),
            "openrouter-qwen-free".to_string(),
        ];
        InferenceRouter::apply_cloud_constraints(&mut clouds, None, Some(0.0));
        assert_eq!(clouds, vec!["openrouter-qwen-free"]);
    }

    #[test]
    fn cloud_failover_order_prefers_sticky_vendor_once() {
        use crate::susi_core::registry::CapabilityRegistry;

        // Serialize against every other test that touches the real
        // preference file or mutates HOME / XDG (see env_test_lock's doc).
        let _guard = crate::engines::env_test_lock();

        let tmp = std::env::temp_dir().join(format!(
            "susi_failover_pref_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let config = tmp.join("config");
        let _ = std::fs::create_dir_all(&config);
        let _ = std::fs::create_dir_all(tmp.join(".susi"));

        let prev_home = std::env::var_os("HOME");
        let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
        let prev_susi_xdg = std::env::var_os("SUSI_XDG");
        // SAFETY: test-only env override; restored below under ENV_LOCK.
        unsafe {
            std::env::set_var("HOME", &tmp);
            std::env::set_var("XDG_CONFIG_HOME", &config);
            std::env::set_var("SUSI_XDG", "0");
        }

        let registry = CapabilityRegistry::new();
        registry.register_provider(crate::http_provider::HttpProvider {
            name: "openai-gpt-4o-mini".into(),
            api_base: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            protocol: crate::http_provider::InferenceProtocol::OpenAiChat,
            api_key: String::new(),
        });
        registry.register_provider(crate::http_provider::HttpProvider {
            name: "deepseek-deepseek-chat".into(),
            api_base: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            protocol: crate::http_provider::InferenceProtocol::OpenAiChat,
            api_key: String::new(),
        });
        registry.register_provider(crate::http_provider::HttpProvider::openai_local(
            "ollama-llama3",
            "http://127.0.0.1:11434/v1",
            "llama3",
        ));

        InferenceRouter::save_preference(&RoutingPreference {
            preferred_cloud: Some("deepseek".into()),
            ..Default::default()
        });
        assert_eq!(
            InferenceRouter::load_preference()
                .preferred_cloud
                .as_deref(),
            Some("deepseek"),
            "preference must round-trip under the isolated HOME"
        );
        let order = InferenceRouter::cloud_failover_order(&registry);

        unsafe {
            match prev_home {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            match prev_xdg {
                Some(h) => std::env::set_var("XDG_CONFIG_HOME", h),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match prev_susi_xdg {
                Some(h) => std::env::set_var("SUSI_XDG", h),
                None => std::env::remove_var("SUSI_XDG"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(
            order.first().map(String::as_str),
            Some("deepseek-deepseek-chat")
        );
        assert!(order.iter().any(|n| n.contains("openai")));
        assert!(!order.iter().any(|n| n.contains("ollama")));
    }

    /// Redirect the persisted cooldown file into a per-test tmp path so
    /// `record_*` never writes into the host's live `provider_cooldowns.json`.
    /// Caller holds `env_test_lock` for the whole test body.
    struct CooldownFileGuard {
        path: std::path::PathBuf,
    }

    impl CooldownFileGuard {
        fn new(tag: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("susi-cd-{tag}-{}.json", std::process::id()));
            // SAFETY: test-only env override; the caller serializes env
            // mutation through `env_test_lock` and restores on drop.
            unsafe {
                std::env::set_var("SUSI_COOLDOWNS_FILE", &path);
            }
            Self { path }
        }
    }

    impl Drop for CooldownFileGuard {
        fn drop(&mut self) {
            // SAFETY: see `new` — same lock scope.
            unsafe {
                std::env::remove_var("SUSI_COOLDOWNS_FILE");
            }
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn provider_cooldown_marks_down_and_clears_on_success() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("basic");
        let name = format!("cd-test-{}", std::process::id());
        assert!(!InferenceRouter::provider_cooled(&name));
        InferenceRouter::record_provider_failure(&name);
        assert!(InferenceRouter::provider_cooled(&name));
        assert!(InferenceRouter::provider_cooldown_until(&name).is_some());
        InferenceRouter::record_provider_success(&name);
        assert!(!InferenceRouter::provider_cooled(&name));
    }

    #[test]
    fn provider_cooldown_can_be_cleared_after_repair() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("repair");
        let name = format!("repairable-provider-{}", std::process::id());
        InferenceRouter::record_provider_failure(&name);
        assert!(InferenceRouter::provider_cooled(&name));
        assert!(InferenceRouter::clear_provider_cooldown(&name));
        assert!(!InferenceRouter::provider_cooled(&name));
        assert!(!InferenceRouter::clear_provider_cooldown(&name));
    }

    #[test]
    fn cooldown_persistence_creates_fresh_config_directory() {
        let _env = crate::engines::env_test_lock();
        let root = std::env::temp_dir().join(format!(
            "susi-cd-fresh-{}-{}",
            std::process::id(),
            now_unix()
        ));
        let path = root.join("nested/provider_cooldowns.json");
        // SAFETY: test-only env override, serialized by env_test_lock and
        // restored below before the directory is removed.
        unsafe {
            std::env::set_var("SUSI_COOLDOWNS_FILE", &path);
        }
        InferenceRouter::record_provider_failure(&format!(
            "fresh-config-provider-{}",
            std::process::id()
        ));
        unsafe {
            std::env::remove_var("SUSI_COOLDOWNS_FILE");
        }
        assert!(path.is_file());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn placement_candidates_exclude_cooled_providers() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("placement");
        let cooled = format!("openai-cooled-{}", std::process::id());
        let ready = format!("deepseek-ready-{}", std::process::id());
        InferenceRouter::record_provider_failure(&cooled);
        let mut providers = vec![cooled, ready.clone()];
        InferenceRouter::remove_cooled_providers(&mut providers);
        assert_eq!(providers, vec![ready]);
    }

    #[test]
    fn vendor_failure_cools_siblings_not_strangers() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("vend");
        let tag = std::process::id();
        let down = format!("vend{tag}-model-a");
        let sibling = format!("vend{tag}-model-b");
        let catalog_sibling = format!("catalog-vend{tag}-model-c");
        let stranger = format!("other{tag}-model-a");
        InferenceRouter::record_vendor_failure(&down);
        assert!(InferenceRouter::provider_cooled(&sibling));
        assert!(InferenceRouter::provider_cooled(&catalog_sibling));
        assert!(!InferenceRouter::provider_cooled(&stranger));
    }

    #[test]
    fn provider_failure_classification_is_case_insensitive() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("case");
        let provider = format!("casevendor-model-{}", std::process::id());
        let sibling = format!("casevendor-other-{}", std::process::id());
        InferenceRouter::record_failure(&provider, "connection REFUSED by upstream");
        assert!(InferenceRouter::provider_cooled(&sibling));
        InferenceRouter::clear_provider_cooldown(&provider);
    }

    #[test]
    fn mcp_provider_failures_do_not_quarantine_unrelated_bridges() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("mcp-scope");
        let failed = format!("mcp-server-a-chat-{}", std::process::id());
        let unrelated = format!("mcp-server-b-chat-{}", std::process::id());
        InferenceRouter::record_failure(&failed, "connection refused");
        assert!(InferenceRouter::provider_cooled(&failed));
        assert!(!InferenceRouter::provider_cooled(&unrelated));
        InferenceRouter::clear_provider_cooldown(&failed);
    }

    #[test]
    fn empty_cloud_reason_distinguishes_quarantine_from_absence() {
        assert_eq!(
            InferenceRouter::no_cloud_reason(true, true),
            "all matching cloud providers are quarantined"
        );
        assert!(InferenceRouter::no_cloud_reason(false, true).contains("no ready local model"));
        assert_eq!(
            InferenceRouter::no_cloud_reason(true, false),
            "no ready cloud provider is registered"
        );
    }

    #[test]
    fn placement_contract_is_versioned_and_timestamped() {
        let first = InferenceRouter::plan_placement_for(&[], Some("text"), None, false);
        let second = InferenceRouter::plan_placement_for(&[], Some("text"), None, false);
        assert_eq!(first.contract.schema, "susi/placement/v1");
        assert!(first.contract.decided_at_unix > 0);
        assert_ne!(first.contract.decision_id, second.contract.decision_id);
        let encoded = serde_json::to_value(first).unwrap();
        assert_eq!(encoded["contract"]["schema"], "susi/placement/v1");
        assert!(encoded["contract"]["decision_id"]
            .as_str()
            .is_some_and(|value| value.starts_with("placement-")));
    }

    #[test]
    fn classified_failure_scopes_vendor_and_success_clears_it() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("cls");
        // Unique vendor token — the cooldown map is process-global.
        let down = format!("vendcls{}-model-a", std::process::id());
        let sibling = down.replace("model-a", "model-b");
        // Credential-scoped error cools the whole vendor scope.
        InferenceRouter::record_failure(&down, "HTTP 402: insufficient credits");
        assert!(InferenceRouter::provider_cooled(&down));
        assert!(InferenceRouter::provider_cooled(&sibling));
        // A sibling's success proves the credential works — scope clears.
        InferenceRouter::record_provider_success(&sibling);
        assert!(!InferenceRouter::provider_cooled(&sibling));
        // The vendor scope lifted, but `down` keeps its own entry.
        assert!(InferenceRouter::provider_cooled(&down));
        InferenceRouter::record_provider_success(&down);
        assert!(!InferenceRouter::provider_cooled(&down));
    }

    #[test]
    fn unclassified_failure_does_not_scope_vendor() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("uncls");
        let down = format!("venduncls{}-model-a", std::process::id());
        let sibling = down.replace("model-a", "model-b");
        InferenceRouter::record_failure(&down, "HTTP 404: model not found");
        assert!(InferenceRouter::provider_cooled(&down));
        assert!(!InferenceRouter::provider_cooled(&sibling));
        // Model-gone errors must still clear on a later success (catalog fixed).
        InferenceRouter::record_provider_success(&down);
        assert!(!InferenceRouter::provider_cooled(&down));
    }
}
