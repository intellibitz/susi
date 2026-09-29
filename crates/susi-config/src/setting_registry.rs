//! Setting registry: every bundled config key declares how its default is derived (VC-201-061 / zero-config).
//!
//! No key is `UserMustSet` — secrets/consent are requested just-in-time, never as a required file edit.

use serde::{Deserialize, Serialize};

/// How the bundled default for a setting is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingDerivation {
    /// Fixed bundled constant.
    Constant,
    /// Derived from host hardware (CPU/RAM/VRAM).
    Hardware,
    /// Derived by probing installed tools/paths/models.
    Detection,
    /// Derived from measured latency/throughput/usage.
    Measurement,
    /// Requires operator consent (never a required hand-edit).
    Consent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingEntry {
    pub key: &'static str,
    pub derivation: SettingDerivation,
}

/// Complete registry for every top-level key in `config.default.json`.
pub const SETTING_REGISTRY: &[SettingEntry] = &[
    SettingEntry {
        key: "a2a_http_port",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "admin_command_routing",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "admin_pulses",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "agent_rank_threshold",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "agent_routing",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "allow_origin",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "alpha_weights_filename",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "alpha_weights_url",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "auto_download_models",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "axiomatic_risk_patterns",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "bind_address",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "bootstrap_mcp_servers",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "capability_rediscovery_secs",
        derivation: SettingDerivation::Measurement,
    },
    SettingEntry {
        key: "cloud_scout_timeout_secs",
        derivation: SettingDerivation::Measurement,
    },
    SettingEntry {
        key: "crates_io_api_url",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "default_engine",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "default_fallback_model",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "default_model",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "discoverable_assets",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "eos_token_ids",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "execution_lease_secs",
        derivation: SettingDerivation::Measurement,
    },
    SettingEntry {
        key: "external_peer_agents",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "external_peer_agents_note",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "gemi_max_concurrent_requests",
        derivation: SettingDerivation::Hardware,
    },
    SettingEntry {
        key: "gemi_port",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "gmcp_http_port",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "gmcp_port",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "governance",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "hf_base_url",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "home_scan_root_exclude_dirs",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "https_only",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "inference_endpoints",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "inference_routing",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "intent_classify",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "kv_cache_capacity_tokens",
        derivation: SettingDerivation::Hardware,
    },
    SettingEntry {
        key: "local_scan_paths",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "max_concurrent_agents",
        derivation: SettingDerivation::Hardware,
    },
    SettingEntry {
        key: "max_generation_tokens",
        derivation: SettingDerivation::Hardware,
    },
    SettingEntry {
        key: "max_rpc_body_bytes",
        derivation: SettingDerivation::Hardware,
    },
    SettingEntry {
        key: "max_stdin_size_bytes",
        derivation: SettingDerivation::Hardware,
    },
    SettingEntry {
        key: "mcp_registry_url",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "memory_experience_heuristics",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "model_discovery_exclude_dirs",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "model_file_extensions",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "model_file_min_bytes",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "model_idle_timeout_secs",
        derivation: SettingDerivation::Measurement,
    },
    SettingEntry {
        key: "model_ladder",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "model_lifecycle",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "model_provisioning_wait_secs",
        derivation: SettingDerivation::Measurement,
    },
    SettingEntry {
        key: "model_scan_exclude_dirs",
        derivation: SettingDerivation::Detection,
    },
    SettingEntry {
        key: "model_scoring_heuristics",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "openrouter_app_title",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "openrouter_http_referer",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "port_offset",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "privacy",
        derivation: SettingDerivation::Consent,
    },
    SettingEntry {
        key: "qdrant_url",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "rate_limit_per_minute",
        derivation: SettingDerivation::Measurement,
    },
    SettingEntry {
        key: "reflex_training_threshold",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "repeat_last_n",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "repeat_penalty",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "sandbox_image",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "speculative_decoding_enabled",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "speculative_draft_tokens",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "tls_cert_path",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "tls_key_path",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "tokenizer_filename",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "trust_level",
        derivation: SettingDerivation::Constant,
    },
    SettingEntry {
        key: "udp_discovery_port",
        derivation: SettingDerivation::Constant,
    },
];

#[must_use]
pub fn registry_keys() -> Vec<&'static str> {
    SETTING_REGISTRY.iter().map(|e| e.key).collect()
}

#[must_use]
pub fn lookup(key: &str) -> Option<&'static SettingEntry> {
    SETTING_REGISTRY.iter().find(|e| e.key == key)
}
