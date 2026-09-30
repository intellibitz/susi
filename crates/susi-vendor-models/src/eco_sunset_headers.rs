//! Deprecation/Sunset header tracking (VC-201-088 / T-CLAUDE-337).
//!
//! RFC 8594 (`Deprecation`) and the companion `Sunset` header turn real
//! responses into lifecycle facts. This module parses those headers (plus
//! `Link: rel="deprecation"|"successor-version"`) and produces lifecycle
//! proposals carrying the dates — never silent flag-flips in the KB.

use crate::eco_openapi_ingest::Proposal;
use crate::eco_schema::Provenance;
use serde::{Deserialize, Serialize};

/// A lifecycle observation parsed from one response's headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleNotice {
    /// Raw `Deprecation` value: `true`, an HTTP-date, or `@<unix>` (RFC 9745).
    pub deprecated: Option<String>,
    /// `Sunset` HTTP-date after which the endpoint may go dark.
    pub sunset: Option<String>,
    /// `Link` targets with `rel="deprecation"` or `rel="successor-version"`.
    pub links: Vec<(String, String)>,
}

impl LifecycleNotice {
    /// Any lifecycle signal at all?
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.deprecated.is_none() && self.sunset.is_none() && self.links.is_empty()
    }
}

/// Parse `Deprecation`, `Sunset` and `Link` headers from a response.
/// Header names are matched case-insensitively, as HTTP requires.
#[must_use]
pub fn parse_headers(headers: &[(String, String)]) -> LifecycleNotice {
    let mut notice = LifecycleNotice {
        deprecated: None,
        sunset: None,
        links: Vec::new(),
    };
    for (name, value) in headers {
        match name.to_ascii_lowercase().as_str() {
            "deprecation" => notice.deprecated = Some(value.clone()),
            "sunset" => notice.sunset = Some(value.clone()),
            "link" => {
                for part in value.split(',') {
                    let mut target = None;
                    let mut rel = None;
                    for seg in part.split(';') {
                        let seg = seg.trim();
                        if seg.starts_with('<') {
                            target = Some(seg.trim_matches(|c| c == '<' || c == '>').to_string());
                        } else if let Some(r) = seg.strip_prefix("rel=") {
                            rel = Some(r.trim_matches('"').to_string());
                        }
                    }
                    if let (Some(t), Some(r)) = (target, rel) {
                        if r == "deprecation" || r == "successor-version" {
                            notice.links.push((r, t));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    notice
}

/// Turn a notice into a reviewable lifecycle proposal for a KB subject.
#[must_use]
pub fn lifecycle_proposal(subject: &str, notice: &LifecycleNotice, src: &Provenance) -> Proposal {
    Proposal {
        kind: "lifecycle".into(),
        subject: subject.to_string(),
        summary: format!("lifecycle signal on {subject}"),
        detail: serde_json::to_value(notice).unwrap_or_default(),
        provenance: src.clone(),
    }
}

/// Is a `Deprecation` header value affirmative? `true`, a date, or `@unix`
/// all mean deprecated; only absence or the literal `false` does not.
#[must_use]
pub fn is_deprecated(notice: &LifecycleNotice) -> bool {
    notice.deprecated.as_deref().is_some_and(|v| v != "false")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::Confidence;

    fn headers(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn eco_sunset_headers_parse_deprecation_sunset_and_links() {
        let n = parse_headers(&headers(&[
            ("Deprecation", "@1735689600"),
            ("Sunset", "Wed, 01 Jan 2026 00:00:00 GMT"),
            (
                "Link",
                "<https://api.example.com/v2>; rel=\"successor-version\", <https://api.example.com/deprecation-notice>; rel=\"deprecation\"",
            ),
        ]));
        assert_eq!(n.deprecated.as_deref(), Some("@1735689600"));
        assert_eq!(n.sunset.as_deref(), Some("Wed, 01 Jan 2026 00:00:00 GMT"));
        assert!(n.links.contains(&(
            "successor-version".into(),
            "https://api.example.com/v2".into()
        )));
        assert!(n.links.contains(&(
            "deprecation".into(),
            "https://api.example.com/deprecation-notice".into()
        )));
    }

    #[test]
    fn eco_sunset_headers_case_insensitive_and_sparse() {
        let n = parse_headers(&headers(&[("dEpReCaTiOn", "true")]));
        assert!(is_deprecated(&n));
        assert!(n.sunset.is_none());
        let none = parse_headers(&headers(&[("content-type", "application/json")]));
        assert!(none.is_empty());
        assert!(!is_deprecated(&none));
    }

    #[test]
    fn eco_sunset_headers_false_means_not_deprecated() {
        let n = parse_headers(&headers(&[("Deprecation", "false")]));
        assert!(!is_deprecated(&n));
    }

    #[test]
    fn eco_sunset_headers_notices_become_proposals_with_dates() {
        let n = parse_headers(&headers(&[
            ("Deprecation", "Wed, 11 Nov 2026 23:59:59 GMT"),
            ("Sunset", "Wed, 01 Jan 2027 00:00:00 GMT"),
        ]));
        let src = Provenance {
            source: "probe:/v1/chat/completions".into(),
            spec_version: "2024-06".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Reported,
        };
        let p = lifecycle_proposal("openai-chat-completions", &n, &src);
        assert_eq!(p.kind, "lifecycle");
        assert!(p.detail["sunset"].as_str().unwrap().contains("2027"));
        assert!(p.detail["deprecated"].as_str().unwrap().contains("2026"));
        assert!(p.provenance.source.contains("probe:"));
    }
}
