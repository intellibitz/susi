//! Provider region and residency eligibility (VC-201-054).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegionProvenance {
    Verified,
    OperatorAttested,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegionMeta {
    pub region: String,
    pub provenance: RegionProvenance,
}

impl RegionMeta {
    /// Verify structural invariants on region residency metadata.
    pub fn verify_region_meta_invariants(&self) -> Result<(), String> {
        if self.region.trim().is_empty() {
            return Err("RegionMeta region must not be empty".to_string());
        }
        match self.provenance {
            RegionProvenance::Verified | RegionProvenance::OperatorAttested => Ok(()),
            RegionProvenance::Unknown => {
                Err("RegionMeta provenance must not be Unknown for attested residency".to_string())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudTarget {
    pub provider: String,
    pub model: String,
    pub region: Option<RegionMeta>,
}

impl CloudTarget {
    /// Verify structural invariants on a cloud target and its optional region metadata.
    pub fn verify_target_invariants(&self) -> Result<(), String> {
        if self.provider.trim().is_empty() {
            return Err("CloudTarget provider must not be empty".to_string());
        }
        if self.model.trim().is_empty() {
            return Err("CloudTarget model must not be empty".to_string());
        }
        if let Some(meta) = &self.region {
            meta.verify_region_meta_invariants()?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementDecision {
    Allow,
    RejectUnknownRegion,
    RejectDisallowed { region: String },
}

/// Reject unknown or disallowed regions before any payload is sent.
pub fn place(target: &CloudTarget, allowed: &[&str], constrain: bool) -> PlacementDecision {
    if !constrain {
        return PlacementDecision::Allow;
    }
    let Some(meta) = &target.region else {
        return PlacementDecision::RejectUnknownRegion;
    };
    if matches!(meta.provenance, RegionProvenance::Unknown) || meta.region.is_empty() {
        return PlacementDecision::RejectUnknownRegion;
    }
    if allowed.iter().any(|r| *r == meta.region) {
        PlacementDecision::Allow
    } else {
        PlacementDecision::RejectDisallowed {
            region: meta.region.clone(),
        }
    }
}

#[cfg(test)]
mod inline_tests {
    use super::*;

    #[test]
    fn unconstrained_placement_allows_target_even_without_region_metadata() {
        let target = CloudTarget {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            region: None,
        };
        assert_eq!(
            place(&target, &["us-east-1"], false),
            PlacementDecision::Allow
        );
    }

    #[test]
    fn constrained_placement_rejects_missing_or_unknown_provenance_region() {
        let missing = CloudTarget {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            region: None,
        };
        assert_eq!(
            place(&missing, &["us-east-1"], true),
            PlacementDecision::RejectUnknownRegion
        );

        let unknown_prov = CloudTarget {
            provider: "openai".to_string(),
            model: "gpt-4o".to_string(),
            region: Some(RegionMeta {
                region: "us-east-1".to_string(),
                provenance: RegionProvenance::Unknown,
            }),
        };
        assert_eq!(
            place(&unknown_prov, &["us-east-1"], true),
            PlacementDecision::RejectUnknownRegion
        );
    }

    #[test]
    fn constrained_placement_allows_verified_and_operator_attested_allowed_regions() {
        for provenance in [
            RegionProvenance::Verified,
            RegionProvenance::OperatorAttested,
        ] {
            let target = CloudTarget {
                provider: "vertex".to_string(),
                model: "gemini".to_string(),
                region: Some(RegionMeta {
                    region: "eu-west-1".to_string(),
                    provenance,
                }),
            };
            assert_eq!(
                place(&target, &["us-east-1", "eu-west-1"], true),
                PlacementDecision::Allow
            );
        }
    }

    #[test]
    fn constrained_placement_rejects_disallowed_or_empty_region_names() {
        let disallowed = CloudTarget {
            provider: "vertex".to_string(),
            model: "gemini".to_string(),
            region: Some(RegionMeta {
                region: "ap-northeast-1".to_string(),
                provenance: RegionProvenance::Verified,
            }),
        };
        assert_eq!(
            place(&disallowed, &["eu-west-1"], true),
            PlacementDecision::RejectDisallowed {
                region: "ap-northeast-1".to_string()
            }
        );

        let empty_name = CloudTarget {
            provider: "vertex".to_string(),
            model: "gemini".to_string(),
            region: Some(RegionMeta {
                region: String::new(),
                provenance: RegionProvenance::Verified,
            }),
        };
        assert_eq!(
            place(&empty_name, &["eu-west-1"], true),
            PlacementDecision::RejectUnknownRegion
        );
    }

    #[test]
    fn region_provenance_and_cloud_target_round_trip_serde_json() {
        let target = CloudTarget {
            provider: "vertex".to_string(),
            model: "gemini-2.5-pro".to_string(),
            region: Some(RegionMeta {
                region: "us-central1".to_string(),
                provenance: RegionProvenance::OperatorAttested,
            }),
        };
        let encoded = serde_json::to_string(&target).unwrap();
        assert!(encoded.contains("operator_attested"));
        let decoded: CloudTarget = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, target);
    }

    #[test]
    fn verify_cloud_target_and_region_meta_invariants() {
        let valid_target = CloudTarget {
            provider: "vertex".to_string(),
            model: "gemini-2.5-pro".to_string(),
            region: Some(RegionMeta {
                region: "us-central1".to_string(),
                provenance: RegionProvenance::Verified,
            }),
        };
        assert!(valid_target.verify_target_invariants().is_ok());

        let unpinned_target = CloudTarget {
            region: None,
            ..valid_target.clone()
        };
        assert!(unpinned_target.verify_target_invariants().is_ok());

        let unknown_provenance = CloudTarget {
            region: Some(RegionMeta {
                region: "us-central1".to_string(),
                provenance: RegionProvenance::Unknown,
            }),
            ..valid_target.clone()
        };
        assert!(unknown_provenance.verify_target_invariants().is_err());

        let blank_provider = CloudTarget {
            provider: "   ".to_string(),
            ..valid_target
        };
        assert!(blank_provider.verify_target_invariants().is_err());
    }
}
