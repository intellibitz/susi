//! Canonical GMCP content model (VC-203-008).
//!
//! The vendored `rmcp::model` structs are the typed-API form the service
//! handlers consume, but their serde derives silently drop keys the schema
//! revision does not name — an extension field a future revision or a
//! permitted peer adds would vanish at the boundary. This module owns the
//! lossless representation the wire path validates and carries: every
//! content variant, annotation, icon, resource link, embedded resource,
//! and `_meta` object, with unknown keys captured in a `#[serde(flatten)]`
//! extension map so a `parse → serialize` round-trip preserves them
//! verbatim (`docs/gmcp/contract-203-008.md`).
//!
//! Validation is a boundary duty: `CanonicalContentBlock::from_wire` runs
//! the contract's checks — URI, MIME, base64, Unicode control characters,
//! audience/priority ranges — and fails with a typed `ContentError`,
//! never a silent coercion or a dropped field. `canonical_json` serializes
//! deterministically (serde_json maps are key-ordered) so equal values
//! hash identically in evidence artifacts.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Extra/extension members of a spec object — unknown keys round-trip
/// verbatim; never interpreted for dispatch or authorization.
pub type Extensions = Map<String, Value>;

/// A typed validation failure at the content boundary. Each variant maps
/// to a deterministic wire message; the gate answers with `-32600`/`-32602`
/// instead of silently coercing or dropping the payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContentError {
    /// The value is not one of the five spec content variants.
    UnknownVariant,
    /// A required spec field is absent or has the wrong JSON type.
    MissingField(&'static str),
    /// A string field carries a disallowed control character.
    BadUnicode(&'static str),
    /// A URI field fails absolute-URI parsing (scheme required).
    BadUri(&'static str),
    /// A `mimeType` field is not `type/subtype` token syntax.
    BadMimeType(&'static str),
    /// A binary field is not strict RFC 4648 base64.
    BadBase64(&'static str),
    /// `annotations.priority` is outside `0.0..=1.0`.
    BadPriority,
    /// `annotations.audience` holds a value other than `user`/`assistant`.
    BadAudience,
    /// An icon's `src` is not a valid URI or `data:` URI.
    BadIcon,
    /// The payload exceeds the bounded-resource limits.
    TooLarge,
}

impl ContentError {
    /// Deterministic wire message for the gate's typed error.
    pub fn wire_message(&self) -> String {
        match self {
            Self::UnknownVariant => "content block type is not a known variant".into(),
            Self::MissingField(f) => format!("content field `{f}` is missing or mistyped"),
            Self::BadUnicode(f) => format!("content field `{f}` carries a control character"),
            Self::BadUri(f) => format!("content field `{f}` is not a valid absolute URI"),
            Self::BadMimeType(f) => format!("content field `{f}` is not a valid MIME type"),
            Self::BadBase64(f) => format!("content field `{f}` is not strict base64"),
            Self::BadPriority => "annotations.priority must be within 0.0..=1.0".into(),
            Self::BadAudience => {
                "annotations.audience entries must be `user` or `assistant`".into()
            }
            Self::BadIcon => "icon src is not a valid URI".into(),
            Self::TooLarge => "content payload exceeds the bounded-resource limits".into(),
        }
    }
}

impl std::fmt::Display for ContentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.wire_message())
    }
}

impl std::error::Error for ContentError {}

/// Depth/node bound for a canonical payload — aligned with the schema
/// complexity bound in `protocol::compile_schema` so nested content trees
/// cannot become an unbounded-recursion vector.
const MAX_NODES: usize = 10_000;
const MAX_DEPTH: usize = 64;

fn bounded(value: &Value, depth: usize, remaining: &mut usize) -> bool {
    if depth > MAX_DEPTH || *remaining == 0 {
        return false;
    }
    *remaining -= 1;
    match value {
        Value::Object(map) => map.values().all(|v| bounded(v, depth + 1, remaining)),
        Value::Array(array) => array.iter().all(|v| bounded(v, depth + 1, remaining)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
    }
}

/// RFC 7230 `token` characters, the charset a MIME type/subtype permits.
fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(
                    b,
                    b'!' | b'#'
                        | b'$'
                        | b'%'
                        | b'&'
                        | b'\''
                        | b'*'
                        | b'+'
                        | b'-'
                        | b'.'
                        | b'^'
                        | b'_'
                        | b'`'
                        | b'|'
                        | b'~'
                )
        })
}

/// `type/subtype` with optional `; name=value` parameters — the grammar a
/// `mimeType` field carries in every pinned revision.
pub fn is_valid_mime_type(s: &str) -> bool {
    let (essence, params) = match s.split_once(';') {
        Some((e, p)) => (e.trim(), p),
        None => (s, ""),
    };
    let Some((ty, sub)) = essence.split_once('/') else {
        return false;
    };
    if !is_token(ty.trim()) || !is_token(sub.trim()) {
        return false;
    }
    params.split(';').all(|pair| {
        let pair = pair.trim();
        if pair.is_empty() {
            return true;
        }
        match pair.split_once('=') {
            Some((name, value)) => {
                let value = value.trim();
                let value_ok = is_token(value)
                    || (value.len() >= 2 && value.starts_with('"') && value.ends_with('"'));
                is_token(name.trim()) && value_ok
            }
            None => false,
        }
    })
}

/// Strict RFC 4648 base64: alphabet members only, `=` padding confined to
/// the last two positions, length a multiple of four.
pub fn is_valid_base64(s: &str) -> bool {
    if !s.len().is_multiple_of(4) {
        return false;
    }
    let mut padding = 0usize;
    for (i, b) in s.bytes().enumerate() {
        let last_two = i + 2 >= s.len();
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' | b'/' => {
                if padding > 0 {
                    return false;
                }
            }
            b'=' if last_two && padding < 2 => padding += 1,
            _ => return false,
        }
    }
    true
}

/// No C0/C1 control characters in wire strings — `\t`, `\n`, `\r` are the
/// only permitted whitespace controls (text blocks may carry newlines).
fn has_bad_control(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
}

/// Absolute URI (a scheme is required); `data:` URIs are permitted for
/// icon sources. `file:///x`, `https://…`, and custom `scheme:` forms all
/// satisfy the boundary — relative references never do.
pub fn is_valid_uri(s: &str) -> bool {
    if s.is_empty() || has_bad_control(s) {
        return false;
    }
    url::Url::parse(s).is_ok()
}

/// Spec `Annotations` — audience/priority/lastModified plus preserved
/// extension keys.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CanonicalAnnotations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "lastModified")]
    pub last_modified: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl CanonicalAnnotations {
    fn validate(&self) -> Result<(), ContentError> {
        if let Some(priority) = self.priority {
            if !(0.0..=1.0).contains(&priority) {
                return Err(ContentError::BadPriority);
            }
        }
        if let Some(audience) = &self.audience {
            if !audience
                .iter()
                .all(|r| matches!(r.as_str(), "user" | "assistant"))
            {
                return Err(ContentError::BadAudience);
            }
        }
        if let Some(ts) = &self.last_modified {
            if has_bad_control(ts) {
                return Err(ContentError::BadUnicode("annotations.lastModified"));
            }
        }
        Ok(())
    }
}

/// Spec `Icon` — a URI or `data:` URI source plus optional hints.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalIcon {
    pub src: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sizes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

impl CanonicalIcon {
    fn validate(&self) -> Result<(), ContentError> {
        if !is_valid_uri(&self.src) {
            return Err(ContentError::BadIcon);
        }
        if let Some(mime) = &self.mime_type {
            if !is_valid_mime_type(mime) {
                return Err(ContentError::BadMimeType("icons[].mimeType"));
            }
        }
        if let Some(theme) = &self.theme {
            if !matches!(theme.as_str(), "light" | "dark") {
                return Err(ContentError::BadIcon);
            }
        }
        Ok(())
    }
}

/// `text` content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextBlock {
    pub text: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<CanonicalAnnotations>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// `image`/`audio` content block — base64 media plus a MIME type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaBlock {
    pub data: String,
    pub mime_type: String,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<CanonicalAnnotations>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The `resource` member of an embedded-resource content block — text or
/// base64 blob contents of a URI-addressed resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CanonicalResourceContents {
    Text {
        uri: String,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        text: String,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Map<String, Value>>,
        #[serde(flatten)]
        extensions: Extensions,
    },
    Blob {
        uri: String,
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        blob: String,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Map<String, Value>>,
        #[serde(flatten)]
        extensions: Extensions,
    },
}

impl CanonicalResourceContents {
    fn validate(&self) -> Result<(), ContentError> {
        match self {
            Self::Text {
                uri,
                mime_type,
                text,
                ..
            } => {
                if !is_valid_uri(uri) {
                    return Err(ContentError::BadUri("resource.uri"));
                }
                if let Some(mime) = mime_type {
                    if !is_valid_mime_type(mime) {
                        return Err(ContentError::BadMimeType("resource.mimeType"));
                    }
                }
                if has_bad_control(text) {
                    return Err(ContentError::BadUnicode("resource.text"));
                }
                Ok(())
            }
            Self::Blob {
                uri,
                mime_type,
                blob,
                ..
            } => {
                if !is_valid_uri(uri) {
                    return Err(ContentError::BadUri("resource.uri"));
                }
                if let Some(mime) = mime_type {
                    if !is_valid_mime_type(mime) {
                        return Err(ContentError::BadMimeType("resource.mimeType"));
                    }
                }
                if !is_valid_base64(blob) {
                    return Err(ContentError::BadBase64("resource.blob"));
                }
                Ok(())
            }
        }
    }
}

/// `resource` (embedded resource) content block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResourceBlock {
    pub resource: CanonicalResourceContents,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<CanonicalAnnotations>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// `resource_link` content block — every `Resource` field plus preserved
/// extensions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLinkBlock {
    pub uri: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "mimeType")]
    pub mime_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icons: Option<Vec<CanonicalIcon>>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub annotations: Option<CanonicalAnnotations>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// The canonical lossless content union — one variant per spec
/// `ContentBlock` alternative, every variant preserving unknown keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CanonicalContentBlock {
    Text(TextBlock),
    Image(MediaBlock),
    Audio(MediaBlock),
    Resource(EmbeddedResourceBlock),
    ResourceLink(ResourceLinkBlock),
}

impl CanonicalContentBlock {
    /// Parse a wire `Value` into the canonical model and run boundary
    /// validation. Bounded before decode so an adversarial nesting cannot
    /// exhaust the validator; a failure is typed, never a coerced value.
    pub fn from_wire(value: &Value) -> Result<Self, ContentError> {
        let mut remaining = MAX_NODES;
        if !bounded(value, 0, &mut remaining) {
            return Err(ContentError::TooLarge);
        }
        let block: Self =
            serde_json::from_value(value.clone()).map_err(|_| ContentError::UnknownVariant)?;
        block.validate()?;
        Ok(block)
    }

    /// Boundary validation per `docs/gmcp/contract-203-008.md`: URI, MIME,
    /// base64, Unicode control characters, and annotation ranges.
    pub fn validate(&self) -> Result<(), ContentError> {
        let annotations = match self {
            Self::Text(b) => b.annotations.as_ref(),
            Self::Image(b) | Self::Audio(b) => b.annotations.as_ref(),
            Self::Resource(b) => b.annotations.as_ref(),
            Self::ResourceLink(b) => b.annotations.as_ref(),
        };
        if let Some(annotations) = annotations {
            annotations.validate()?;
        }
        match self {
            Self::Text(b) => {
                if has_bad_control(&b.text) {
                    return Err(ContentError::BadUnicode("text"));
                }
                Ok(())
            }
            Self::Image(b) | Self::Audio(b) => {
                let field = if matches!(self, Self::Image(_)) {
                    "image"
                } else {
                    "audio"
                };
                if !is_valid_mime_type(&b.mime_type) {
                    return Err(ContentError::BadMimeType(field));
                }
                if !is_valid_base64(&b.data) {
                    return Err(ContentError::BadBase64(field));
                }
                Ok(())
            }
            Self::Resource(b) => b.resource.validate(),
            Self::ResourceLink(b) => {
                if !is_valid_uri(&b.uri) {
                    return Err(ContentError::BadUri("resource_link.uri"));
                }
                if let Some(mime) = &b.mime_type {
                    if !is_valid_mime_type(mime) {
                        return Err(ContentError::BadMimeType("resource_link.mimeType"));
                    }
                }
                if let Some(icons) = &b.icons {
                    for icon in icons {
                        icon.validate()?;
                    }
                }
                for (field, value) in [
                    ("resource_link.name", &b.name),
                    ("resource_link.title", &b.title.clone().unwrap_or_default()),
                    (
                        "resource_link.description",
                        &b.description.clone().unwrap_or_default(),
                    ),
                ] {
                    if has_bad_control(value) {
                        return Err(ContentError::BadUnicode(field));
                    }
                }
                Ok(())
            }
        }
    }

    /// Deterministic canonical JSON: serde_json maps are key-ordered, so
    /// equal values serialize byte-identically — evidence hashes over
    /// canonical output are stable.
    pub fn canonical_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The canonical model as a wire `Value`, extension keys merged back
    /// into the object exactly where the peer placed them.
    pub fn to_wire(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The typed-API form service handlers consume — the explicit adapter
    /// between the canonical representation and `rmcp::model`. Every spec
    /// field maps exactly; extension keys live only on the canonical side
    /// (the typed model does not carry them, which is precisely why the
    /// canonical form exists).
    pub fn to_rmcp(&self) -> Result<susi_vendor_mcp_server::model::ContentBlock, ContentError> {
        serde_json::from_value(self.to_wire()).map_err(|_| ContentError::UnknownVariant)
    }
}

/// Validate one JSON-RPC payload's spec-defined content and URI fields —
/// the transport-independent boundary check `jsonrpc_gate` runs inside
/// `classify_envelope`. Returns `(code, error)`: `-32602` for a malformed
/// request `params` field, `-32600` for a malformed response payload.
///
/// Scope is deliberately narrow: only fields a pinned revision defines —
/// `params.uri` on resource methods, `params.ref.uri` on completions, and
/// `result` payloads that carry `content`/`roots` members a client
/// response to a server→client request can legally hold (sampling and
/// elicitation results). Tool `arguments`, `_meta` maps, and any
/// non-`type` `content` object (elicitation form data) pass through
/// untouched — the canonical model judges spec-shaped fields only.
pub fn validate_payload(object: &Map<String, Value>) -> Option<(i64, ContentError)> {
    let method = object.get("method").and_then(Value::as_str);
    let params = object.get("params").unwrap_or(&Value::Null);

    // URI-bearing requests: a malformed `uri`/`ref.uri` is an invalid
    // params failure, never a service-level surprise.
    let uri_param = match method {
        Some("resources/read") | Some("resources/subscribe") | Some("resources/unsubscribe") => {
            params.get("uri")
        }
        Some("completion/complete") => params.pointer("/ref/uri"),
        _ => None,
    };
    if let Some(uri) = uri_param {
        match uri.as_str() {
            Some(s) if is_valid_uri(s) => {}
            _ => return Some((-32602, ContentError::BadUri("params.uri"))),
        }
    }

    // Client responses: `result` members a sampling/elicitation/roots
    // answer may carry. `content` is validated only when it is spec
    // content — an array of `type`-tagged blocks or a single tagged block;
    // an untagged object is elicitation form data, not a content block.
    if let Some(result) = object.get("result").and_then(Value::as_object) {
        if let Some(content) = result.get("content") {
            let blocks: &[Value] = match content {
                Value::Array(items) => items.as_slice(),
                Value::Object(o) if o.contains_key("type") => std::slice::from_ref(content),
                Value::Object(_)
                | Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_) => &[],
            };
            for block in blocks {
                if !matches!(block, Value::Object(o) if o.contains_key("type")) {
                    return Some((-32600, ContentError::UnknownVariant));
                }
                if let Err(e) = CanonicalContentBlock::from_wire(block) {
                    return Some((-32600, e));
                }
            }
        }
        if let Some(roots) = result.get("roots").and_then(Value::as_array) {
            for root in roots {
                let uri_ok = root
                    .get("uri")
                    .and_then(Value::as_str)
                    .is_some_and(is_valid_uri);
                if !uri_ok {
                    return Some((-32600, ContentError::BadUri("result.roots[].uri")));
                }
            }
        }
    }
    None
}
