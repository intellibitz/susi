//! Egress allowlist derived from configured vendors.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}
