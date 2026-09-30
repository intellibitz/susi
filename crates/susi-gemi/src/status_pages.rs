//! Vendor status-page integration.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VendorHealth {
    Operational,
    Degraded,
    Outage,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusSnapshot {
    pub vendor: String,
    pub health: VendorHealth,
    pub url: String,
}

/// Map a status-page indicator string to a health enum.
#[must_use]
pub fn parse_status(vendor: &str, indicator: &str) -> StatusSnapshot {
    let lower = indicator.to_ascii_lowercase();
    let health = if lower.contains("none") || lower.contains("operational") {
        VendorHealth::Operational
    } else if lower.contains("minor") || lower.contains("degraded") {
        VendorHealth::Degraded
    } else if lower.contains("major") || lower.contains("outage") || lower.contains("critical") {
        VendorHealth::Outage
    } else {
        VendorHealth::Unknown
    };
    StatusSnapshot {
        vendor: vendor.into(),
        health,
        url: format!("https://status.{vendor}.com"),
    }
}

#[cfg(test)]
mod status_pages_tests {
    use super::*;

    #[test]
    fn status_pages_map_indicators() {
        assert_eq!(
            parse_status("openai", "none").health,
            VendorHealth::Operational
        );
        assert_eq!(
            parse_status("anthropic", "minor").health,
            VendorHealth::Degraded
        );
        assert_eq!(
            parse_status("google", "major_outage").health,
            VendorHealth::Outage
        );
    }
}
