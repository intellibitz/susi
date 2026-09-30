//! Qualify a complete RSI swarm AI operating-layer release (VC-201-100).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseDrill {
    pub env: String,
    pub configured: bool,
    pub mission_ok: bool,
    pub recovered: bool,
    pub self_patch_evaluated: bool,
    pub promoted_via_tag: bool,
    pub scorecard_delta: f64,
    pub predeclared_2x_metric: Option<f64>,
    pub baseline_2x_metric: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualificationReport {
    pub qualified: bool,
    pub claim_2x: bool,
    pub limitations: Vec<String>,
}

/// Promote only through tagged release path; 2x claim only with evidence.
#[must_use]
pub fn qualify_release(d: &ReleaseDrill) -> QualificationReport {
    let mut limitations = Vec::new();
    if !d.configured {
        limitations.push("ecosystem not configured".into());
    }
    if !d.mission_ok {
        limitations.push("swarm mission failed".into());
    }
    if !d.recovered {
        limitations.push("recovery incomplete".into());
    }
    if !d.self_patch_evaluated {
        limitations.push("self-patch not evaluated".into());
    }
    if !d.promoted_via_tag {
        limitations.push("promotion bypassed tagged release path".into());
    }
    let claim_2x = match (d.baseline_2x_metric, d.predeclared_2x_metric) {
        (Some(base), Some(now)) if base > 0.0 && now >= base * 2.0 => true,
        (Some(_), Some(_)) => {
            limitations.push("2x claim unsupported by predeclared metric".into());
            false
        }
        _ => false,
    };
    let qualified =
        d.configured && d.mission_ok && d.recovered && d.self_patch_evaluated && d.promoted_via_tag;
    QualificationReport {
        qualified,
        claim_2x,
        limitations,
    }
}
