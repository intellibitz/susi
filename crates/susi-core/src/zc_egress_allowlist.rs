//! Egress allowlist derived from configured vendors.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressAllowlist {
    pub hosts: BTreeSet<String>,
}

#[must_use]
pub fn allowlist_from_vendors(vendors: &[&str]) -> EgressAllowlist {
    let mut hosts = BTreeSet::new();
    for v in vendors {
        match *v {
            "openai" => {
                hosts.insert("api.openai.com".into());
            }
            "anthropic" => {
                hosts.insert("api.anthropic.com".into());
            }
            "hf" | "huggingface" => {
                hosts.insert("huggingface.co".into());
            }
            _ => {}
        }
    }
    EgressAllowlist { hosts }
}

#[must_use]
pub fn allows(list: &EgressAllowlist, host: &str) -> bool {
    list.hosts.contains(host)
}

#[cfg(test)]
mod zc_egress_allowlist_tests {
    use super::*;

    #[test]
    fn zc_egress_allowlist_from_configured_vendors() {
        let a = allowlist_from_vendors(&["openai", "anthropic"]);
        assert!(allows(&a, "api.openai.com"));
        assert!(allows(&a, "api.anthropic.com"));
        assert!(!allows(&a, "evil.example"));
    }

    #[test]
    fn allowlist_from_vendors_maps_hf_and_huggingface_aliases_to_huggingface_co() {
        let hf_short = allowlist_from_vendors(&["hf"]);
        let hf_long = allowlist_from_vendors(&["huggingface"]);
        assert_eq!(hf_short.hosts.len(), 1);
        assert_eq!(hf_long.hosts.len(), 1);
        assert!(allows(&hf_short, "huggingface.co"));
        assert!(allows(&hf_long, "huggingface.co"));
    }

    #[test]
    fn allowlist_from_vendors_ignores_unknown_vendors_and_deduplicates() {
        let a = allowlist_from_vendors(&["unknown", "openai", "openai"]);
        assert_eq!(a.hosts.len(), 1);
        assert!(allows(&a, "api.openai.com"));
        assert!(!allows(&a, "unknown"));
    }

    #[test]
    fn egress_allowlist_default_is_empty_and_serde_roundtrips() {
        let empty = EgressAllowlist::default();
        assert!(empty.hosts.is_empty());
        assert!(!allows(&empty, "api.openai.com"));

        let populated = allowlist_from_vendors(&["openai", "hf"]);
        let json = serde_json::to_string(&populated).expect("serialize");
        let back: EgressAllowlist = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, populated);
    }

    #[test]
    fn allowlist_from_vendors_with_empty_slice_produces_empty_host_set() {
        let list = allowlist_from_vendors(&[]);
        assert_eq!(list, EgressAllowlist::default());
        assert!(!allows(&list, "api.openai.com"));
        assert!(!allows(&list, "api.anthropic.com"));
        assert!(!allows(&list, "huggingface.co"));
    }
}
