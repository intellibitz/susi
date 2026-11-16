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

macro_rules! setting_registry {
    ($($key:literal => $derivation:ident),+ $(,)?) => {
        /// Complete registry for every top-level key in `config.default.json`.
        pub const SETTING_REGISTRY: &[SettingEntry] = &[
            $(
                SettingEntry {
                    key: $key,
                    derivation: SettingDerivation::$derivation,
                },
            )+
        ];
    };
}

setting_registry! {
    "a2a_http_port" => Constant,
    "admin_command_routing" => Constant,
    "agent_rank_threshold" => Constant,
    "agent_routing" => Constant,
    "allow_origin" => Constant,
    "alpha_weights_filename" => Constant,
    "alpha_weights_url" => Constant,
    // Deprecated, no effect: core never downloads a model by itself (CEO
    // decision 2026-11-04). Kept, defaulting to `false`, so existing host
    // `config.json` files still load.
    "auto_download_models" => Constant,
    "axiomatic_risk_patterns" => Constant,
    "bind_address" => Constant,
    "bootstrap_mcp_servers" => Detection,
    "capability_rediscovery_secs" => Measurement,
    "cloud_scout_timeout_secs" => Measurement,
    "crates_io_api_url" => Constant,
    "default_engine" => Constant,
    "default_fallback_model" => Constant,
    "default_model" => Constant,
    "discoverable_assets" => Detection,
    "eos_token_ids" => Constant,
    "execution_lease_secs" => Measurement,
    "external_peer_agents" => Constant,
    "external_peer_agents_note" => Constant,
    "gemi_max_concurrent_requests" => Hardware,
    "gemi_port" => Constant,
    "gmcp_http_port" => Constant,
    "gmcp_port" => Constant,
    "governance" => Constant,
    "hf_base_url" => Constant,
    "home_scan_root_exclude_dirs" => Detection,
    "inference_endpoints" => Constant,
    "inference_routing" => Constant,
    "intent_classify" => Constant,
    "kv_cache_capacity_tokens" => Hardware,
    "local_scan_paths" => Detection,
    "max_concurrent_agents" => Hardware,
    "max_generation_tokens" => Hardware,
    "max_rpc_body_bytes" => Hardware,
    "max_stdin_size_bytes" => Hardware,
    "memory_experience_heuristics" => Constant,
    "model_discovery_exclude_dirs" => Detection,
    "model_file_extensions" => Detection,
    "model_file_min_bytes" => Detection,
    "model_idle_timeout_secs" => Measurement,
    "model_ladder" => Constant,
    "model_lifecycle" => Constant,
    "model_provisioning_wait_secs" => Measurement,
    "model_scan_exclude_dirs" => Detection,
    "model_scoring_heuristics" => Constant,
    "openrouter_app_title" => Constant,
    "openrouter_http_referer" => Constant,
    "port_offset" => Constant,
    "privacy" => Consent,
    "qdrant_url" => Constant,
    "rate_limit_per_minute" => Measurement,
    "reflex_training_threshold" => Constant,
    "repeat_last_n" => Constant,
    "repeat_penalty" => Constant,
    "sandbox_image" => Constant,
    "speculative_decoding_enabled" => Constant,
    "speculative_draft_tokens" => Constant,
    "tls_cert_path" => Constant,
    "tls_key_path" => Constant,
    "tokenizer_filename" => Constant,
    "trust_level" => Constant,
    "udp_discovery_port" => Constant,
}

#[must_use]
pub fn registry_keys() -> Vec<&'static str> {
    SETTING_REGISTRY.iter().map(|e| e.key).collect()
}

#[must_use]
pub fn lookup(key: &str) -> Option<&'static SettingEntry> {
    SETTING_REGISTRY.iter().find(|e| e.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_registry_keys_are_sorted_unique_and_non_empty() {
        assert!(!SETTING_REGISTRY.is_empty());
        let keys = registry_keys();
        assert_eq!(keys.len(), SETTING_REGISTRY.len());
        for key in &keys {
            assert!(!key.trim().is_empty(), "registry key must not be empty");
        }
        for pair in keys.windows(2) {
            assert!(
                pair[0] < pair[1],
                "registry keys must be strictly sorted and unique: {:?} vs {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn lookup_resolves_all_derivation_categories_and_returns_none_for_unknown() {
        assert_eq!(
            lookup("gemi_port").map(|e| e.derivation),
            Some(SettingDerivation::Constant)
        );
        assert_eq!(
            lookup("max_concurrent_agents").map(|e| e.derivation),
            Some(SettingDerivation::Hardware)
        );
        assert_eq!(
            lookup("local_scan_paths").map(|e| e.derivation),
            Some(SettingDerivation::Detection)
        );
        assert_eq!(
            lookup("execution_lease_secs").map(|e| e.derivation),
            Some(SettingDerivation::Measurement)
        );
        assert_eq!(
            lookup("privacy").map(|e| e.derivation),
            Some(SettingDerivation::Consent)
        );
        assert_eq!(lookup("nonexistent_key"), None);
    }

    #[test]
    fn every_registry_entry_roundtrips_lookup_and_five_derivations_partition_the_registry() {
        for entry in SETTING_REGISTRY {
            let found = lookup(entry.key).expect("every SETTING_REGISTRY key must resolve");
            assert_eq!(found, entry);
        }

        let variants = [
            (SettingDerivation::Constant, "\"constant\""),
            (SettingDerivation::Hardware, "\"hardware\""),
            (SettingDerivation::Detection, "\"detection\""),
            (SettingDerivation::Measurement, "\"measurement\""),
            (SettingDerivation::Consent, "\"consent\""),
        ];
        let mut total_partitioned = 0usize;
        for (variant, expected_json) in variants {
            let count = SETTING_REGISTRY
                .iter()
                .filter(|e| e.derivation == variant)
                .count();
            assert!(
                count > 0,
                "derivation {variant:?} must be represented in SETTING_REGISTRY"
            );
            total_partitioned += count;

            let serialized = serde_json::to_string(&variant).expect("serialize derivation");
            assert_eq!(serialized, expected_json);
            let deserialized: SettingDerivation =
                serde_json::from_str(&serialized).expect("deserialize derivation");
            assert_eq!(deserialized, variant);
        }
        assert_eq!(total_partitioned, SETTING_REGISTRY.len());
    }

    #[test]
    fn every_bundled_default_config_key_is_registered_in_setting_registry() {
        for key in crate::SusiConfig::default().settings.keys() {
            assert!(
                lookup(key).is_some(),
                "bundled config.default.json key {key:?} is missing from SETTING_REGISTRY"
            );
        }
        for key in crate::SusiConfig::heal_defaults().keys() {
            assert!(
                lookup(key).is_some(),
                "heal_defaults key {key:?} is missing from SETTING_REGISTRY"
            );
        }

        let sample = SettingEntry {
            key: "gemi_port",
            derivation: SettingDerivation::Constant,
        };
        let encoded = serde_json::to_string(&sample).expect("serialize SettingEntry");
        assert_eq!(encoded, r#"{"key":"gemi_port","derivation":"constant"}"#);
        let decoded: SettingEntry =
            serde_json::from_str(r#"{"key":"gemi_port","derivation":"constant"}"#)
                .expect("deserialize SettingEntry");
        assert_eq!(decoded, sample);
    }

    #[test]
    fn setting_derivation_copy_clone_and_debug_hold_across_all_variants() {
        let all = [
            SettingDerivation::Constant,
            SettingDerivation::Hardware,
            SettingDerivation::Detection,
            SettingDerivation::Measurement,
            SettingDerivation::Consent,
        ];
        for v in all {
            let copied = v;
            assert_eq!(copied, v);
            assert!(!format!("{v:?}").is_empty());
        }
    }
}
