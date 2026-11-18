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

    #[test]
    fn boot_key_report_empty_input_returns_empty_vec() {
        assert!(boot_key_report(&[]).is_empty());
    }

    #[test]
    fn boot_key_report_all_ok_entries_returns_empty_vec() {
        let r = boot_key_report(&[
            KeyStatus {
                vendor: "openai".into(),
                health: KeyHealth::Ok,
            },
            KeyStatus {
                vendor: "gemini".into(),
                health: KeyHealth::Ok,
            },
        ]);
        assert!(r.is_empty());
    }

    #[test]
    fn boot_key_report_surfaces_missing_expired_and_revoked_vendors() {
        let r = boot_key_report(&[
            KeyStatus {
                vendor: "openai".into(),
                health: KeyHealth::Missing,
            },
            KeyStatus {
                vendor: "anthropic".into(),
                health: KeyHealth::Expired,
            },
            KeyStatus {
                vendor: "deepseek".into(),
                health: KeyHealth::Revoked,
            },
        ]);
        assert_eq!(r.len(), 3);
        assert!(r[0].contains("openai") && r[0].contains("Missing"));
        assert!(r[1].contains("anthropic") && r[1].contains("Expired"));
        assert!(r[2].contains("deepseek") && r[2].contains("Revoked"));
    }

    #[test]
    fn key_status_and_health_serde_snake_case_roundtrip() {
        let status = KeyStatus {
            vendor: "gemini".into(),
            health: KeyHealth::Expired,
        };
        let json = serde_json::to_string(&status).expect("serialize KeyStatus");
        assert!(json.contains("\"expired\""));
        let decoded: KeyStatus = serde_json::from_str(&json).expect("deserialize KeyStatus");
        assert_eq!(decoded, status);
    }
}
