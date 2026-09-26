//! Root `SusiConfig` dynamic registry and accessors.
use crate::susi_error::EaiResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::json_util::{
    atomic_write_json_pretty, merge_missing_registry_defaults, DynamicRegistry,
};
use super::types::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SusiConfig {
    #[serde(flatten)]
    pub settings: DynamicRegistry, // ALL settings are dynamic, loaded from config.default.json
}

impl Default for SusiConfig {
    fn default() -> Self {
        Self::bundled_defaults().clone()
    }
}

/// Host-contract ports live in `crate::susi_paths::ports` (compile-time). Bundled
/// JSON may still list them for documentation; they must not be backfilled
/// into `~/.susi/config.json` on heal.
const HOST_CONTRACT_PORT_KEYS: &[&str] = &[
    "gmcp_port",
    "gmcp_http_port",
    "gemi_port",
    "udp_discovery_port",
    "a2a_http_port",
];

impl SusiConfig {
    #[allow(clippy::expect_used)]
    fn bundled_defaults() -> &'static Self {
        static DEFAULTS: std::sync::OnceLock<SusiConfig> = std::sync::OnceLock::new();
        DEFAULTS.get_or_init(|| {
            serde_json::from_str(include_str!("../../../config/config.default.json"))
                .expect("bundled config.default.json must be valid JSON")
        })
    }

    /// Bundled defaults with host-contract port keys removed — used only for
    /// heal merges so polluted/legacy port fields are never re-persisted.
    fn heal_defaults() -> &'static DynamicRegistry {
        static DEFAULTS: std::sync::OnceLock<DynamicRegistry> = std::sync::OnceLock::new();
        DEFAULTS.get_or_init(|| {
            let mut settings = Self::bundled_defaults().settings.clone();
            for key in HOST_CONTRACT_PORT_KEYS {
                settings.remove(*key);
            }
            settings
        })
    }

    fn heal_in_place(cfg: &mut Self) -> bool {
        let mut changed = merge_missing_registry_defaults(&mut cfg.settings, Self::heal_defaults());
        // Bearer lives only in ~/.susi/api_token (0600). Never heal or
        // persist it into world-readable config.json.
        if cfg.settings.remove("api_auth_token").is_some() {
            changed = true;
        }
        changed
    }

    fn store() -> &'static super::versioned_store::VersionedJsonStore<Self> {
        static STORE: std::sync::OnceLock<super::versioned_store::VersionedJsonStore<SusiConfig>> =
            std::sync::OnceLock::new();
        STORE.get_or_init(super::versioned_store::VersionedJsonStore::new)
    }

    pub fn get_config_path(global_dir: &Path) -> PathBuf {
        global_dir.join("config.json")
    }

    /// Loads a user's persisted config.json and self-heals schema drift against
    /// the binary's bundled config.default.json: any key (at any nesting depth,
    /// including per-element within same-length arrays like model_ladder) that
    /// exists in the bundled default but is missing from the user's file is
    /// backfilled in memory and the merged result is written back to disk.
    /// Values the user already set are never touched. Without this, a fix that
    /// only lands in config.default.json (e.g. a new field on an existing key)
    /// silently never reaches an install whose config.json predates it.
    pub fn load(global_dir: &Path) -> EaiResult<Self> {
        Ok((*Self::load_arc(global_dir)?).clone())
    }

    /// Shared snapshot of the config (cache-hit is `Arc::clone`, not a deep copy).
    pub fn load_arc(global_dir: &Path) -> EaiResult<std::sync::Arc<Self>> {
        let path = Self::get_config_path(global_dir);
        Self::store().load_arc_with_healing(
            &path,
            || Ok(Self::default()),
            Self::heal_in_place,
            true,
        )
    }

    pub fn reload(global_dir: &Path) -> EaiResult<Self> {
        Ok((*Self::reload_arc(global_dir)?).clone())
    }

    pub fn reload_arc(global_dir: &Path) -> EaiResult<std::sync::Arc<Self>> {
        Self::store().reload_arc_with_healing(
            &Self::get_config_path(global_dir),
            || Ok(Self::default()),
            Self::heal_in_place,
            true,
        )
    }

    pub fn load_global() -> EaiResult<Self> {
        if let Some(cfg) = super::service::get_global() {
            return Ok(cfg);
        }
        Self::load(&crate::susi_paths::SusiDirs::config_dir())
    }

    /// Process-global config snapshot shared across hot request paths.
    pub fn load_global_arc() -> EaiResult<std::sync::Arc<Self>> {
        if let Some(cfg) = super::service::get_global() {
            return Ok(std::sync::Arc::new(cfg));
        }
        Self::load_arc(&crate::susi_paths::SusiDirs::config_dir())
    }

    /// Writes via a same-directory temp file + rename rather than a direct
    /// fs::write (which truncates before writing), so a concurrent reader —
    /// another process's CLI invocation, the daemon's own background cycle —
    /// never observes a torn/empty file. This matters more now that `load()`
    /// can itself trigger a save on schema-drift backfill, making concurrent
    /// writers to the same config.json from multiple processes routine rather
    /// than rare.
    pub fn save(&self, global_dir: &Path) -> EaiResult<()> {
        if global_dir == crate::susi_paths::SusiDirs::config_dir()
            && super::service::save_global(self)
        {
            return Ok(());
        }
        fs::create_dir_all(global_dir)?;
        atomic_write_json_pretty(&Self::get_config_path(global_dir), self)
    }

    // === TYPED ACCESSORS - No hardcoded fields, dynamic getters with defaults ===
    pub fn get<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Option<T> {
        let v = self.settings.get(key)?;
        T::deserialize(v).ok()
    }

    /// Falls back to the bundled config.default.json's value for `key` (not a
    /// zero-value literal duplicated in Rust) when a user's ~/.susi/config.json
    /// predates this key or omits it. This is the *only* fallback path for every
    /// accessor below — config.default.json is the single source of truth for
    /// every default; there is no second, Rust-side copy of any value that
    /// could silently drift out of sync with it (as several of these already
    /// had: gmcp_http_port/gemi_port/udp_discovery_port were scrambled between
    /// their Rust literal and config.default.json, max_stdin_size_bytes was off
    /// by 100x, reflex_training_threshold by 10x, and alpha_weights_url/
    /// mcp_registry_url's Rust fallback was an empty string).
    fn get_or_bundled_default<T: for<'de> Deserialize<'de> + Default>(&self, key: &str) -> T {
        self.get(key)
            .unwrap_or_else(|| Self::bundled_defaults().get(key).unwrap_or_default())
    }

    /// Uniform shift applied to every host-contract port. One knob — a
    /// second instance or a nonstandard host layout gets clean ports while
    /// the relative contract shape stays fixed (the legacy per-port config
    /// keys stay ignored: scrambled individual ports were the incident that
    /// made the contract compile-time). `SUSI_PORT_OFFSET` env wins over the
    /// `port_offset` config key.
    pub fn port_offset(&self) -> u16 {
        std::env::var("SUSI_PORT_OFFSET")
            .ok()
            .and_then(|v| v.trim().parse::<u16>().ok())
            .unwrap_or_else(|| self.get_or_bundled_default("port_offset"))
    }
    // Public substrate ports are a hard contract with external clients:
    // canonical base + the uniform offset. Individual per-port overrides are
    // deliberately impossible (see port_offset).
    pub fn gmcp_port(&self) -> u16 {
        crate::susi_paths::ports::GMCP.saturating_add(self.port_offset())
    }
    pub fn gmcp_http_port(&self) -> u16 {
        crate::susi_paths::ports::GMCP_HTTP.saturating_add(self.port_offset())
    }
    pub fn gemi_port(&self) -> u16 {
        crate::susi_paths::ports::GEMI.saturating_add(self.port_offset())
    }
    pub fn udp_discovery_port(&self) -> u16 {
        crate::susi_paths::ports::UDP_DISCOVERY.saturating_add(self.port_offset())
    }
    pub fn a2a_http_port(&self) -> u16 {
        crate::susi_paths::ports::A2A_HTTP.saturating_add(self.port_offset())
    }
    pub fn execution_lease_secs(&self) -> u64 {
        self.get_or_bundled_default("execution_lease_secs")
    }
    pub fn max_concurrent_agents(&self) -> usize {
        self.get_or_bundled_default("max_concurrent_agents")
    }
    pub fn trust_level(&self) -> String {
        self.get_or_bundled_default("trust_level")
    }
    pub fn max_stdin_size_bytes(&self) -> usize {
        self.get_or_bundled_default("max_stdin_size_bytes")
    }
    pub fn max_rpc_body_bytes(&self) -> usize {
        self.get_or_bundled_default("max_rpc_body_bytes")
    }
    pub fn allow_origin(&self) -> String {
        self.get_or_bundled_default("allow_origin")
    }
    /// Bearer token required on world-facing HTTP surfaces (GMCP HTTP, GEMI
    /// REST). Prefer the dedicated `~/.susi/api_token` file (0600); fall back to
    /// a legacy `settings.api_auth_token` value only for migration.
    pub fn api_auth_token(&self) -> String {
        let token_path = crate::susi_paths::SusiDirs::config_dir().join("api_token");
        // Mtime-cached read — NetGuard calls this on every request.
        if let Some(raw) = super::cluster_key::cached_file_bytes(&token_path) {
            if let Ok(from_file) = String::from_utf8(raw) {
                let trimmed = from_file.trim().to_string();
                if !trimmed.is_empty() {
                    return trimmed;
                }
            }
        }
        self.get_or_bundled_default("api_auth_token")
    }

    /// Ensure `api_auth_token` is non-empty on the host substrate. Generates a
    /// 32-byte hex secret on first run, persists it **only** to
    /// `~/.susi/api_token` (0600) — never into world-readable `config.json`.
    pub fn ensure_api_auth_token_seeded() -> String {
        let global_dir = crate::susi_paths::SusiDirs::config_dir();
        let token_path = global_dir.join("api_token");
        if let Ok(existing) = fs::read_to_string(&token_path) {
            let trimmed = existing.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        // Migrate a legacy config.json copy into the private file, then strip it.
        let mut cfg = Self::load_global().unwrap_or_default();
        let legacy = cfg
            .settings
            .get("api_auth_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let token = if !legacy.is_empty() {
            legacy
        } else {
            let mut raw = [0u8; 32];
            if getrandom::fill(&mut raw).is_err() {
                // Extremely unlikely; fall back to a process-unique but weaker seed.
                let fallback = format!(
                    "{}:{}:{}",
                    std::process::id(),
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos())
                        .unwrap_or(0),
                    global_dir.display()
                );
                use sha2::{Digest, Sha256};
                let digest = Sha256::digest(fallback.as_bytes());
                raw.copy_from_slice(&digest[..32]);
            }
            hex::encode(raw)
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let _ = fs::create_dir_all(&global_dir);
            let _ = fs::set_permissions(&global_dir, fs::Permissions::from_mode(0o700));
            // Create with 0600 atomically — never write-then-chmod (umask window).
            let mut options = fs::OpenOptions::new();
            options.write(true).create(true).truncate(true).mode(0o600);
            match options.open(&token_path).and_then(|mut f| {
                use std::io::Write;
                f.write_all(token.as_bytes())?;
                f.sync_all()
            }) {
                Ok(()) => {
                    let _ = fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600));
                }
                Err(_) => {
                    let _ = fs::write(&token_path, &token);
                    let _ = fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600));
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = fs::write(&token_path, &token);
        }
        if cfg.settings.remove("api_auth_token").is_some() {
            let _ = cfg.save(&global_dir);
        }
        eprintln!(
            "[Zero-Trust] Seeded host API bearer token → {} (required on HTTP {}/{}/{}/{})",
            token_path.display(),
            cfg.gmcp_port(),
            cfg.gemi_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port()
        );
        token
    }

    /// Max requests per IP per 60s window on world-facing HTTP surfaces. 0
    /// disables rate limiting.
    pub fn rate_limit_per_minute(&self) -> u32 {
        self.get_or_bundled_default("rate_limit_per_minute")
    }

    pub fn default_model(&self) -> String {
        self.get_or_bundled_default("default_model")
    }
    pub fn default_engine(&self) -> String {
        self.get_or_bundled_default("default_engine")
    }
    pub fn mcp_registry_url(&self) -> String {
        self.get_or_bundled_default("mcp_registry_url")
    }
    pub fn bootstrap_mcp_servers<T: for<'de> Deserialize<'de> + Default>(&self) -> T {
        self.get("bootstrap_mcp_servers").unwrap_or_default()
    }
    pub fn local_scan_paths(&self) -> Vec<String> {
        self.get("local_scan_paths").unwrap_or_default()
    }
    pub fn discoverable_assets<T: for<'de> Deserialize<'de> + Default>(&self) -> T {
        self.get("discoverable_assets").unwrap_or_default()
    }
    pub fn governance(&self) -> GovernancePatterns {
        self.get_or_bundled_default("governance")
    }
    pub fn admin_pulses(&self) -> AdminPulsesConfig {
        self.get_or_bundled_default("admin_pulses")
    }
    pub fn intent_classify(&self) -> IntentClassifyConfig {
        self.get_or_bundled_default("intent_classify")
    }
    pub fn alpha_weights_url(&self) -> String {
        self.get_or_bundled_default("alpha_weights_url")
    }
    pub fn alpha_weights_filename(&self) -> String {
        self.get_or_bundled_default("alpha_weights_filename")
    }
    pub fn tokenizer_filename(&self) -> String {
        self.get_or_bundled_default("tokenizer_filename")
    }
    pub fn hf_base_url(&self) -> String {
        self.get_or_bundled_default("hf_base_url")
    }
    pub fn qdrant_url(&self) -> String {
        self.get_or_bundled_default("qdrant_url")
    }
    pub fn crates_io_api_url(&self) -> String {
        self.get_or_bundled_default("crates_io_api_url")
    }
    pub fn inference_endpoints(&self) -> InferenceEndpointsConfig {
        self.get_or_bundled_default("inference_endpoints")
    }
    pub fn agent_rank_threshold(&self) -> f32 {
        self.get_or_bundled_default("agent_rank_threshold")
    }
    pub fn cloud_scout_timeout_secs(&self) -> u64 {
        self.get_or_bundled_default("cloud_scout_timeout_secs")
    }
    pub fn model_provisioning_wait_secs(&self) -> u64 {
        self.get_or_bundled_default("model_provisioning_wait_secs")
    }
    /// Interval for re-probing local inference engines and MCP tools after
    /// daemon start. `0` disables the background rediscovery loop.
    pub fn capability_rediscovery_secs(&self) -> u64 {
        self.get_or_bundled_default("capability_rediscovery_secs")
    }
    pub fn reflex_training_threshold(&self) -> usize {
        self.get_or_bundled_default("reflex_training_threshold")
    }
    pub fn model_lifecycle(&self) -> ModelLifecycleConfig {
        self.get_or_bundled_default("model_lifecycle")
    }
    pub fn inference_routing(&self) -> InferenceRoutingConfig {
        self.get_or_bundled_default("inference_routing")
    }

    pub fn privacy(&self) -> PrivacyConfig {
        self.get_or_bundled_default("privacy")
    }

    /// Leading external coding agents mounted as pluggable swarm peers.
    ///
    /// `config.default.json` / host `config.json` may list CLI/HTTP/A2A peers and
    /// optional managed overlays. **Managed** peers are also admitted from the
    /// execution-agents + agent-engines catalogs (`peer_name`) so adding a
    /// catalog entry does not require duplicating it under `external_peer_agents`.
    pub fn external_peer_agents(&self) -> Vec<ExternalPeerAgentSpec> {
        let mut peers: Vec<ExternalPeerAgentSpec> =
            self.get_or_bundled_default("external_peer_agents");
        // Migrate only exact historical built-ins. Preserve user-edited commands,
        // endpoints, argv, credentials and timeouts. New bundled peers are admitted
        // for existing installations as well as fresh installs.
        let legacy: Vec<ExternalPeerAgentSpec> =
            serde_json::from_str(include_str!("../../../config/execution-peers.legacy.json"))
                .unwrap_or_default();
        let defaults: Vec<ExternalPeerAgentSpec> =
            Self::default().get_or_bundled_default("external_peer_agents");
        for peer in &mut peers {
            if legacy.iter().any(|old| peer_exact_legacy_match(old, peer)) {
                if let Some(updated) = defaults.iter().find(|p| p.name == peer.name) {
                    *peer = updated.clone();
                }
            }
        }
        for default in defaults.into_iter().filter(|p| p.protocol == "managed") {
            if !peers.iter().any(|p| p.name == default.name) {
                peers.push(default);
            }
        }
        for catalog_peer in managed_peers_from_catalogs() {
            if !peers.iter().any(|p| p.name == catalog_peer.name) {
                peers.push(catalog_peer);
            }
        }
        peers
    }

    /// Leading models catalog (~50 curated); live `/models` discovery remains
    /// unbounded. Override via config key `model_catalog`.
    #[allow(clippy::expect_used)]
    pub fn model_catalog(&self) -> Vec<ModelCatalogEntry> {
        if let Some(cfg) = self.get::<ModelCatalogConfig>("model_catalog") {
            if !cfg.models.is_empty() {
                return cfg.models;
            }
        }
        static BUNDLED: std::sync::OnceLock<Vec<ModelCatalogEntry>> = std::sync::OnceLock::new();
        BUNDLED
            .get_or_init(|| {
                let file: ModelCatalogConfig = serde_json::from_str(include_str!(
                    "../../../config/models.catalog.default.json"
                ))
                .expect("bundled models.catalog.default.json must be valid");
                file.models
            })
            .clone()
    }

    /// Bundled leading MCP scout registry (~100 real packages). Remote scout
    /// can refresh `global_mcp_registry.json`; not all are hot-plugged at once.
    pub fn leading_mcp_registry_json() -> &'static str {
        include_str!("../../../config/mcp.registry.default.json")
    }

    /// The explicitly configured ladder, or empty if none is set. This is a
    /// pure config accessor with no network/discovery fallback — `sandbox`
    /// must not depend on `gemi::hf_discovery` for that (it would create a
    /// cycle, since `gemi` already depends on `sandbox` for `SusiConfig`
    /// itself). Callers that want the dynamic-discovery fallback when this
    /// is empty should use `gemi::hf_discovery::resolve_model_ladder`.
    pub fn model_ladder(&self) -> Vec<ModelLadderConfigStep> {
        self.get_or_bundled_default("model_ladder")
    }
    pub fn default_fallback_model(&self) -> ModelLadderConfigStep {
        self.get_or_bundled_default("default_fallback_model")
    }
    pub fn admin_command_routing(&self) -> HashMap<String, Vec<Vec<String>>> {
        self.get_or_bundled_default("admin_command_routing")
    }
    pub fn agent_routing(&self) -> HashMap<String, Vec<String>> {
        self.get_or_bundled_default("agent_routing")
    }
    pub fn model_scoring_heuristics(&self) -> ModelScoringHeuristics {
        self.get_or_bundled_default("model_scoring_heuristics")
    }
    pub fn memory_experience_heuristics(&self) -> MemoryExperienceHeuristics {
        self.get_or_bundled_default("memory_experience_heuristics")
    }
    pub fn sandbox_image(&self) -> String {
        self.get_or_bundled_default("sandbox_image")
    }
    /// Special-token IDs that terminate generation (previously hardcoded as
    /// `1 | 2 | 32000 | 151643` directly in the inference loop — vendor/
    /// tokenizer-specific magic numbers with zero comment on which model
    /// family each belonged to).
    pub fn eos_token_ids(&self) -> Vec<u32> {
        self.get_or_bundled_default("eos_token_ids")
    }
    /// Maximum simultaneous HTTP completion jobs; excess clients receive HTTP 429.
    pub fn gemi_max_concurrent_requests(&self) -> usize {
        self.get_or_bundled_default::<usize>("gemi_max_concurrent_requests")
            .max(1)
    }
    pub fn max_generation_tokens(&self) -> usize {
        self.get_or_bundled_default("max_generation_tokens")
    }
    /// Penalty divisor applied to already-seen tokens' logits before argmax
    /// (llama.cpp convention: >1.0 discourages repetition, 1.0 disables it).
    /// Pure greedy decoding with no penalty readily loops on tiny models -
    /// observed live on qwen2.5-0.5b as a "Name three colors" response
    /// degenerating into an infinitely-nesting repeated JSON structure.
    pub fn repeat_penalty(&self) -> f32 {
        self.get_or_bundled_default("repeat_penalty")
    }
    /// How many of the most recent tokens (prompt + generated) count toward
    /// the repeat penalty above.
    pub fn repeat_last_n(&self) -> usize {
        self.get_or_bundled_default("repeat_last_n")
    }
    /// Whether the generation loop may draft-and-verify multiple tokens per
    /// target-model forward pass (see `gemi::speculative`) instead of one
    /// token at a time. Verification always falls back to the target
    /// model's own greedy choice on the first disagreement, so the emitted
    /// token sequence is provably identical to plain greedy decoding
    /// either way (see `speculative::tests::
    /// test_speculative_output_matches_plain_greedy_decoding`) - this only
    /// gates whether the batched path is attempted, never behavior.
    ///
    /// Defaults to `false`: measured live on this host (RTX 2000 Ada
    /// Laptop, 8GB VRAM), a fully GPU-resident Qwen2.5-7B target with a
    /// 0.5B draft ran at 4.36 tok/s versus 18.47 tok/s for plain greedy
    /// decoding of the same model - a 4x regression, not the hoped-for
    /// speedup. CUDA's quantized matmul genuinely gets cheaper per-token
    /// with a wider batch (confirmed via `cudarc`'s `fast_mmq` threshold),
    /// but that saving is consumed by the extra host/kernel-launch round
    /// trips this adds: the draft model still needs `speculative_draft_tokens
    /// - 1` *sequential* single-token forwards per round (autoregressive,
    ///   can't be batched), plus a resync forward, on top of the target's
    ///   batched verify call - more total round trips than the classic loop
    ///   for the same tokens, and per-call host/launch overhead dominates
    ///   over raw compute at these model sizes on this stack. Left
    ///   configurable (and the implementation fully correctness-tested) in
    ///   case a future candle version, different hardware, or a larger
    ///   draft_chunk changes this trade-off - but never default-on without
    ///   remeasuring.
    pub fn speculative_decoding_enabled(&self) -> bool {
        self.get_or_bundled_default("speculative_decoding_enabled")
    }
    /// How many tokens the draft model proposes ahead of the target model
    /// per verification round. Larger values amortize more work into each
    /// batched target-model forward pass (raising GPU utilization) but waste
    /// more of that pass whenever the draft diverges early.
    pub fn speculative_draft_tokens(&self) -> usize {
        self.get_or_bundled_default("speculative_draft_tokens")
    }
    /// Upper bound (prompt + generated tokens combined) the native Qwen2
    /// engine's KV cache preallocates per layer at model-load time (see
    /// `qwen2_split::ModelWeights::from_gguf_split`). The cache is written
    /// into in place as generation proceeds rather than reallocated every
    /// token, so this must be decided once, before any particular
    /// request's prompt length is known - sized generously above
    /// `max_generation_tokens` to comfortably cover realistic prompts.
    /// Exceeding it mid-generation is a hard error (not silent truncation
    /// or a fallback to reallocation): raise this value if it's ever hit.
    pub fn kv_cache_capacity_tokens(&self) -> usize {
        self.get_or_bundled_default("kv_cache_capacity_tokens")
    }
    /// Risk substrings that block reasoning OUTPUT before it's returned as a
    /// final answer — a distinct security layer from `governance()`'s
    /// `destructive_commands` (which gates COMMANDS before execution); the
    /// two lists overlap in spirit but not in content.
    pub fn axiomatic_risk_patterns(&self) -> Vec<String> {
        self.get_or_bundled_default("axiomatic_risk_patterns")
    }
    pub fn model_scan_exclude_dirs(&self) -> Vec<String> {
        self.get_or_bundled_default("model_scan_exclude_dirs")
    }
    /// Deliberately narrower than `model_scan_exclude_dirs`: this gates
    /// `recursive_scan_model_dir_for_paths`, which `deep_scan_home_and_register`
    /// invokes *with* `.cache`/`.local` etc. as starting directories (they're
    /// dot-prefixed, so a separate first pass has to seed them explicitly) — if
    /// this list excluded them too, the scan would refuse to look inside its
    /// own seeded roots.
    pub fn model_discovery_exclude_dirs(&self) -> Vec<String> {
        self.get_or_bundled_default("model_discovery_exclude_dirs")
    }
    pub fn home_scan_root_exclude_dirs(&self) -> Vec<String> {
        self.get_or_bundled_default("home_scan_root_exclude_dirs")
    }
    pub fn model_file_extensions(&self) -> Vec<String> {
        self.get_or_bundled_default("model_file_extensions")
    }
    pub fn model_file_min_bytes(&self) -> u64 {
        self.get_or_bundled_default("model_file_min_bytes")
    }
}

/// Exact field equality for legacy peer migrate (avoids double `serde_json::to_value`).
fn peer_exact_legacy_match(a: &ExternalPeerAgentSpec, b: &ExternalPeerAgentSpec) -> bool {
    a.name == b.name
        && a.description == b.description
        && a.protocol == b.protocol
        && a.api_base == b.api_base
        && a.model == b.model
        && a.detect_bins == b.detect_bins
        && a.command == b.command
        && a.args == b.args
        && a.timeout_secs == b.timeout_secs
        && a.api_key_env == b.api_key_env
}

/// Minimal catalog row used to admit managed swarm peers from extension packs.
#[derive(Debug, Deserialize)]
struct CatalogPeerHint {
    peer_name: String,
    name: String,
    #[serde(flatten, default)]
    _extra: HashMap<String, serde_json::Value>,
}

fn managed_peers_from_catalogs() -> Vec<ExternalPeerAgentSpec> {
    let mut out = Vec::new();
    let catalogs = [
        (
            "execution-agents.json",
            include_str!("../../../config/execution-agents.json"),
        ),
        (
            "agent-engines.json",
            include_str!("../../../config/agent-engines.json"),
        ),
    ];
    for (logical, bundled) in catalogs {
        let entries: Vec<CatalogPeerHint> =
            super::extensions::load_json_or_bundled(logical, bundled);
        for entry in entries {
            let peer = entry.peer_name.trim();
            if peer.is_empty() {
                continue;
            }
            out.push(ExternalPeerAgentSpec {
                name: peer.to_string(),
                description: format!(
                    "{} managed executor/framework; setup via susi agents / susi frameworks.",
                    entry.name
                ),
                protocol: "managed".into(),
                ..Default::default()
            });
        }
    }
    out
}

// Compatibility shim for old code that accessed fields directly
impl std::ops::Deref for SusiConfig {
    type Target = DynamicRegistry;
    fn deref(&self) -> &Self::Target {
        &self.settings
    }
}
