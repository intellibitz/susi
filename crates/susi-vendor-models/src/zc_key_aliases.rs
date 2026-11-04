//! One canonical key per vendor; accept aliases.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyAliasMap {
    /// alias env name -> canonical vendor
    pub aliases: BTreeMap<String, String>,
}

impl Default for KeyAliasMap {
    fn default() -> Self {
        let mut aliases = BTreeMap::new();
        aliases.insert("OPENAI_API_KEY".into(), "openai".into());
        aliases.insert("OPENAI_KEY".into(), "openai".into());
        aliases.insert("ANTHROPIC_API_KEY".into(), "anthropic".into());
        aliases.insert("CLAUDE_API_KEY".into(), "anthropic".into());
        Self { aliases }
    }
}

#[must_use]
pub fn canonical_vendor(map: &KeyAliasMap, env_name: &str) -> Option<String> {
    map.aliases.get(env_name).cloned()
}

#[cfg(test)]
mod zc_key_aliases_tests {
    use super::*;

    #[test]
    fn zc_key_aliases_map_to_one_vendor() {
        let m = KeyAliasMap::default();
        assert_eq!(
            canonical_vendor(&m, "OPENAI_KEY").as_deref(),
            Some("openai")
        );
        assert_eq!(
            canonical_vendor(&m, "CLAUDE_API_KEY").as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn canonical_vendor_maps_primary_env_names_and_returns_none_for_unknown() {
        let m = KeyAliasMap::default();
        assert_eq!(
            canonical_vendor(&m, "OPENAI_API_KEY").as_deref(),
            Some("openai")
        );
        assert_eq!(
            canonical_vendor(&m, "ANTHROPIC_API_KEY").as_deref(),
            Some("anthropic")
        );
        assert!(canonical_vendor(&m, "UNKNOWN_KEY").is_none());
    }

    #[test]
    fn key_alias_map_supports_custom_aliases_and_case_sensitive_lookup() {
        let mut m = KeyAliasMap::default();
        m.aliases.insert("GEMINI_KEY".into(), "google".into());
        assert_eq!(
            canonical_vendor(&m, "GEMINI_KEY").as_deref(),
            Some("google")
        );
        assert!(canonical_vendor(&m, "openai_key").is_none());
    }

    #[test]
    fn key_alias_map_serde_json_roundtrip_preserves_all_entries() {
        let m = KeyAliasMap::default();
        assert_eq!(m.aliases.len(), 4);
        let json = serde_json::to_string(&m).expect("KeyAliasMap must serialize");
        let roundtripped: KeyAliasMap =
            serde_json::from_str(&json).expect("KeyAliasMap must deserialize");
        assert_eq!(roundtripped, m);
    }
}
