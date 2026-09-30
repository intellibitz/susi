//! Key health at boot: report what needs attention.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyHealth {
    Ok,
    Missing,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyStatus {
    pub vendor: String,
    pub health: KeyHealth,
}

#[must_use]
pub fn boot_key_report(statuses: &[KeyStatus]) -> Vec<String> {
    statuses
        .iter()
        .filter(|s| !matches!(s.health, KeyHealth::Ok))
        .map(|s| format!("{} needs attention: {:?}", s.vendor, s.health))
        .collect()
}

#[cfg(test)]
mod zc_keys_health_boot_tests {
    use super::*;

    #[test]
    fn zc_keys_health_boot_lists_problems() {
        let r = boot_key_report(&[
            KeyStatus {
                vendor: "openai".into(),
                health: KeyHealth::Ok,
            },
            KeyStatus {
                vendor: "anthropic".into(),
                health: KeyHealth::Missing,
            },
        ]);
        assert_eq!(r.len(), 1);
        assert!(r[0].contains("anthropic"));
    }
}
