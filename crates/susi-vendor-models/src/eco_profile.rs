//! Machine-readable ecosystem profiles.
//!
//! A profile is the detailed record behind a knowledge-base entity: the
//! endpoints, auth schemes, versions, request/response shapes, error shapes,
//! rate-limit signals, streaming semantics and capability tags of one API or
//! protocol. Profiles live as JSON in `config/ecosystem/profiles/` (bundled)
//! and `<config>/ecosystem/profiles/` (user layer); [`validate`] is the
//! offline contract every profile task's tests run.
use crate::eco_schema::{valid_id, Issue, Provenance, Stage};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult, ResultExt as Context};

/// Profile document version tag.
pub const PROFILE_VERSION: &str = "eco-profile/v1";

/// How a caller authenticates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthScheme {
    /// `Authorization: Bearer <key>`.
    Bearer,
    /// A named header carries the key verbatim (`x-api-key`, `api-key`, …).
    ApiKeyHeader,
    /// An ephemeral token minted by another call (Realtime sessions).
    SessionToken,
    /// A signature scheme over the request (AWS SigV4, …).
    RequestSignature,
    /// No auth — documented, not omitted.
    None,
}

/// One HTTP (or transport) endpoint the profile covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    /// `POST`, `GET`, `GET+SSE`, `WSS`, …
    pub method: String,
    /// Canonical path, e.g. `/v1/chat/completions`.
    pub path: String,
    #[serde(default)]
    pub description: String,
    /// Name of the request shape (matches `request_shapes[].name`).
    #[serde(default)]
    pub request: Option<String>,
    /// Name of the response shape (matches `response_shapes[].name`).
    #[serde(default)]
    pub response: Option<String>,
    /// This endpoint streams rather than returning one body.
    #[serde(default)]
    pub streaming: bool,
}

/// A declared API version — header value, path segment or dated release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileVersion {
    /// Version token the wire carries (`2023-06-01`, `v1`, `beta`).
    pub id: String,
    /// Release date `YYYY-MM-DD`, when the spec publishes one.
    #[serde(default)]
    pub released: Option<String>,
    /// `current`, `beta`, `deprecated`, `sunset`.
    pub status: String,
}

/// A named request or response shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shape {
    pub name: String,
    /// `json`, `sse`, `jsonl`, `websocket-json`, `binary`, `text`.
    pub format: String,
    /// Load-bearing fields/events the shape always carries.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

/// One documented error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorShape {
    /// HTTP status (`401`, `429`, …) or transport-level kind.
    pub status: String,
    /// Machine-readable code/type the body carries (`rate_limit_exceeded`).
    pub code: String,
    /// Safe to retry the same request.
    #[serde(default)]
    pub retriable: bool,
}

/// A rate-limit signal: header name or HTTP status, and what it means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// `x-ratelimit-remaining-requests`, `retry-after`, `http-429`, …
    pub signal: String,
    pub meaning: String,
}

/// Streaming contract for the profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Streaming {
    /// `sse`, `websocket`, `webrtc`, `http-chunked`.
    pub format: String,
    /// Event/terminator tokens on the wire (`data: [DONE]`, event names).
    #[serde(default)]
    pub events: Vec<String>,
    #[serde(default)]
    pub notes: String,
}

/// A known deviation of a compatible host from this profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deviation {
    /// Host or component id that deviates (`openrouter`, `azure-openai`, …).
    pub host: String,
    /// Field or behavior affected.
    pub field: String,
    /// What the host does instead.
    pub behavior: String,
}

/// The full profile document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// Must equal [`PROFILE_VERSION`].
    pub version: String,
    /// Document kind; gates which fact classes [`validate`] requires.
    #[serde(default)]
    pub kind: ProfileKind,
    /// Profile id, e.g. `openai-chat-completions` (`valid_id` rules).
    pub id: String,
    pub name: String,
    /// Knowledge-base entity id this profile details (protocol, component
    /// or spec-version entity in `config/ecosystem/`).
    pub subject: String,
    pub endpoints: Vec<Endpoint>,
    pub auth: Vec<Auth>,
    pub versions: Vec<ProfileVersion>,
    #[serde(default)]
    pub request_shapes: Vec<Shape>,
    #[serde(default)]
    pub response_shapes: Vec<Shape>,
    #[serde(default)]
    pub errors: Vec<ErrorShape>,
    #[serde(default)]
    pub rate_limits: Vec<RateLimit>,
    #[serde(default)]
    pub streaming: Option<Streaming>,
    /// Canonical capability ids (`cap-*` from [`crate::eco_taxonomy`]).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Deviations of compatible hosts from this profile.
    #[serde(default)]
    pub deviations: Vec<Deviation>,
    pub provenance: Provenance,
}

/// What sort of document this is — decides which fact classes are required.
/// `Api` needs endpoints/auth/errors/rate-limits; `Format`, `Framework`,
/// `Platform` and `Catalog` documents (standards, licences, residency
/// tables) describe ecosystems, not wire surfaces, so they require only
/// versions + provenance and whatever fact classes they declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileKind {
    /// A wire API surface (the default).
    #[default]
    Api,
    /// A file/wire format or spec (GGUF, safetensors, OpenAPI).
    Format,
    /// A framework or integration surface (LangGraph, NIST AI RMF).
    Framework,
    /// A platform catalog (Bedrock, Vertex, NIM).
    Platform,
    /// A catalog of attributes (residency, disclosures, licences).
    Catalog,
}

impl Profile {
    /// What kind of document this is.
    #[must_use]
    pub fn is_api(&self) -> bool {
        self.kind == ProfileKind::Api
    }
}

/// One declared auth mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub scheme: AuthScheme,
    /// Header the credential rides (`authorization`, `x-api-key`).
    pub header: String,
    /// Value prefix (`Bearer `), empty for verbatim headers.
    #[serde(default)]
    pub prefix: String,
    /// Environment variable names hosts commonly read the key from.
    #[serde(default)]
    pub env: Vec<String>,
}

fn issue(path: impl Into<String>, message: impl Into<String>) -> Issue {
    Issue {
        stage: Stage::Model,
        path: path.into(),
        message: message.into(),
    }
}

/// Offline contract for a profile document. Empty means the profile is
/// well-formed and carries every fact class the profile tasks require.
#[must_use]
pub fn validate(p: &Profile) -> Vec<Issue> {
    let mut out = Vec::new();
    if p.version != PROFILE_VERSION {
        out.push(issue(
            "/version",
            format!(
                "profile version {:?}, expected {PROFILE_VERSION:?}",
                p.version
            ),
        ));
    }
    if !valid_id(&p.id) {
        out.push(issue(
            "/id",
            format!("profile id {:?} is not a valid id", p.id),
        ));
    }
    if !valid_id(&p.subject) {
        out.push(issue(
            "/subject",
            format!("subject {:?} is not a valid entity id", p.subject),
        ));
    }
    if !crate::eco_schema::valid_date(&p.provenance.retrieved) {
        out.push(issue(
            "/provenance/retrieved",
            format!("retrieved {:?} is not a real date", p.provenance.retrieved),
        ));
    }
    if p.provenance.source.trim().is_empty() {
        out.push(issue("/provenance/source", "source must not be empty"));
    }
    // API documents must describe a wire surface; catalog/format/framework
    // documents may legitimately have none.
    if p.is_api() && p.endpoints.is_empty() {
        out.push(issue("/endpoints", "profile declares no endpoints"));
    }
    for (i, e) in p.endpoints.iter().enumerate() {
        if e.method.trim().is_empty() || !e.path.starts_with('/') {
            out.push(issue(
                format!("/endpoints/{i}"),
                format!(
                    "endpoint {:?} {} lacks a method or absolute path",
                    e.method, e.path
                ),
            ));
        }
    }
    if p.is_api() && p.auth.is_empty() {
        out.push(issue(
            "/auth",
            "profile declares no auth scheme (use `none` to document an open API)",
        ));
    }
    for (i, a) in p.auth.iter().enumerate() {
        if a.scheme == AuthScheme::None && a.header != "none" {
            out.push(issue(
                format!("/auth/{i}"),
                "auth scheme `none` must use header `none`",
            ));
        }
        if a.scheme != AuthScheme::None && a.header.trim().is_empty() {
            out.push(issue(
                format!("/auth/{i}"),
                "auth scheme needs a header name",
            ));
        }
    }
    if p.is_api() && p.capabilities.is_empty() {
        out.push(issue(
            "/capabilities",
            "profile declares no capability tags",
        ));
    }
    for (i, c) in p.capabilities.iter().enumerate() {
        if !c.starts_with("cap-") || !valid_id(c) {
            out.push(issue(
                format!("/capabilities/{i}"),
                format!("capability tag {c:?} is not a canonical `cap-*` id"),
            ));
        }
    }
    if p.is_api() {
        if p.errors.is_empty() {
            out.push(issue("/errors", "profile documents no error shapes"));
        }
        if p.rate_limits.is_empty() {
            out.push(issue(
                "/rate_limits",
                "profile documents no rate-limit signals",
            ));
        }
    }
    if p.versions.is_empty() {
        out.push(issue("/versions", "profile declares no versions"));
    }
    // Shape references on endpoints resolve.
    let req: std::collections::HashSet<&str> =
        p.request_shapes.iter().map(|s| s.name.as_str()).collect();
    let resp: std::collections::HashSet<&str> =
        p.response_shapes.iter().map(|s| s.name.as_str()).collect();
    for (i, e) in p.endpoints.iter().enumerate() {
        if let Some(r) = &e.request {
            if !req.contains(r.as_str()) {
                out.push(issue(
                    format!("/endpoints/{i}/request"),
                    format!("request shape {r:?} is not declared"),
                ));
            }
        }
        if let Some(r) = &e.response {
            if !resp.contains(r.as_str()) {
                out.push(issue(
                    format!("/endpoints/{i}/response"),
                    format!("response shape {r:?} is not declared"),
                ));
            }
        }
    }
    out
}

/// Parse and validate a profile JSON document.
///
/// # Errors
/// `EaiError::config` listing every issue found.
pub fn parse(text: &str) -> EaiResult<Profile> {
    let p: Profile = serde_json::from_str(text)
        .map_err(|e| EaiError::config(format!("profile is not valid JSON: {e}")))?;
    let issues = validate(&p);
    if !issues.is_empty() {
        return Err(EaiError::config(format!(
            "profile {} has {} issue(s); first: {} {}",
            p.id,
            issues.len(),
            issues[0].path,
            issues[0].message
        )));
    }
    Ok(p)
}

/// Bundled profiles directory inside this repository
/// (`config/ecosystem/profiles/`), resolved from the crate manifest.
/// `SUSI_ECO_PROFILES` overrides (installed layouts, tests).
#[must_use]
pub fn bundled_profiles_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SUSI_ECO_PROFILES") {
        return PathBuf::from(d);
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/ecosystem/profiles")
}

/// Load one bundled profile by id.
///
/// # Errors
/// `EaiError::io`/`config` on read or validation failure.
pub fn load_bundled(id: &str) -> EaiResult<Profile> {
    let path = bundled_profiles_dir().join(format!("{id}.json"));
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    parse(&text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::Confidence;

    fn prov() -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        }
    }

    fn minimal() -> Profile {
        Profile {
            version: PROFILE_VERSION.into(),
            kind: ProfileKind::Api,
            id: "test-profile".into(),
            name: "Test".into(),
            subject: "test-proto".into(),
            endpoints: vec![Endpoint {
                method: "POST".into(),
                path: "/v1/test".into(),
                description: String::new(),
                request: Some("req".into()),
                response: Some("resp".into()),
                streaming: false,
            }],
            auth: vec![Auth {
                scheme: AuthScheme::Bearer,
                header: "authorization".into(),
                prefix: "Bearer ".into(),
                env: vec!["TEST_KEY".into()],
            }],
            versions: vec![ProfileVersion {
                id: "v1".into(),
                released: None,
                status: "current".into(),
            }],
            request_shapes: vec![Shape {
                name: "req".into(),
                format: "json".into(),
                fields: vec![],
                notes: String::new(),
            }],
            response_shapes: vec![Shape {
                name: "resp".into(),
                format: "json".into(),
                fields: vec![],
                notes: String::new(),
            }],
            errors: vec![ErrorShape {
                status: "429".into(),
                code: "rate_limit_exceeded".into(),
                retriable: true,
            }],
            rate_limits: vec![RateLimit {
                signal: "http-429".into(),
                meaning: "quota exhausted".into(),
            }],
            streaming: None,
            capabilities: vec!["cap-chat".into()],
            deviations: vec![],
            provenance: prov(),
        }
    }

    #[test]
    fn eco_profile_minimal_document_validates() {
        assert!(
            validate(&minimal()).is_empty(),
            "{:?}",
            validate(&minimal())
        );
    }

    #[test]
    fn eco_profile_flags_missing_fact_classes() {
        let mut p = minimal();
        p.endpoints.clear();
        p.auth.clear();
        p.capabilities.clear();
        p.errors.clear();
        p.rate_limits.clear();
        let issues = validate(&p);
        let paths: Vec<&str> = issues.iter().map(|i| i.path.as_str()).collect();
        for want in [
            "/endpoints",
            "/auth",
            "/capabilities",
            "/errors",
            "/rate_limits",
        ] {
            assert!(paths.contains(&want), "missing {want} in {paths:?}");
        }
    }

    #[test]
    fn eco_profile_dangling_shape_refs_flagged() {
        let mut p = minimal();
        p.endpoints[0].request = Some("ghost".into());
        assert!(validate(&p).iter().any(|i| i.message.contains("ghost")));
    }

    #[test]
    fn eco_profile_roundtrips_json() {
        let p = minimal();
        let text = serde_json::to_string_pretty(&p).unwrap();
        let back = parse(&text).unwrap();
        assert_eq!(back.id, "test-profile");
    }
}
