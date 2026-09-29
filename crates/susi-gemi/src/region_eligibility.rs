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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudTarget {
    pub provider: String,
    pub model: String,
    pub region: Option<RegionMeta>,
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
