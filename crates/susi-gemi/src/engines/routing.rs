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
    /// The user's own preferred cloud, while susi has moved `preferred_cloud`
    /// off it because that vendor is failing (no credit / rejected key). The
    /// key is untouched; the preference returns as soon as the vendor works.
    pub auto_switched_from: Option<String>,
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

/// Which rung of the routing ladder a step belongs to — the ladder descends
/// requested model → provider cascade → local engine (VC-202-004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LadderRung {
    /// A caller-named model (or escalation-named provider) the request
    /// asked for directly, before the general cascade ran.
    Model,
    /// A registry provider candidate in the failover cascade.
    Provider,
    /// The local inference engine — the last rung before failure.
    Local,
}

impl LadderRung {
    pub fn label(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Provider => "provider",
            Self::Local => "local",
        }
    }
}

/// How one ladder step resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepOutcome {
    /// Chosen — the request was routed to this candidate.
    Selected,
    /// Passed over before any call was made: cooled, quota or arbitration
    /// refusal, registration vanished, named model honored as a local id.
    Skipped,
    /// Called and failed or answered unusably.
    Failed,
}

impl StepOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }
}

/// One recorded routing step — which candidate, how it resolved, and why.
/// The reason is mandatory: a step down the ladder without one is a silent
/// fallback, which is the defect this type exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RouteStep {
    pub rung: LadderRung,
    pub candidate: String,
    pub outcome: StepOutcome,
    pub reason: String,
}

/// Why one candidate did not serve — verbatim from its recorded step, so
/// the mission's "worked alone" answer is checkable, not paraphrased.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AloneCause {
    pub candidate: String,
    pub outcome: StepOutcome,
    pub reason: String,
}

/// The mission's "the primary worked alone" record (VC-202-022/T-123).
/// Solo is a first-class path — on a bad day it is most missions — so the
/// trace names who served and *why nobody else did*: not available, not
/// healthy, or not worth its cost, one cause per considered candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoloReport {
    pub served_by: String,
    pub rung: LadderRung,
    /// Every other candidate the request considered and why it did not
    /// serve. Empty means the primary was the only candidate — alone by
    /// default, not by exception.
    pub alone_because: Vec<AloneCause>,
}

/// The complete routing trace for one request: the task class that shaped
/// it and every rung stepped down, each carrying its reason (VC-202-004).
/// Persisted as one JSON line per request so an operator can ask "which
/// model ran and why" after the fact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingLadder {
    pub schema: String,
    pub recorded_at_unix: u64,
    pub task_class: String,
    pub steps: Vec<RouteStep>,
    /// Set at persist time: present whenever exactly one candidate served —
    /// the primary working alone is a recorded outcome, not a silent gap.
    #[serde(default)]
    pub solo: Option<SoloReport>,
}

impl RoutingLadder {
    pub fn new(class: crate::engines::brain::TaskClass) -> Self {
        Self {
            schema: "susi/routing-ladder/v1".to_string(),
            recorded_at_unix: now_unix(),
            task_class: class.label().to_string(),
            steps: Vec::new(),
            solo: None,
        }
    }

    pub fn record(
        &mut self,
        rung: LadderRung,
        candidate: impl Into<String>,
        outcome: StepOutcome,
        reason: impl Into<String>,
    ) {
        self.steps.push(RouteStep {
            rung,
            candidate: candidate.into(),
            outcome,
            reason: reason.into(),
        });
    }

    /// The step that actually served the request, if any rung was selected.
    pub fn selected(&self) -> Option<&RouteStep> {
        self.steps
            .iter()
            .rev()
            .find(|step| step.outcome == StepOutcome::Selected)
    }

    /// How many rungs were stepped down (skipped or failed) before the
    /// request resolved — zero means the first-choice candidate served it.
    pub fn step_downs(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| step.outcome != StepOutcome::Selected)
            .count()
    }

    /// `Some` when exactly one candidate served — the primary working
    /// alone, a first-class outcome rather than a degraded one. The report
    /// names who served and carries every other candidate's reason for
    /// not serving, so "why it worked alone" is a record, not an inference.
    #[must_use]
    pub fn served_alone(&self) -> Option<SoloReport> {
        let mut selected = self
            .steps
            .iter()
            .filter(|step| step.outcome == StepOutcome::Selected);
        let winner = selected.next()?;
        if selected.next().is_some() {
            return None;
        }
        Some(SoloReport {
            served_by: winner.candidate.clone(),
            rung: winner.rung,
            alone_because: self
                .steps
                .iter()
                .filter(|step| step.outcome != StepOutcome::Selected)
                .map(|step| AloneCause {
                    candidate: step.candidate.clone(),
                    outcome: step.outcome,
                    reason: step.reason.clone(),
                })
                .collect(),
        })
    }

    /// One-line operator trace: which candidate served and why, followed by
    /// every rung stepped down with its reason.
    pub fn summary(&self) -> String {
        let head = match self.selected() {
            Some(step) => format!(
                "{} '{}' selected{}: {}",
                step.rung.label(),
                step.candidate,
                if self.served_alone().is_some() {
                    " (served alone)"
                } else {
                    ""
                },
                step.reason
            ),
            None => "no candidate selected".to_string(),
        };
        let downs: Vec<String> = self
            .steps
            .iter()
            .filter(|step| step.outcome != StepOutcome::Selected)
            .map(|step| {
                format!(
                    "{} '{}' {}: {}",
                    step.rung.label(),
                    step.candidate,
                    step.outcome.label(),
                    step.reason
                )
            })
            .collect();
        if downs.is_empty() {
            head
        } else {
            format!("{head} — stepped down: {}", downs.join("; "))
        }
    }

    /// Append the ladder as one JSON line to the routing trace file — the
    /// solo verdict is computed here so every persisted mission carries
    /// "who served alone and why" (VC-202-022/T-123). Hermetic under
    /// `cargo test` (Mandate 52): nothing writes unless the test sets
    /// `SUSI_ROUTING_TRACE_FILE`, exactly like the brain evidence store.
    pub fn persist(&mut self) {
        self.solo = self.served_alone();
        if cfg!(test) && std::env::var_os("SUSI_ROUTING_TRACE_FILE").is_none() {
            return;
        }
        let path = routing_trace_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(mut line) = serde_json::to_string(self) else {
            return;
        };
        line.push('\n');
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

fn routing_trace_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SUSI_ROUTING_TRACE_FILE").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    susi_paths::SusiDirs::config_dir().join("routing_trace.jsonl")
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
pub(crate) fn vendor_scope(name: &str) -> Option<String> {
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
    // Unit tests drive record_failure/heal without a guard; without this every
    // test run leaked its providers into the developer's real cooldown file.
    if cfg!(test) {
        return std::env::temp_dir()
            .join(format!("susi-test-cooldowns-{}.json", std::process::id()));
    }
    susi_paths::SusiDirs::config_dir().join("provider_cooldowns.json")
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
        Self::quarantine_vendor(name, VENDOR_DOWN_COOLDOWN_SECS);
    }

    /// Quarantine `name`'s whole credential scope for `secs`.
    fn quarantine_vendor(name: &str, secs: u64) {
        if let Some(scope) = vendor_scope(name) {
            let mut map = provider_down_map()
                .write()
                .unwrap_or_else(|e| e.into_inner());
            map.insert(format!("vendor:{scope}"), now_unix() + secs);
            save_cooldowns(&map);
        }
    }

    /// Failure-streak keys a provider's health is tracked under: itself and
    /// its credential scope.
    fn health_keys(name: &str) -> Vec<String> {
        std::iter::once(name.to_string())
            .chain(vendor_scope(name).map(|s| format!("vendor:{s}")))
            .collect()
    }

    /// Record a failed provider attempt: per-provider cooldown always,
    /// plus the credential/endpoint scope when the error is auth/quota or
    /// transport (siblings sharing the key or engine would fail the same
    /// way). Shared by the primary cascade and the plane-bus endpoint the
    /// swarm recovery loop reports through.
    pub fn record_failure(name: &str, error: &str) {
        Self::record_provider_failure(name);
        let kind = crate::engines::brain::classify_error(error);
        // No credit / rejected key: nothing changes until an operator acts, so
        // the credential scope is quarantined on an escalating ladder (10 min,
        // 1 h, 6 h) instead of being re-tried on a short timer, and the brain
        // stops treating the provider as fit. A later success or an operator
        // reset clears the streak.
        if kind.needs_operator() {
            let scope_key = vendor_scope(name)
                .map(|s| format!("vendor:{s}"))
                .unwrap_or_else(|| name.to_string());
            let streak = crate::engines::brain::note_failure(&scope_key, kind);
            let secs = crate::engines::brain::quarantine_secs(kind, streak)
                .unwrap_or(VENDOR_DOWN_COOLDOWN_SECS);
            Self::quarantine_vendor(name, secs);
            Self::heal_preferred_cloud(name);
            return;
        }
        crate::engines::brain::note_failure(name, kind);
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
        drop(map);
        crate::engines::brain::note_success(&Self::health_keys(name));
        Self::restore_preferred_cloud(name);
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
        drop(map);
        crate::engines::brain::note_success(&Self::health_keys(name));
        changed
    }

    /// Unexpired cooldown entries currently skipping providers (and any
    /// `vendor:` scope keys), sorted by provider name. Operator-facing
    /// view of the same map the router consults when ranking clouds.
    pub fn cooled_providers() -> Vec<ProviderCooldown> {
        let map = provider_down_map()
            .read()
            .unwrap_or_else(|e| e.into_inner());
        let now = now_unix();
        let mut out: Vec<ProviderCooldown> = map
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(provider, until_unix)| ProviderCooldown {
                provider: provider.clone(),
                until_unix: *until_unix,
            })
            .collect();
        out.sort_by(|a, b| a.provider.cmp(&b.provider));
        out
    }

    fn preference_path() -> PathBuf {
        // Same reason as `cooldowns_path`: tests must never rewrite the
        // developer's real routing preference.
        if cfg!(test) {
            return std::env::temp_dir().join(format!(
                "susi-test-routing-pref-{}.json",
                std::process::id()
            ));
        }
        susi_paths::SusiDirs::config_dir().join("routing_preference.json")
    }

    fn stats_path() -> PathBuf {
        susi_paths::SusiDirs::config_dir().join("local_inference_stats.json")
    }

    pub fn load_preference() -> RoutingPreference {
        let path = Self::preference_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomic: a reader racing an in-place write would see an empty file,
    /// parse it as the default preference, and lose a `local_only` override.
    pub fn save_preference(pref: &RoutingPreference) {
        if let Ok(body) = serde_json::to_string_pretty(pref) {
            let _ =
                crate::susi_config::atomic_write_bytes(&Self::preference_path(), body.as_bytes());
        }
    }

    /// Make `local_only` the sticky routing policy, keeping the rest of the
    /// saved preference. The one writer for this override — the privacy CLI
    /// and the interactive cloud prompt both go through it, so it always
    /// lands in the file the router reads.
    pub fn set_local_only() {
        let mut pref = Self::load_preference();
        pref.policy_override = Some("local_only".to_string());
        Self::save_preference(&pref);
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
        // An explicit choice by the user wins over any automatic switch.
        pref.auto_switched_from = None;
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
        pref.auto_switched_from = None;
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

    /// The sticky preferred cloud is out of credit or rejecting its key: move
    /// the preference to the best healthy cloud so the brain is always
    /// something that works. Keys are never touched, and the user's original
    /// choice is remembered in `auto_switched_from` so it returns on recovery.
    /// Nothing changes when the failing vendor is not the preferred one or no
    /// healthy cloud exists (local stays the floor).
    fn heal_preferred_cloud(failed: &str) {
        Self::heal_preferred_cloud_in(
            crate::susi_core::registry::CapabilityRegistry::global(),
            failed,
        );
    }

    fn heal_preferred_cloud_in(
        registry: &crate::susi_core::registry::CapabilityRegistry,
        failed: &str,
    ) {
        let mut pref = Self::load_preference();
        let Some(current) = pref.preferred_cloud.clone() else {
            return;
        };
        let failed_l = failed.to_ascii_lowercase();
        let current_l = current.to_ascii_lowercase();
        if !(failed_l.contains(&current_l) || current_l.contains(&failed_l)) {
            return;
        }
        let clouds = Self::list_cloud_providers_from_registry(registry);
        let Some(best) = crate::engines::brain::best_healthy(
            &clouds,
            crate::engines::brain::TaskClass::Chat,
            &Self::provider_cooled,
        ) else {
            return;
        };
        let Some(vendor) = vendor_scope(&best) else {
            return;
        };
        if vendor.eq_ignore_ascii_case(&current) {
            return;
        }
        pref.auto_switched_from.get_or_insert(current.clone());
        pref.preferred_cloud = Some(vendor.clone());
        Self::save_preference(&pref);
        eprintln!(
            "[SUSI ROUTING] Preferred cloud `{current}` is unavailable (no credit or rejected key); \
             switched to healthy `{vendor}`. Your key is untouched; `{current}` returns as soon as it works."
        );
    }

    /// `name` just answered: when it belongs to the vendor susi moved the
    /// preference away from, put the user's preference back.
    fn restore_preferred_cloud(name: &str) {
        let mut pref = Self::load_preference();
        let Some(original) = pref.auto_switched_from.clone() else {
            return;
        };
        let name_l = name.to_ascii_lowercase();
        let original_l = original.to_ascii_lowercase();
        if !(name_l.contains(&original_l) || original_l.contains(&name_l)) {
            return;
        }
        pref.preferred_cloud = Some(original.clone());
        pref.auto_switched_from = None;
        Self::save_preference(&pref);
        eprintln!("[SUSI ROUTING] `{original}` works again; restored as the preferred cloud.");
    }

    /// True for a provider of the vendor susi moved the preference away from
    /// once its quarantine has lapsed: it gets one trial call first (a topped-up
    /// account is noticed), and a failure re-quarantines it on the next rung.
    pub fn is_recovery_probe(name: &str) -> bool {
        let Some(original) = Self::load_preference().auto_switched_from else {
            return false;
        };
        let name_l = name.to_ascii_lowercase();
        let original_l = original.to_ascii_lowercase();
        (name_l.contains(&original_l) || original_l.contains(&name_l))
            && !Self::provider_cooled(name)
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

        if let Ok(body) = serde_json::to_string_pretty(&stats) {
            let _ = crate::susi_config::atomic_write_bytes(&Self::stats_path(), body.as_bytes());
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
                .downcast_ref::<crate::engines::http_provider::HttpProvider>()
            {
                if crate::engines::http_provider::HttpProvider::is_remote_cloud(&http.api_base) {
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
    /// Recovery has no prompt to classify: it orders by ordinary chat.
    pub fn cloud_failover_order(
        registry: &crate::susi_core::registry::CapabilityRegistry,
    ) -> Vec<String> {
        Self::cloud_failover_order_for(registry, crate::engines::brain::TaskClass::Chat)
    }

    /// Workload-aware failover order: ranks providers by performance on
    /// `class`, so the capability floor of the work is applied — a model
    /// below the floor sorts behind every floor-meeting one whatever it
    /// costs (VC-202-004).
    pub fn cloud_failover_order_for(
        registry: &crate::susi_core::registry::CapabilityRegistry,
        class: crate::engines::brain::TaskClass,
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
        Self::cloud_failover_order_scoped(registry, class, &crate::key_arbitration::quota_headroom)
    }

    /// `cloud_failover_order_for` with an explicit quota-headroom lookup —
    /// tests inject headroom without touching the process-global arbiter.
    /// `headroom` returns `(remaining, allowance)` per provider, `None`
    /// when no quota windows are declared.
    pub fn cloud_failover_order_scoped(
        registry: &crate::susi_core::registry::CapabilityRegistry,
        class: crate::engines::brain::TaskClass,
        headroom: &crate::key_arbitration::HeadroomFn,
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
        // Order by the brain's ranking of this task class — its position
        // already folds in the capability floor, quota exhaustion, the
        // scarcity-adjusted marginal cost per verified outcome (or evidence
        // score when unpriced / Budget::Max), and the static rank as the
        // tiebreaker (VC-202-003, VC-202-021). The rank call is fed the
        // canonical static order first so a brain tie breaks
        // deterministically — `providers` arrives in registry (hash) order.
        providers.sort_by_key(|name| (Self::cloud_rank(name), name.clone()));
        // The injected headroom feeds the ranking's scarcity signal: a
        // worker's own cap (subscription window, prepaid balance) wins when
        // it declares one; the quota headroom covers everything else.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let fraction = |name: &str| -> Option<f64> {
            crate::worker::remaining_fraction(name, now).or_else(|| {
                headroom(name).map(|(remaining, allowance)| {
                    if allowance == 0 {
                        0.0
                    } else {
                        remaining as f64 / allowance as f64
                    }
                })
            })
        };
        let ranked: std::collections::HashMap<String, (bool, usize)> =
            crate::engines::brain::rank_with_quota(
                &providers,
                class,
                crate::engines::cost::Budget::from_env(),
                &fraction,
            )
            .into_iter()
            .enumerate()
            .map(|(pos, r)| (r.provider, (r.meets_floor, pos)))
            .collect();
        providers.sort_by_key(|name| {
            let preferred = pref.preferred_cloud.as_ref().is_some_and(|preferred| {
                name.to_ascii_lowercase()
                    .contains(&preferred.to_ascii_lowercase())
            });
            let (meets_floor, pos) = ranked.get(name).copied().unwrap_or((false, usize::MAX));
            (
                !preferred,
                !meets_floor,
                pos,
                Self::cloud_rank(name),
                name.clone(),
            )
        });
        providers
    }

    /// Scarcity tier for the failover sort: a provider whose tightest quota
    /// window is spent or nearly spent (≤10% allowance) sorts behind an
    /// abundant one — scarce capacity is not free capacity (VC-202-021).
    pub fn quota_scarcity_tier(headroom: Option<(u64, u64)>) -> u8 {
        match headroom {
            None => 0,
            Some((0, _)) => 2,
            Some((remaining, allowance)) if remaining <= allowance / 10 => 1,
            Some(_) => 0,
        }
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
    /// The `requires` token's task class sets the capability floor before any
    /// cost ceiling applies (VC-202-004).
    pub fn resolve_model_for_capabilities(
        requires: Option<&str>,
        max_cost: Option<f64>,
        registry: &crate::susi_core::registry::CapabilityRegistry,
    ) -> Option<String> {
        let class = crate::engines::brain::TaskClass::from_requires(requires);
        let mut clouds = Self::cloud_failover_order_for(registry, class);
        Self::apply_cloud_constraints(&mut clouds, requires, max_cost);

        clouds.first().cloned()
    }

    pub(crate) fn apply_cloud_constraints(
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
        // Cost ceilings price the task before the model is chosen
        // (VC-202-002/T-DEEPSEEK-88): a candidate with a catalog price
        // record is judged on the expected cost of the task's token
        // profile at its achieved cache rate; one without a record keeps
        // the coarse tier-table semantics — a zero ceiling keeps only free
        // routes, a near-zero one drops the premium tier.
        if let Some(cap) = max_cost {
            let class = crate::engines::brain::TaskClass::from_requires(requires);
            let catalog = crate::engines::cost::price_catalog();
            clouds.retain(|name| {
                match crate::engines::cost::expected_task_cost_usd_in(catalog.as_ref(), name, class)
                {
                    Some(cost) => cost <= cap,
                    None if cap <= 0.0 => {
                        crate::engines::cost::tier_of(name) == crate::engines::cost::CostTier::Free
                    }
                    None if cap <= 0.01 => {
                        crate::engines::cost::tier_of(name) < crate::engines::cost::CostTier::High
                    }
                    None => true,
                }
            });
        }
    }

    fn remove_cooled_providers(clouds: &mut Vec<String>) {
        clouds.retain(|name| !Self::provider_cooled(name));
    }

    fn supports_requirement(requirement: &str) -> bool {
        matches!(
            requirement.to_ascii_lowercase().as_str(),
            "text" | "chat" | "reasoning" | "code" | "vision" | "tools"
        )
    }

    /// Whether the local rung can serve the request's declared capability.
    /// `tools` is the GGUF-side equivalent of the cloud `BlockReason::NoTools`
    /// gate: the model must ship a tool-aware chat template, which is only
    /// knowable post-download (`ModelManager::supports_tool_calling`), so the
    /// host qualifies only when some discovered local model does.
    fn local_supports_requirement(requires: Option<&str>) -> bool {
        match requires.map(|value| value.to_ascii_lowercase()) {
            Some(value) if value == "vision" => false,
            Some(value) if value == "tools" => {
                crate::models::ModelManager::any_tool_capable_local_model(std::path::Path::new(""))
            }
            _ => true,
        }
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
        let profile = crate::models::hardware::HardwareProfiler::get_profile();
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
        Self::plan_placement_inner(
            available_providers,
            requires,
            max_cost,
            allow_cloud,
            Self::local_supports_requirement,
        )
    }

    /// The local capability verdict is a parameter so tests pin both
    /// branches deterministically — whether a host actually owns a
    /// tool-capable GGUF is environment state, not logic.
    fn plan_placement_inner(
        available_providers: &[String],
        requires: Option<&str>,
        max_cost: Option<f64>,
        allow_cloud: bool,
        local_capable: impl Fn(Option<&str>) -> bool,
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
            let unsupported_local_capability = !local_capable(requires);
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

        // Capability floor before any cost comparison (VC-202-004): the
        // `requires` token's class floor partitions candidates — below-floor
        // providers stay at the tail as the last rung before local, never
        // ahead of a floor-meeting one whatever they cost.
        let class = crate::engines::brain::TaskClass::from_requires(requires);
        clouds.sort_by_key(|name| !crate::engines::capability::meets_floor(name, class));

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
            let capability_supported = local_capable(requires);
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

        // `requires: "tools"` — a healthy local rung is still wrong when no
        // discovered GGUF can emit tool calls; escalate like vision does.
        if requires.is_some_and(|value| value.eq_ignore_ascii_case("tools"))
            && !local_capable(requires)
        {
            let provider = Self::pick_cloud(&clouds, &pref, &cfg);
            return PlacementDecision {
                contract: PlacementContract::now(),
                target: "cloud".to_string(),
                provider: Some(provider),
                reason: "required tool capability is not available locally".to_string(),
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
        // Tests using the mock seam must never prompt, persist a routing
        // choice, or contact a provider. The native runtime handles the same
        // flag and returns its deterministic mock failure locally.
        if std::env::var("SUSI_TEST_MOCK_INFERENCE").unwrap_or_default() == "true" {
            return None;
        }
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
            Self::set_local_only();
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
    fn set_local_only_keeps_the_rest_of_the_preference() {
        // Same real-file serialization as preferred_cloud_substring_match.
        let _guard = crate::engines::env_test_lock();
        let prev = InferenceRouter::load_preference();
        InferenceRouter::save_preference(&RoutingPreference {
            preferred_cloud: Some("deepseek".into()),
            force_local_until_unix: Some(42),
            ..Default::default()
        });
        InferenceRouter::set_local_only();
        let now = InferenceRouter::load_preference();
        InferenceRouter::save_preference(&prev);
        assert_eq!(now.policy_override.as_deref(), Some("local_only"));
        assert_eq!(now.preferred_cloud.as_deref(), Some("deepseek"));
        assert_eq!(now.force_local_until_unix, Some(42));
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

    /// `requires: "tools"` on a host with no tool-capable GGUF must not
    /// settle for a prose-only model — it escalates when cloud is allowed
    /// and fails closed when it is not. The capability probe is injected:
    /// which GGUFs the host actually owns is environment state, not logic.
    #[test]
    fn tools_request_routes_away_from_tool_less_local() {
        let incapable = |_: Option<&str>| false;
        let escalated = InferenceRouter::plan_placement_inner(
            &["openai-gpt-4o-mini".to_string()],
            Some("tools"),
            None,
            true,
            incapable,
        );
        assert_eq!(escalated.target, "cloud");
        assert_eq!(escalated.provider.as_deref(), Some("openai-gpt-4o-mini"));
        let blocked = InferenceRouter::plan_placement_inner(
            &["openai-gpt-4o-mini".to_string()],
            Some("tools"),
            None,
            false,
            incapable,
        );
        assert_eq!(blocked.target, "unavailable");
        assert!(blocked.reason.contains("required capability"));
    }

    #[test]
    fn local_runtime_capability_gate_is_explicit() {
        assert!(InferenceRouter::local_supports_requirement(None));
        assert!(InferenceRouter::local_supports_requirement(Some("code")));
        assert!(!InferenceRouter::local_supports_requirement(Some("vision")));
        // `tools` consults the host's GGUFs through the injected probe —
        // pinned at the placement level by
        // tools_request_routes_away_from_tool_less_local.
        assert!(InferenceRouter::supports_requirement("tools"));
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

        // Also scrubs SUSI_HOME/SUSI_PORT_OFFSET; restored via `env` below.
        let mut env = susi_paths::test_env::EnvGuard::isolated();
        env.set("HOME", &tmp);
        env.set("XDG_CONFIG_HOME", &config);
        env.set("SUSI_XDG", "0");

        let registry = CapabilityRegistry::new();
        registry.register_provider(crate::engines::http_provider::HttpProvider {
            name: "openai-gpt-4o-mini".into(),
            api_base: "https://api.openai.com/v1".into(),
            model: "gpt-4o-mini".into(),
            protocol: crate::engines::http_provider::InferenceProtocol::OpenAiChat,
            api_key: String::new(),
        });
        registry.register_provider(crate::engines::http_provider::HttpProvider {
            name: "deepseek-deepseek-chat".into(),
            api_base: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            protocol: crate::engines::http_provider::InferenceProtocol::OpenAiChat,
            api_key: String::new(),
        });
        registry.register_provider(crate::engines::http_provider::HttpProvider::openai_local(
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

        drop(env);
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
    fn cooled_providers_lists_unexpired_sorted_by_name() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("list");
        let tag = std::process::id();
        let later = format!("zzz-cd-list-{tag}");
        let earlier = format!("aaa-cd-list-{tag}");
        let expired = format!("mid-cd-list-expired-{tag}");
        InferenceRouter::record_provider_failure(&later);
        InferenceRouter::record_provider_failure(&earlier);
        {
            let mut map = provider_down_map()
                .write()
                .unwrap_or_else(|e| e.into_inner());
            map.insert(expired.clone(), now_unix().saturating_sub(1));
        }
        let cooled = InferenceRouter::cooled_providers();
        assert!(cooled.windows(2).all(|w| w[0].provider <= w[1].provider));
        let ours: Vec<&ProviderCooldown> = cooled
            .iter()
            .filter(|c| c.provider == earlier || c.provider == later || c.provider == expired)
            .collect();
        assert_eq!(ours.len(), 2);
        assert_eq!(ours[0].provider, earlier);
        assert_eq!(ours[1].provider, later);
        assert!(ours[0].until_unix > now_unix());
        assert!(ours[1].until_unix > now_unix());
        InferenceRouter::record_provider_success(&earlier);
        InferenceRouter::record_provider_success(&later);
    }

    #[test]
    fn funds_failures_escalate_the_quarantine_and_success_resets_it() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("ladder");
        let name = format!("ladderco-model-{}", std::process::id());
        let secs_left = |n: &str| {
            InferenceRouter::provider_cooldown_until(n)
                .map(|u| u.saturating_sub(now_unix()))
                .unwrap_or(0)
        };
        let near = |got: u64, want: u64| got <= want && got + 5 >= want;
        InferenceRouter::record_failure(&name, "Provider: HTTP 402: Insufficient credits");
        assert!(near(secs_left(&name), 600), "{}", secs_left(&name));
        InferenceRouter::record_failure(&name, "Insufficient Balance");
        assert!(near(secs_left(&name), 3_600), "{}", secs_left(&name));
        InferenceRouter::record_failure(&name, "HTTP 402");
        assert!(near(secs_left(&name), 21_600), "{}", secs_left(&name));
        InferenceRouter::record_failure(&name, "HTTP 402");
        assert!(near(secs_left(&name), 21_600), "capped at 6h");
        // A working call (or an operator reset) restarts the ladder.
        InferenceRouter::record_provider_success(&name);
        assert!(!InferenceRouter::provider_cooled(&name));
        InferenceRouter::record_failure(&name, "HTTP 402");
        assert!(near(secs_left(&name), 600));
        assert!(InferenceRouter::clear_provider_cooldown(&name));
    }

    /// Isolated HOME/XDG + cooldown file for preference-writing tests.
    struct PrefSandbox {
        _cd: CooldownFileGuard,
        prev: [(&'static str, Option<std::ffi::OsString>); 3],
        tmp: PathBuf,
    }
    impl PrefSandbox {
        fn new(tag: &str) -> Self {
            let tmp = std::env::temp_dir().join(format!(
                "susi_pref_{tag}_{}_{}",
                std::process::id(),
                now_unix()
            ));
            let _ = std::fs::create_dir_all(tmp.join("config"));
            let _ = std::fs::create_dir_all(tmp.join(".susi"));
            let keys = ["HOME", "XDG_CONFIG_HOME", "SUSI_XDG"];
            let prev = keys.map(|k| (k, std::env::var_os(k)));
            // SAFETY: test-only env override, serialized by env_test_lock and
            // restored in Drop.
            unsafe {
                std::env::set_var("HOME", &tmp);
                std::env::set_var("XDG_CONFIG_HOME", tmp.join("config"));
                std::env::set_var("SUSI_XDG", "0");
            }
            Self {
                _cd: CooldownFileGuard::new(tag),
                prev,
                tmp,
            }
        }
    }
    impl Drop for PrefSandbox {
        fn drop(&mut self) {
            // SAFETY: restores the values captured in `new`, under the same lock.
            unsafe {
                for (k, v) in &self.prev {
                    match v {
                        Some(v) => std::env::set_var(k, v),
                        None => std::env::remove_var(k),
                    }
                }
            }
            let _ = std::fs::remove_dir_all(&self.tmp);
        }
    }

    fn http_cloud(name: &str) -> crate::engines::http_provider::HttpProvider {
        crate::engines::http_provider::HttpProvider {
            name: name.into(),
            api_base: "https://api.example.com/v1".into(),
            model: "m".into(),
            protocol: crate::engines::http_provider::InferenceProtocol::OpenAiChat,
            api_key: String::new(),
        }
    }

    #[test]
    fn an_unfunded_preferred_cloud_hands_over_to_a_healthy_one_and_returns_on_recovery() {
        let _env = crate::engines::env_test_lock();
        let _sb = PrefSandbox::new("heal");
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(http_cloud("dryheal-big-model"));
        registry.register_provider(http_cloud("okheal-small-model"));
        InferenceRouter::save_preference(&RoutingPreference {
            preferred_cloud: Some("dryheal".into()),
            ..Default::default()
        });

        // The preferred vendor runs out of credit: its scope is quarantined and
        // the preference moves to the healthy cloud, remembering the original.
        InferenceRouter::record_provider_failure("dryheal-big-model");
        InferenceRouter::quarantine_vendor("dryheal-big-model", 600);
        InferenceRouter::heal_preferred_cloud_in(&registry, "dryheal-big-model");
        let pref = InferenceRouter::load_preference();
        assert_eq!(pref.preferred_cloud.as_deref(), Some("okheal"));
        assert_eq!(pref.auto_switched_from.as_deref(), Some("dryheal"));
        assert!(!InferenceRouter::matches_preferred_cloud(
            "dryheal-big-model"
        ));

        // While quarantined it is not probed; once the quarantine lapses it is
        // (a top-up is noticed with no operator action).
        assert!(!InferenceRouter::is_recovery_probe("dryheal-big-model"));
        assert!(InferenceRouter::clear_provider_cooldown(
            "dryheal-big-model"
        ));
        assert!(InferenceRouter::is_recovery_probe("dryheal-big-model"));
        assert!(!InferenceRouter::is_recovery_probe("okheal-small-model"));

        // It answers again: the user's preference is restored.
        InferenceRouter::record_provider_success("dryheal-big-model");
        let pref = InferenceRouter::load_preference();
        assert_eq!(pref.preferred_cloud.as_deref(), Some("dryheal"));
        assert_eq!(pref.auto_switched_from, None);
    }

    #[test]
    fn healing_leaves_routing_alone_when_nothing_is_healthy_or_vendor_differs() {
        let _env = crate::engines::env_test_lock();
        let _sb = PrefSandbox::new("noheal");
        let registry = crate::susi_core::registry::CapabilityRegistry::new();
        registry.register_provider(http_cloud("solocloud-only-model"));
        InferenceRouter::save_preference(&RoutingPreference {
            preferred_cloud: Some("solocloud".into()),
            ..Default::default()
        });
        // Only cloud available is the failing one: nothing healthy to move to.
        InferenceRouter::quarantine_vendor("solocloud-only-model", 600);
        InferenceRouter::heal_preferred_cloud_in(&registry, "solocloud-only-model");
        let pref = InferenceRouter::load_preference();
        assert_eq!(pref.preferred_cloud.as_deref(), Some("solocloud"));
        assert_eq!(pref.auto_switched_from, None);
        // A different vendor failing does not touch the preference.
        registry.register_provider(http_cloud("other-model"));
        InferenceRouter::heal_preferred_cloud_in(&registry, "other-model");
        assert_eq!(
            InferenceRouter::load_preference()
                .preferred_cloud
                .as_deref(),
            Some("solocloud")
        );
    }

    #[test]
    fn an_explicit_user_choice_overrides_an_automatic_switch() {
        let _env = crate::engines::env_test_lock();
        let _sb = PrefSandbox::new("explicit");
        InferenceRouter::save_preference(&RoutingPreference {
            preferred_cloud: Some("okheal".into()),
            auto_switched_from: Some("dryheal".into()),
            ..Default::default()
        });
        InferenceRouter::set_preferred_cloud("groq").unwrap();
        let pref = InferenceRouter::load_preference();
        assert_eq!(pref.preferred_cloud.as_deref(), Some("groq"));
        assert_eq!(pref.auto_switched_from, None);
    }

    #[test]
    fn transient_failures_do_not_escalate() {
        let _env = crate::engines::env_test_lock();
        let _cd = CooldownFileGuard::new("transient");
        let name = format!("flakyco-model-{}", std::process::id());
        for _ in 0..3 {
            InferenceRouter::record_failure(&name, "HTTP 429 Too Many Requests");
        }
        let left = InferenceRouter::provider_cooldown_until(&name)
            .map(|u| u.saturating_sub(now_unix()))
            .unwrap_or(0);
        assert!(
            left <= VENDOR_DOWN_COOLDOWN_SECS,
            "rate limits keep the short window: {left}"
        );
        InferenceRouter::record_provider_success(&name);
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
