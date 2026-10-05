//! Qualify a complete RSI swarm AI operating-layer release (VC-201-100).

use serde::{Deserialize, Serialize};
use std::path::Path;

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
    /// Scorecard deltas in deterministic environment order.
    pub scorecard_deltas: Vec<ScorecardDelta>,
}

/// A reproducible scorecard result for one qualification environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScorecardDelta {
    /// Stable environment name.
    pub environment: String,
    /// Measured delta against that environment's declared baseline.
    pub delta: f64,
}

/// Evidence collected from one isolated local or explicitly configured cloud environment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualificationInput {
    /// The release drill facts for the environment.
    pub drill: ReleaseDrill,
    /// Whether the run stayed within its declared budget.
    pub budget_preserved: bool,
    /// Whether the run preserved its privacy boundary.
    pub privacy_preserved: bool,
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
        scorecard_deltas: vec![ScorecardDelta {
            environment: d.env.clone(),
            delta: d.scorecard_delta,
        }],
    }
}

/// Qualify the complete multi-stage release evidence set.
///
/// A single local smoke run is deliberately insufficient: qualification needs
/// both a local and an explicitly configured cloud environment, and both must
/// report mission recovery, self-patch evaluation, privacy, budget, and tagged
/// promotion. The returned report is serializable and contains deterministic
/// scorecard deltas plus every limitation that kept the claim from qualifying.
#[must_use]
pub fn qualify_multi_stage(inputs: &[QualificationInput]) -> QualificationReport {
    let mut limitations = Vec::new();
    let mut scorecard_deltas = Vec::new();
    let mut has_local = false;
    let mut has_cloud = false;
    let mut every_stage_passed = !inputs.is_empty();
    let mut all_2x = !inputs.is_empty();

    for input in inputs {
        let environment = input.drill.env.as_str();
        has_local |= environment == "local";
        has_cloud |= environment == "cloud";
        let report = qualify_release(&input.drill);
        every_stage_passed &= report.qualified;
        all_2x &= report.claim_2x;
        limitations.extend(
            report
                .limitations
                .into_iter()
                .map(|limitation| format!("{environment}: {limitation}")),
        );
        if !input.budget_preserved {
            every_stage_passed = false;
            limitations.push(format!("{environment}: budget preservation failed"));
        }
        if !input.privacy_preserved {
            every_stage_passed = false;
            limitations.push(format!("{environment}: privacy preservation failed"));
        }
        if !input.drill.scorecard_delta.is_finite() {
            every_stage_passed = false;
            limitations.push(format!("{environment}: scorecard delta is not finite"));
        }
        scorecard_deltas.push(ScorecardDelta {
            environment: input.drill.env.clone(),
            delta: input.drill.scorecard_delta,
        });
    }

    if !has_local {
        every_stage_passed = false;
        limitations.push("local: no local environment evidence".into());
    }
    if !has_cloud {
        every_stage_passed = false;
        limitations.push("cloud: no explicitly configured cloud environment evidence".into());
    }
    scorecard_deltas.sort_by(|left, right| left.environment.cmp(&right.environment));

    QualificationReport {
        qualified: every_stage_passed,
        claim_2x: every_stage_passed && all_2x,
        limitations,
        scorecard_deltas,
    }
}

/// Load an explicitly supplied cloud evidence record for release qualification.
pub fn load_cloud_evidence(path: &Path) -> Result<QualificationInput, String> {
    let body =
        std::fs::read_to_string(path).map_err(|error| format!("read cloud evidence: {error}"))?;
    serde_json::from_str(&body).map_err(|error| format!("parse cloud evidence: {error}"))
}
