//! Read-only capability probes (VC-201-088 / T-CLAUDE-336).
//!
//! A probe answers "what does this endpoint *really* support" with a small,
//! budgeted set of read-only requests: a model list, a capability flag, an
//! error shape. Probes **never send user data** — every request in a plan is
//! static and marked so. Results become evidence rows feeding the
//! compatibility matrix and the conflict/confidence reports.
//!
//! This module builds plans and classifies recorded transcripts; the actual
//! HTTP is performed elsewhere under the daemon's scheduler (T-CLAUDE-340).

use crate::eco_profile::Profile;
use crate::eco_schema::{Confidence, Provenance};
use serde::{Deserialize, Serialize};

/// One static, read-only request a probe may make. `sends_user_data` is
/// always `false` — a probe that would carry user input is not a probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequest {
    pub method: String,
    pub path: String,
    /// Why this request exists (`"models-list"`, `"capability-flag"`,
    /// `"error-shape"`).
    pub purpose: String,
    /// Probes are read-only: this is `false` by construction.
    pub sends_user_data: bool,
}

/// A bounded set of probe requests for one profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbePlan {
    pub profile_id: String,
    pub requests: Vec<ProbeRequest>,
    /// Hard cap on requests; plans never exceed it.
    pub budget: usize,
}

/// Build a probe plan: read-only GETs for list/discovery endpoints, plus the
/// first POST surfaced for error-shape probing — capped by `budget`.
#[must_use]
pub fn plan(profile: &Profile, budget: usize) -> ProbePlan {
    let mut requests = Vec::new();
    for ep in &profile.endpoints {
        if requests.len() >= budget {
            break;
        }
        let read_only = ep.method == "GET" || ep.method == "HEAD";
        let purpose = if ep.path.ends_with("/models") || ep.path.contains("models") {
            "models-list"
        } else if read_only {
            "capability-flag"
        } else {
            continue; // POSTs are only probed for error shapes, below
        };
        requests.push(ProbeRequest {
            method: ep.method.clone(),
            path: ep.path.clone(),
            purpose: purpose.into(),
            sends_user_data: false,
        });
    }
    if let Some(ep) = profile.endpoints.iter().find(|e| e.method == "POST") {
        if requests.len() < budget {
            requests.push(ProbeRequest {
                method: "POST".into(),
                path: ep.path.clone(),
                purpose: "error-shape".into(),
                sends_user_data: false,
            });
        }
    }
    ProbePlan {
        profile_id: profile.id.clone(),
        requests,
        budget,
    }
}

/// What a probe saw at one request, as recorded (status, headers, body).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub status: u16,
    /// Response headers of interest (lower-cased names).
    pub headers: Vec<(String, String)>,
    /// Parsed JSON body when the response carried one.
    pub body: Option<serde_json::Value>,
}

/// One evidence finding derived from a transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// `reachable`, `model-list`, `capability-flag`, `error-shape`.
    pub kind: String,
    pub detail: String,
    pub confidence: Confidence,
}

/// Classify a transcript for one probe request.
#[must_use]
pub fn classify(request: &ProbeRequest, t: &Transcript) -> Vec<Finding> {
    let mut out = Vec::new();
    match t.status {
        200..=299 => out.push(Finding {
            kind: "reachable".into(),
            detail: format!("{} {} -> {}", request.method, request.path, t.status),
            confidence: Confidence::Verified,
        }),
        401 | 403 => out.push(Finding {
            kind: "reachable".into(),
            detail: "endpoint exists but auth denied the probe".into(),
            confidence: Confidence::Inferred,
        }),
        _ => {}
    }
    if request.purpose == "models-list" {
        if let Some(data) = t.body.as_ref().and_then(|b| b.get("data")) {
            let n = data.as_array().map_or(0, Vec::len);
            out.push(Finding {
                kind: "model-list".into(),
                detail: format!("{n} model entries returned"),
                confidence: Confidence::Verified,
            });
        }
    }
    if let Some(err) = t.body.as_ref().and_then(|b| b.get("error")) {
        let keys: Vec<String> = err
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        out.push(Finding {
            kind: "error-shape".into(),
            detail: format!("error keys: {}", keys.join(",")),
            confidence: Confidence::Verified,
        });
    }
    out
}

/// Attach provenance to findings gathered by a specific probe run.
#[must_use]
pub fn evidence_for(plan: &ProbePlan, findings: &[Finding], src: &Provenance) -> serde_json::Value {
    serde_json::json!({
        "profile": plan.profile_id,
        "provenance": src,
        "findings": findings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_profile;

    #[test]
    fn eco_live_probe_plans_are_read_only_and_budgeted() {
        let p = eco_profile::load_bundled("openai-chat-completions").unwrap();
        let plan = plan(&p, 3);
        assert!(plan.requests.len() <= 3);
        for r in &plan.requests {
            assert!(!r.sends_user_data, "probes never send user data");
            assert!(["GET", "HEAD", "POST"].contains(&r.method.as_str()));
            if r.method == "POST" {
                assert_eq!(r.purpose, "error-shape");
            }
        }
    }

    #[test]
    fn eco_live_probe_plan_prefers_models_list() {
        let p = eco_profile::load_bundled("openai-chat-completions").unwrap();
        let plan = plan(&p, 10);
        assert!(plan
            .requests
            .iter()
            .any(|r| r.purpose == "models-list" && r.path.contains("models")));
    }

    #[test]
    fn eco_live_probe_classifies_models_transcript() {
        let req = ProbeRequest {
            method: "GET".into(),
            path: "/v1/models".into(),
            purpose: "models-list".into(),
            sends_user_data: false,
        };
        let t = Transcript {
            status: 200,
            headers: vec![],
            body: Some(serde_json::json!({"data": [{"id": "a"}, {"id": "b"}]})),
        };
        let findings = classify(&req, &t);
        assert!(findings.iter().any(|f| f.kind == "reachable"));
        let ml = findings.iter().find(|f| f.kind == "model-list").unwrap();
        assert!(ml.detail.contains("2 model entries"));
    }

    #[test]
    fn eco_live_probe_classifies_error_shape() {
        let req = ProbeRequest {
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            purpose: "error-shape".into(),
            sends_user_data: false,
        };
        let t = Transcript {
            status: 400,
            headers: vec![],
            body: Some(serde_json::json!({
                "error": {"message": "bad", "type": "invalid_request_error", "code": "x"}
            })),
        };
        let findings = classify(&req, &t);
        let es = findings.iter().find(|f| f.kind == "error-shape").unwrap();
        assert!(es.detail.contains("message"));
        assert!(es.detail.contains("type"));
    }

    #[test]
    fn eco_live_probe_evidence_carries_provenance() {
        let p = eco_profile::load_bundled("openai-chat-completions").unwrap();
        let plan = plan(&p, 2);
        let src = Provenance {
            source: "probe:2026-09-01".into(),
            spec_version: "live".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        };
        let ev = evidence_for(&plan, &[], &src);
        assert_eq!(ev["profile"], "openai-chat-completions");
        assert_eq!(ev["provenance"]["retrieved"], "2026-09-01");
    }
}
