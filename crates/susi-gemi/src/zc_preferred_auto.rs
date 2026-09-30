//! Preferred cloud chosen by recorded evidence, not merely key presence.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEvidence {
    pub name: String,
    pub has_key: bool,
    pub success_rate: f64,
    pub p50_ms: f64,
}

#[must_use]
pub fn prefer_by_evidence(providers: &[ProviderEvidence]) -> Option<String> {
    providers
        .iter()
        .filter(|p| p.has_key && p.success_rate > 0.0)
        .max_by(|a, b| {
            a.success_rate
                .partial_cmp(&b.success_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    b.p50_ms
                        .partial_cmp(&a.p50_ms)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
        })
        .map(|p| p.name.clone())
}

#[cfg(test)]
mod zc_preferred_auto_tests {
    use super::*;

    #[test]
    fn zc_preferred_auto_picks_evidence_not_just_keys() {
        let pick = prefer_by_evidence(&[
            ProviderEvidence {
                name: "a".into(),
                has_key: true,
                success_rate: 0.5,
                p50_ms: 100.0,
            },
            ProviderEvidence {
                name: "b".into(),
                has_key: true,
                success_rate: 0.9,
                p50_ms: 200.0,
            },
            ProviderEvidence {
                name: "c".into(),
                has_key: false,
                success_rate: 1.0,
                p50_ms: 10.0,
            },
        ]);
        assert_eq!(pick.as_deref(), Some("b"));
    }
}
