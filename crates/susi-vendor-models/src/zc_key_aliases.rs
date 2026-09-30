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
}
