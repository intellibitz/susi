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
    // Deprecated, no effect: core never downloads a model by itself (CEO
    // decision 2026-11-04). Kept, defaulting to `false`, so existing host
    // `config.json` files still load.
    SettingEntry {
        key: "auto_download_models",
        derivation: SettingDerivation::Constant,
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
