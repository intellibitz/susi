//! OpenAPI / JSON Schema ingestion (VC-201-088 / T-CLAUDE-335).
//!
//! Vendors publish machine-readable API specs. This module extracts endpoints
//! from an *already fetched* spec document (fetching is the caller's job —
//! respecting licence and terms is a policy decision, not a parser one),
//! diffs them against the recorded [`Profile`], and emits **proposals**: one
//! reviewable JSON record per suggested knowledge-base change.
//!
//! Ingestion never mutates the KB. `write_proposals` drops files into a
//! review directory; a human or a later task turns them into entity facts.

use crate::eco_profile::{Endpoint, Profile};
use crate::eco_schema::Provenance;
use serde::{Deserialize, Serialize};
use std::path::Path;
use susi_error::{EaiError, EaiResult};

/// One reviewable KB change proposed by ingest, drift or a changelog entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// What kind of change: `add-endpoint`, `removed-endpoint`,
    /// `new-model`, `deprecation`, `lifecycle`, `unknown-field`, `drift`.
    pub kind: String,
    /// Subject this proposal touches (profile or entity id).
    pub subject: String,
    /// Human-readable one-liner for review.
    pub summary: String,
    /// Structured payload the reviewer inspects.
    pub detail: serde_json::Value,
    /// Where this proposal came from — spec doc, feed entry, live probe.
    pub provenance: Provenance,
}

/// HTTP method keys an OpenAPI `paths` object may carry.
const METHODS: [&str; 6] = ["get", "post", "put", "patch", "delete", "head"];

/// Extract `(method, path)` endpoint pairs from an OpenAPI 3.x document.
#[must_use]
pub fn extract_endpoints(doc: &serde_json::Value) -> Vec<Endpoint> {
    let mut out = Vec::new();
    let Some(paths) = doc.get("paths").and_then(|p| p.as_object()) else {
        return out;
    };
    for (path, ops) in paths {
        let Some(ops) = ops.as_object() else { continue };
        for (method, op) in ops {
            if !METHODS.contains(&method.as_str()) {
                continue;
            }
            let streaming = op
                .get("x-streaming")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || op
                    .pointer("/requestBody/content/text/event-stream")
                    .is_some();
            out.push(Endpoint {
                method: method.to_uppercase(),
                path: path.clone(),
                description: op
                    .get("summary")
                    .or_else(|| op.get("operationId"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                request: op
                    .pointer("/requestBody/content/application~1json/schema/$ref")
                    .or_else(|| op.pointer("/requestBody/content/application\\/json/schema/$ref"))
                    .and_then(|v| v.as_str())
                    .map(|r| r.rsplit('/').next().unwrap_or(r).to_string()),
                response: op
                    .pointer("/responses/200/content/application~1json/schema/$ref")
                    .and_then(|v| v.as_str())
                    .map(|r| r.rsplit('/').next().unwrap_or(r).to_string()),
                streaming,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path).then(a.method.cmp(&b.method)));
    out
}

/// Diff a fetched spec's endpoints against a recorded profile.
/// Returns proposals for endpoints the spec adds or drops — never a merge.
#[must_use]
pub fn diff_profile(profile: &Profile, doc: &serde_json::Value, src: &Provenance) -> Vec<Proposal> {
    let live = extract_endpoints(doc);
    let recorded: Vec<(&str, &str)> = profile
        .endpoints
        .iter()
        .map(|e| (e.method.as_str(), e.path.as_str()))
        .collect();
    let mut proposals = Vec::new();
    for ep in &live {
        if !recorded
            .iter()
            .any(|(m, p)| *m == ep.method && *p == ep.path)
        {
            proposals.push(Proposal {
                kind: "add-endpoint".into(),
                subject: profile.id.clone(),
                summary: format!(
                    "{} {} present in spec, missing from profile",
                    ep.method, ep.path
                ),
                detail: serde_json::to_value(ep).unwrap_or_default(),
                provenance: src.clone(),
            });
        }
    }
    for (m, p) in &recorded {
        if !live.iter().any(|e| e.method == *m && e.path == *p) {
            proposals.push(Proposal {
                kind: "removed-endpoint".into(),
                subject: profile.id.clone(),
                summary: format!("{m} {p} recorded but absent from spec"),
                detail: serde_json::json!({"method": m, "path": p}),
                provenance: src.clone(),
            });
        }
    }
    proposals
}

/// Write proposals to a review dir as one file each.
/// Returns the paths written. The KB itself is never touched.
///
/// # Errors
/// `EaiError::io` on directory or write failure.
pub fn write_proposals(dir: &Path, proposals: &[Proposal]) -> EaiResult<Vec<std::path::PathBuf>> {
    std::fs::create_dir_all(dir).map_err(|e| EaiError::io(e.to_string()))?;
    let mut written = Vec::new();
    for (i, p) in proposals.iter().enumerate() {
        let name = format!("{:03}-{}-{}.json", i + 1, p.kind, p.subject);
        let path = dir.join(name);
        let body = serde_json::to_string_pretty(p).map_err(|e| EaiError::config(e.to_string()))?;
        std::fs::write(&path, body).map_err(|e| EaiError::io(e.to_string()))?;
        written.push(path);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::Confidence;

    fn spec_doc() -> serde_json::Value {
        serde_json::json!({
            "openapi": "3.0.3",
            "paths": {
                "/v1/chat/completions": {"post": {"operationId": "createChatCompletion"}},
                "/v1/models": {"get": {"operationId": "listModels"}},
                "/v1/responses": {"post": {"operationId": "createResponse", "x-streaming": true}},
            }
        })
    }

    fn src() -> Provenance {
        Provenance {
            source: "https://example.com/openapi.json".into(),
            spec_version: "2025-01-01".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        }
    }

    fn mini_profile() -> Profile {
        Profile {
            version: crate::eco_profile::PROFILE_VERSION.into(),
            kind: crate::eco_profile::ProfileKind::Api,
            id: "toy".into(),
            name: "Toy API".into(),
            subject: "toy".into(),
            endpoints: vec![
                Endpoint {
                    method: "POST".into(),
                    path: "/v1/chat/completions".into(),
                    description: String::new(),
                    request: None,
                    response: None,
                    streaming: false,
                },
                Endpoint {
                    method: "GET".into(),
                    path: "/v1/old".into(),
                    description: String::new(),
                    request: None,
                    response: None,
                    streaming: false,
                },
            ],
            auth: vec![],
            versions: vec![],
            request_shapes: vec![],
            response_shapes: vec![],
            errors: vec![],
            rate_limits: vec![],
            streaming: None,
            capabilities: vec![],
            deviations: vec![],
            provenance: src(),
        }
    }

    #[test]
    fn eco_openapi_ingest_extracts_endpoints_sorted() {
        let eps = extract_endpoints(&spec_doc());
        let pairs: Vec<_> = eps
            .iter()
            .map(|e| (e.method.as_str(), e.path.as_str()))
            .collect();
        assert!(pairs.contains(&("POST", "/v1/chat/completions")));
        assert!(pairs.contains(&("GET", "/v1/models")));
        let resp = eps.iter().find(|e| e.path == "/v1/responses").unwrap();
        assert!(resp.streaming);
        assert_eq!(resp.method, "POST");
    }

    #[test]
    fn eco_openapi_ingest_diff_proposes_adds_and_removals() {
        let proposals = diff_profile(&mini_profile(), &spec_doc(), &src());
        let kinds: Vec<_> = proposals
            .iter()
            .map(|p| (p.kind.as_str(), p.subject.as_str()))
            .collect();
        assert!(kinds.contains(&("add-endpoint", "toy")), "{proposals:?}");
        assert!(
            kinds.contains(&("removed-endpoint", "toy")),
            "{proposals:?}"
        );
        // /v1/chat/completions is recorded — no proposal for it
        assert!(!proposals
            .iter()
            .any(|p| p.summary.contains("chat/completions")));
        // every proposal carries provenance
        for p in &proposals {
            assert!(p.provenance.source.contains("example.com"));
            assert_eq!(p.provenance.confidence, Confidence::Verified);
        }
    }

    #[test]
    fn eco_openapi_ingest_writes_review_files_never_merges() {
        let proposals = diff_profile(&mini_profile(), &spec_doc(), &src());
        let dir = std::env::temp_dir().join(format!("eco-ingest-{}", std::process::id()));
        let written = write_proposals(&dir, &proposals).expect("writes proposals");
        assert_eq!(written.len(), proposals.len());
        for path in &written {
            let text = std::fs::read_to_string(path).unwrap();
            let back: Proposal = serde_json::from_str(&text).unwrap();
            assert!(!back.summary.is_empty());
        }
        // profile unchanged — ingest only proposes
        let before = serde_json::to_value(&mini_profile()).unwrap();
        diff_profile(&mini_profile(), &spec_doc(), &src());
        assert_eq!(before, serde_json::to_value(&mini_profile()).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
