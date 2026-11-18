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

/// Host-contract ports live in `susi_paths::ports` (compile-time). Bundled
/// JSON may still list them for documentation; they must not be backfilled
/// into `~/.susi/config.json` on heal.
const HOST_CONTRACT_PORT_KEYS: &[&str] = &[
    "gmcp_port",
    "gmcp_http_port",
    "gemi_port",
    "udp_discovery_port",
    "a2a_http_port",
];

// Keep the public typed wrappers and their bundled-default lookup keys together.
// Special accessors with file, environment, migration or validation policy stay explicit.
macro_rules! bundled_accessors {
    ($($(#[$meta:meta])* $name:ident: $ty:ty;)*) => {
        $(
            $(#[$meta])*
            pub fn $name(&self) -> $ty {
                self.get_or_bundled_default(stringify!($name))
            }
        )*
    };
}

impl SusiConfig {
    // Mandate 42: safe - parses the include_str! bundled default config; a
    // parse failure is a build-time bug caught by any test run.
    #[allow(clippy::expect_used)]
    fn bundled_defaults() -> &'static Self {
        static DEFAULTS: std::sync::OnceLock<SusiConfig> = std::sync::OnceLock::new();
        DEFAULTS.get_or_init(|| {
            serde_json::from_str(include_str!("../../../config/config.default.json"))
                .expect("bundled config.default.json must be valid JSON")
        })
    }

    /// Bundled defaults with host-contract port keys removed — used for
    /// heal merges and schema validation so polluted/legacy port fields are
    /// never re-persisted or required.
    pub fn heal_defaults() -> &'static DynamicRegistry {
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
    /// A missing file is seeded with the healed defaults: the caller named the
    /// directory, so it owns it.
    pub fn load_arc(global_dir: &Path) -> EaiResult<std::sync::Arc<Self>> {
        Self::load_arc_seeding(global_dir, true)
    }

    fn load_arc_seeding(global_dir: &Path, seed_missing: bool) -> EaiResult<std::sync::Arc<Self>> {
        let path = Self::get_config_path(global_dir);
        Self::store().load_arc_with_healing_policy(
            &path,
            || Ok(Self::default()),
            Self::heal_in_place,
            super::versioned_store::ReadPolicy {
                strict_parse: true,
                seed_missing,
            },
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

    /// The global config, resolved through the process environment. A read
    /// never seeds `config.json`: the directory comes from `HOME`/`SUSI_HOME`,
    /// which every thread of the process shares, so a reader that wrote
    /// defaults there could overwrite a file another thread (a test with a
    /// swapped home, a CLI invocation mid-save) owns. `susi start` and the
    /// setup workflow save the file explicitly.
    pub fn load_global() -> EaiResult<Self> {
        Ok((*Self::load_global_arc()?).clone())
    }

    /// Process-global config snapshot shared across hot request paths.
    pub fn load_global_arc() -> EaiResult<std::sync::Arc<Self>> {
        if let Some(cfg) = super::service::get_global() {
            return Ok(std::sync::Arc::new(cfg));
        }
        Self::load_arc_seeding(&susi_paths::SusiDirs::config_dir(), false)
    }

    /// Writes via a same-directory temp file + rename rather than a direct
    /// fs::write (which truncates before writing), so a concurrent reader —
    /// another process's CLI invocation, the daemon's own background cycle —
    /// never observes a torn/empty file. This matters more now that `load()`
    /// can itself trigger a save on schema-drift backfill, making concurrent
    /// writers to the same config.json from multiple processes routine rather
    /// than rare.
    pub fn save(&self, global_dir: &Path) -> EaiResult<()> {
        if global_dir == susi_paths::SusiDirs::config_dir() && super::service::save_global(self) {
            return Ok(());
        }
        fs::create_dir_all(global_dir)?;
        atomic_write_json_pretty(&Self::get_config_path(global_dir), self)?;
        // The store is keyed on mtime alone. A rewrite that lands inside the
        // filesystem's timestamp granularity keeps that mtime, and the next
        // load would return the snapshot from before this write.
        *Self::store()
            .cache
            .write()
            .unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
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
    /// by 100x, reflex_training_threshold by 10x, and alpha_weights_url's
    /// Rust fallback was an empty string, as was the since-removed remote
    /// MCP registry URL's).
    fn get_or_bundled_default<T: for<'de> Deserialize<'de> + Default>(&self, key: &str) -> T {
        self.get(key)
            .unwrap_or_else(|| Self::bundled_defaults().get(key).unwrap_or_default())
    }

    bundled_accessors! {
        execution_lease_secs: u64;
        max_concurrent_agents: usize;
        trust_level: String;
        max_stdin_size_bytes: usize;
        max_rpc_body_bytes: usize;
        allow_origin: String;
        /// Max requests per IP per 60s window on world-facing HTTP surfaces. 0
        /// disables rate limiting.
        rate_limit_per_minute: u32;
        default_model: String;
        default_engine: String;
        governance: GovernancePatterns;
        intent_classify: IntentClassifyConfig;
        alpha_weights_url: String;
        alpha_weights_filename: String;
        tokenizer_filename: String;
        hf_base_url: String;
        qdrant_url: String;
        crates_io_api_url: String;
        inference_endpoints: InferenceEndpointsConfig;
        agent_rank_threshold: f32;
        cloud_scout_timeout_secs: u64;
        model_provisioning_wait_secs: u64;
        /// Interval for re-probing local inference engines and MCP tools after
        /// daemon start. `0` disables the background rediscovery loop.
        capability_rediscovery_secs: u64;
        reflex_training_threshold: usize;
        model_lifecycle: ModelLifecycleConfig;
        inference_routing: InferenceRoutingConfig;
        privacy: PrivacyConfig;
        /// The explicitly configured ladder, or empty if none is set. This is a
        /// pure config accessor with no network/discovery fallback — `sandbox`
        /// must not depend on `gemi::hf_discovery` for that (it would create a
        /// cycle, since `gemi` already depends on `sandbox` for `SusiConfig`
        /// itself). Callers that want the dynamic-discovery fallback when this
        /// is empty should use `gemi::hf_discovery::resolve_model_ladder`.
        model_ladder: Vec<ModelLadderConfigStep>;
        default_fallback_model: ModelLadderConfigStep;
        admin_command_routing: HashMap<String, Vec<Vec<String>>>;
        agent_routing: HashMap<String, Vec<String>>;
        model_scoring_heuristics: ModelScoringHeuristics;
        memory_experience_heuristics: MemoryExperienceHeuristics;
        sandbox_image: String;
        /// Special-token IDs that terminate generation (previously hardcoded as
        /// `1 | 2 | 32000 | 151643` directly in the inference loop — vendor/
        /// tokenizer-specific magic numbers with zero comment on which model
        /// family each belonged to).
        eos_token_ids: Vec<u32>;
        max_generation_tokens: usize;
        /// Penalty divisor applied to already-seen tokens' logits before argmax
        /// (llama.cpp convention: >1.0 discourages repetition, 1.0 disables it).
        /// Pure greedy decoding with no penalty readily loops on tiny models -
        /// observed live on qwen2.5-0.5b as a "Name three colors" response
        /// degenerating into an infinitely-nesting repeated JSON structure.
        repeat_penalty: f32;
        /// How many of the most recent tokens (prompt + generated) count toward
        /// the repeat penalty above.
        repeat_last_n: usize;
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
        speculative_decoding_enabled: bool;
        /// How many tokens the draft model proposes ahead of the target model
        /// per verification round. Larger values amortize more work into each
        /// batched target-model forward pass (raising GPU utilization) but waste
        /// more of that pass whenever the draft diverges early.
        speculative_draft_tokens: usize;
        /// Upper bound (prompt + generated tokens combined) the native Qwen2
        /// engine's KV cache preallocates per layer at model-load time (see
        /// `qwen2_split::ModelWeights::from_gguf_split`). The cache is written
        /// into in place as generation proceeds rather than reallocated every
        /// token, so this must be decided once, before any particular
        /// request's prompt length is known - sized generously above
        /// `max_generation_tokens` to comfortably cover realistic prompts.
        /// Exceeding it mid-generation is a hard error (not silent truncation
        /// or a fallback to reallocation): raise this value if it's ever hit.
        kv_cache_capacity_tokens: usize;
        /// Risk substrings that block reasoning OUTPUT before it's returned as a
        /// final answer — a distinct security layer from `governance()`'s
        /// `destructive_commands` (which gates COMMANDS before execution); the
        /// two lists overlap in spirit but not in content.
        axiomatic_risk_patterns: Vec<String>;
        model_scan_exclude_dirs: Vec<String>;
        /// Deliberately narrower than `model_scan_exclude_dirs`: this gates
        /// `recursive_scan_model_dir_for_paths`, which `deep_scan_home_and_register`
        /// invokes *with* `.cache`/`.local` etc. as starting directories (they're
        /// dot-prefixed, so a separate first pass has to seed them explicitly) — if
        /// this list excluded them too, the scan would refuse to look inside its
        /// own seeded roots.
        model_discovery_exclude_dirs: Vec<String>;
        home_scan_root_exclude_dirs: Vec<String>;
        model_file_extensions: Vec<String>;
        model_file_min_bytes: u64;
    }

    /// Uniform shift applied to every host-contract port. One knob — a
    /// second instance or a nonstandard host layout gets clean ports while
    /// the relative contract shape stays fixed (the legacy per-port config
    /// keys stay ignored: scrambled individual ports were the incident that
    /// made the contract compile-time).
    ///
    /// Resolution ([`susi_paths::ports::resolve_offset`]): an explicit
    /// offset ([`Self::explicit_port_offset`]) wins; otherwise the offset the
    /// live daemon of this instance published in `ports.json` (an automatic
    /// fallback block, or 0); otherwise 0.
    pub fn port_offset(&self) -> u16 {
        susi_paths::ports::resolve_offset(
            susi_paths::ports::explicit_env_offset(),
            self.configured_port_offset(),
            susi_paths::ports::live_daemon_offset,
        )
    }

    /// The offset an operator pinned: `SUSI_PORT_OFFSET` (any value that
    /// parses, `0` included, wins), else a non-zero `port_offset` config key.
    /// `None` means nothing is pinned and the daemon may fall back to a free
    /// block. A config value of 0 is not a pin: heal backfills
    /// `port_offset: 0` into every `config.json`, so 0 cannot tell an
    /// operator's choice from the default.
    pub fn explicit_port_offset(&self) -> Option<u16> {
        susi_paths::ports::explicit_env_offset().or_else(|| self.configured_port_offset())
    }

    /// The non-zero `port_offset` config value, if any.
    fn configured_port_offset(&self) -> Option<u16> {
        let configured: u16 = self.get_or_bundled_default("port_offset");
        (configured > 0).then_some(configured)
    }
    // Public substrate ports are a hard contract with external clients:
    // canonical base + the uniform offset. Individual per-port overrides are
    // deliberately impossible (see port_offset).
    pub fn gmcp_port(&self) -> u16 {
        susi_paths::ports::GMCP.saturating_add(self.port_offset())
    }
    pub fn gmcp_http_port(&self) -> u16 {
        susi_paths::ports::GMCP_HTTP.saturating_add(self.port_offset())
    }
    pub fn gemi_port(&self) -> u16 {
        susi_paths::ports::GEMI.saturating_add(self.port_offset())
    }
    pub fn udp_discovery_port(&self) -> u16 {
        susi_paths::ports::UDP_DISCOVERY.saturating_add(self.port_offset())
    }
    pub fn a2a_http_port(&self) -> u16 {
        susi_paths::ports::A2A_HTTP.saturating_add(self.port_offset())
    }
    /// Swarm gossip UDP. Offset with the rest of the instance; not a host-contract port.
    pub fn gossip_port(&self) -> u16 {
        susi_paths::ports::GOSSIP.saturating_add(self.port_offset())
    }
    /// Bearer token required on world-facing HTTP surfaces (GMCP HTTP, GEMI
    /// REST). Prefer the dedicated `~/.susi/api_token` file (0600); fall back to
    /// a legacy `settings.api_auth_token` value only for migration.
    pub fn api_auth_token(&self) -> String {
        let token_path = susi_paths::host_token_path();
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
    ///
    /// # Errors
    /// Fails closed if the token cannot be read, secured, installed, or migrated
    /// out of `config.json`.
    pub fn ensure_api_auth_token_seeded() -> EaiResult<String> {
        let global_dir = susi_paths::SusiDirs::config_dir();
        super::json_util::create_private_dir(&global_dir)?;
        let token_path = global_dir.join("api_token");
        let mut cfg = Self::load(&global_dir)?;
        let legacy = cfg
            .settings
            .get("api_auth_token")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let metadata = match fs::symlink_metadata(&token_path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(crate::susi_error::EaiError::io(format!(
                    "inspect host API token file: {error}"
                )));
            }
        };
        let token = if let Some(metadata) = metadata {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(crate::susi_error::EaiError::authentication(
                    "host API token path must be a regular file",
                ));
            }
            let token = fs::read_to_string(&token_path)?;
            let token = token.trim().to_string();
            if token.is_empty() {
                return Err(crate::susi_error::EaiError::authentication(
                    "host API token file exists but is empty",
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;
            }
            token
        } else {
            let token = if !legacy.is_empty() {
                legacy
            } else {
                let mut raw = [0u8; 32];
                getrandom::fill(&mut raw).map_err(|error| {
                    crate::susi_error::EaiError::authentication(format!(
                        "generate host API bearer token: {error}"
                    ))
                })?;
                susi_paths::hex::encode(raw)
            };
            match super::json_util::install_private_file(&token_path, token.as_bytes())? {
                true => token,
                false => {
                    let metadata = fs::symlink_metadata(&token_path)?;
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        return Err(crate::susi_error::EaiError::authentication(
                            "host API token path must be a regular file",
                        ));
                    }
                    let winner = fs::read_to_string(&token_path)?;
                    let winner = winner.trim().to_string();
                    if winner.is_empty() {
                        return Err(crate::susi_error::EaiError::authentication(
                            "concurrent host API token installation left an empty file",
                        ));
                    }
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        fs::set_permissions(&token_path, fs::Permissions::from_mode(0o600))?;
                    }
                    winner
                }
            }
        };
        if cfg.settings.remove("api_auth_token").is_some() {
            cfg.save(&global_dir)?;
        }
        eprintln!(
            "[Zero-Trust] Host API bearer token ready at {} (required on HTTP {}/{}/{}/{})",
            token_path.display(),
            cfg.gmcp_port(),
            cfg.gemi_port(),
            cfg.gmcp_http_port(),
            cfg.a2a_http_port()
        );
        Ok(token)
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

    /// Maximum simultaneous HTTP completion jobs; excess clients receive HTTP 429.
    pub fn gemi_max_concurrent_requests(&self) -> usize {
        self.get_or_bundled_default::<usize>("gemi_max_concurrent_requests")
            .max(1)
    }
    /// Seconds a loaded model may sit unused before the inference cache's
    /// sweeper unloads it and returns its memory (VRAM included). `0`
    /// disables idle unloading. `SUSI_MODEL_IDLE_TIMEOUT_SECS` env wins over
    /// the config key.
    pub fn model_idle_timeout_secs(&self) -> u64 {
        std::env::var("SUSI_MODEL_IDLE_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or_else(|| self.get_or_bundled_default("model_idle_timeout_secs"))
    }
}

/// The one credential redactor for text leaving SUSI's trust boundary
/// (CLI output, peer/agent logs, provider errors). Masks the value of every
/// environment variable named `*_API_KEY` / `*_TOKEN` / `*_SECRET` (8+
/// chars), then `resolved=` material and credential-named fields, then
/// every configured `governance.secret_tokens` pattern. An unreadable host
/// config falls back to the bundled token list — redaction never fails open.
#[must_use]
pub fn redact_credentials(text: &str) -> String {
    let result = crate::susi_error::redact::mask_env_credentials(text);
    let result = crate::secret_ref::redact_for_export(&result);
    let patterns = SusiConfig::load_arc(&susi_paths::SusiDirs::config_dir())
        .map(|cfg| cfg.governance().secret_tokens)
        .unwrap_or_else(|_| SusiConfig::default().governance().secret_tokens);
    crate::susi_error::redact::redact_patterns(&patterns, &result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json_util::DynamicValue;
    #[test]
    fn bundled_defaults_and_heal_in_place_strip_contract_ports_and_api_auth_token_while_preserving_user_overrides(
    ) {
        let _guard = crate::env_test_lock();
        std::env::remove_var("SUSI_PORT_OFFSET");

        let heal_defaults = SusiConfig::heal_defaults();
        for port_key in HOST_CONTRACT_PORT_KEYS {
            assert!(
                !heal_defaults.contains_key(*port_key),
                "heal_defaults must exclude host-contract port key {port_key}"
            );
        }

        let mut settings = DynamicRegistry::new();
        settings.insert(
            "api_auth_token".into(),
            DynamicValue::String("legacy-secret".into()),
        );
        settings.insert("port_offset".into(), DynamicValue::from(100));
        let mut cfg = SusiConfig { settings };

        assert!(SusiConfig::heal_in_place(&mut cfg));
        assert!(!cfg.settings.contains_key("api_auth_token"));
        assert!(cfg.settings.contains_key("trust_level"));
        assert_eq!(cfg.port_offset(), 100);
        assert_eq!(cfg.gmcp_port(), susi_paths::ports::GMCP + 100);
        assert_eq!(cfg.gmcp_http_port(), susi_paths::ports::GMCP_HTTP + 100);
        assert_eq!(cfg.gemi_port(), susi_paths::ports::GEMI + 100);
        assert_eq!(
            cfg.udp_discovery_port(),
            susi_paths::ports::UDP_DISCOVERY + 100
        );
        assert_eq!(cfg.a2a_http_port(), susi_paths::ports::A2A_HTTP + 100);
        assert_eq!(cfg.gossip_port(), susi_paths::ports::GOSSIP + 100);
        assert!(!SusiConfig::heal_in_place(&mut cfg));
    }

    #[test]
    fn redact_credentials_masks_env_keys_resolved_tokens_and_governance_secret_patterns() {
        let _guard = crate::env_test_lock();
        let prev = std::env::var_os("OPENAI_API_KEY");
        std::env::set_var("OPENAI_API_KEY", "env-super-secret-key-9999");

        let default_patterns = SusiConfig::default().governance().secret_tokens;
        let sample_pattern = default_patterns
            .first()
            .cloned()
            .unwrap_or_else(|| "sk-".to_string());
        let raw = format!(
            "status=healthy key_leak=env-super-secret-key-9999 \
             resolved=inline-secret-value pattern={sample_pattern}abcdef123456"
        );
        let redacted = redact_credentials(&raw);

        match prev {
            Some(v) => std::env::set_var("OPENAI_API_KEY", v),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }

        assert!(
            redacted.contains("status=healthy"),
            "non-secret diagnostic text should survive: {redacted}"
        );
        assert!(
            !redacted.contains("env-super-secret-key-9999"),
            "OPENAI_API_KEY env value must be masked: {redacted}"
        );
        assert!(
            !redacted.contains("inline-secret-value"),
            "resolved= token value must be masked: {redacted}"
        );
        assert!(
            !redacted.contains(&format!("{sample_pattern}abcdef123456")),
            "governance.secret_tokens pattern must be masked: {redacted}"
        );
    }

    #[test]
    fn explicit_port_offset_is_env_or_a_non_zero_config_key() {
        let _guard = crate::env_test_lock();
        let prev_offset = std::env::var_os("SUSI_PORT_OFFSET");
        let with = |value: Option<u16>| {
            let mut settings = DynamicRegistry::new();
            if let Some(v) = value {
                settings.insert("port_offset".into(), DynamicValue::from(v));
            }
            SusiConfig { settings }
        };
        std::env::remove_var("SUSI_PORT_OFFSET");
        let unpinned = [with(None), with(Some(0))].map(|c| c.explicit_port_offset());
        let config_pin = with(Some(300)).explicit_port_offset();
        let config_pin_resolved = with(Some(300)).port_offset();
        std::env::set_var("SUSI_PORT_OFFSET", "0");
        let env_zero = with(Some(300)).explicit_port_offset();
        let env_zero_resolved = with(Some(300)).port_offset();
        match prev_offset {
            Some(v) => std::env::set_var("SUSI_PORT_OFFSET", v),
            None => std::env::remove_var("SUSI_PORT_OFFSET"),
        }
        assert_eq!(unpinned, [None, None], "a healed 0 is not a pin");
        assert_eq!(config_pin, Some(300));
        assert_eq!(config_pin_resolved, 300);
        assert_eq!(
            env_zero,
            Some(0),
            "SUSI_PORT_OFFSET=0 pins the canonical block"
        );
        assert_eq!(env_zero_resolved, 0);
    }

    #[test]
    fn port_offset_env_override_model_idle_timeout_env_override_and_typed_accessors_contract() {
        let _guard = crate::env_test_lock();
        let prev_offset = std::env::var_os("SUSI_PORT_OFFSET");
        let prev_idle = std::env::var_os("SUSI_MODEL_IDLE_TIMEOUT_SECS");

        let mut settings = DynamicRegistry::new();
        settings.insert("port_offset".into(), DynamicValue::from(50));
        let cfg = SusiConfig { settings };

        std::env::set_var("SUSI_PORT_OFFSET", "250");
        assert_eq!(cfg.port_offset(), 250);
        assert_eq!(cfg.gmcp_port(), susi_paths::ports::GMCP + 250);
        assert_eq!(cfg.gmcp_http_port(), susi_paths::ports::GMCP_HTTP + 250);
        assert_eq!(cfg.gemi_port(), susi_paths::ports::GEMI + 250);
        assert_eq!(
            cfg.udp_discovery_port(),
            susi_paths::ports::UDP_DISCOVERY + 250
        );
        assert_eq!(cfg.a2a_http_port(), susi_paths::ports::A2A_HTTP + 250);
        assert_eq!(cfg.gossip_port(), susi_paths::ports::GOSSIP + 250);

        std::env::set_var("SUSI_PORT_OFFSET", "not-a-u16");
        assert_eq!(cfg.port_offset(), 50);

        std::env::set_var("SUSI_MODEL_IDLE_TIMEOUT_SECS", "777");
        assert_eq!(cfg.model_idle_timeout_secs(), 777);

        match prev_offset {
            Some(v) => std::env::set_var("SUSI_PORT_OFFSET", v),
            None => std::env::remove_var("SUSI_PORT_OFFSET"),
        }
        match prev_idle {
            Some(v) => std::env::set_var("SUSI_MODEL_IDLE_TIMEOUT_SECS", v),
            None => std::env::remove_var("SUSI_MODEL_IDLE_TIMEOUT_SECS"),
        }

        assert!(cfg.gemi_max_concurrent_requests() >= 1);
        assert!(!cfg.model_catalog().is_empty());
        assert!(!cfg.eos_token_ids().is_empty());
    }

    #[test]
    fn default_config_and_heal_defaults_are_non_empty_and_contain_default_model() {
        let def = SusiConfig::default();
        assert!(!def.settings.is_empty());
        assert!(!def.default_model().is_empty());
        assert!(SusiConfig::heal_defaults().contains_key("default_model"));
    }
}
