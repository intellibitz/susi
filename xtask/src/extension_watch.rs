//! Upstream extension watch: diff the published extension list against the
//! extensions this repo already knows about and turn every freshly published
//! official extension into exactly one deterministic roadmap item.
//!
//! Everything here is pure: no network, no filesystem, no clock, no
//! randomness. `parse_published` decodes the registry document with a small
//! self-contained JSON reader so xtask keeps its zero-dependency surface, and
//! `diff_new_official` is a straight function of its inputs.

/// One entry of the upstream published-extensions document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedExtension {
    /// Stable extension identifier, e.g. `vendor/name`.
    pub id: String,
    /// Publication status, e.g. `official` or `experimental`.
    pub status: String,
    /// URL of the extension spec/contract document.
    pub spec_url: String,
}

/// One roadmap entry produced for a newly published official extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoadmapItem {
    /// Stable roadmap key: `ext-watch-<id>` with `/` folded to `-`.
    pub key: String,
    /// Human-readable title.
    pub title: String,
    /// The upstream extension id this item tracks.
    pub extension_id: String,
}

/// Roadmap key prefix for extension-watch items.
const KEY_PREFIX: &str = "ext-watch-";

/// Returns one roadmap item per published extension that is `official`
/// (case-insensitive) and not listed in `known_ids`.
///
/// Experimental entries and already-known ids are skipped, duplicate ids in
/// `published` collapse to a single item, and the result is sorted by
/// extension id so the output is deterministic.
pub fn diff_new_official(
    published: &[PublishedExtension],
    known_ids: &[String],
) -> Vec<RoadmapItem> {
    let mut ids: Vec<&str> = published
        .iter()
        .filter(|ext| ext.status.eq_ignore_ascii_case("official"))
        .filter(|ext| !known_ids.iter().any(|known| known == &ext.id))
        .map(|ext| ext.id.as_str())
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids.into_iter()
        .map(|id| RoadmapItem {
            key: format!("{KEY_PREFIX}{}", id.replace('/', "-")),
            title: format!("Watch upstream extension {id}"),
            extension_id: id.to_string(),
        })
        .collect()
}
/// Parses the upstream published-extensions JSON document.
///
/// Accepts either a bare array of extension objects or an object holding that
/// array under a well-known key (`extensions`, `published`, `items`, `data`,
/// `results`, or failing that the first array of objects found). Each entry
/// must carry a string `id`; `status` and `spec_url` default to `""` when
/// absent (camelCase aliases accepted).
///
/// Malformed input is reported as an explicit `Err` describing the problem.
pub fn parse_published(json: &str) -> Result<Vec<PublishedExtension>, String> {
    let mut parser = JsonParser::new(json);
    let root = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.src.len() {
        return Err(format!(
            "invalid JSON: trailing data at byte {}",
            parser.pos
        ));
    }
    let entries = extension_entries(&root)
        .ok_or_else(|| "invalid JSON: expected a list of extension objects".to_string())?;
    let mut out: Vec<PublishedExtension> = Vec::with_capacity(entries.len());
    for entry in entries {
        if let Json::Object(fields) = entry {
            if let Some(id) = field_str(fields, &["id", "extension_id", "extensionId", "slug"]) {
                out.push(PublishedExtension {
                    id: id.to_string(),
                    status: field_str(fields, &["status", "state"])
                        .unwrap_or("")
                        .to_string(),
                    spec_url: field_str(fields, &["spec_url", "specUrl", "spec", "url"])
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
    }
    if out.is_empty() && !entries.is_empty() {
        return Err("invalid JSON: extension entries must carry a string `id`".to_string());
    }
    Ok(out)
}

/// Locates the array of extension objects inside a decoded document.
fn extension_entries(root: &Json) -> Option<&[Json]> {
    match root {
        Json::Array(items) => Some(items.as_slice()),
        Json::Object(fields) => {
            for key in ["extensions", "published", "items", "data", "results"] {
                if let Some(Json::Array(items)) = field(fields, key) {
                    return Some(items.as_slice());
                }
            }
            fields.iter().find_map(|(_, value)| match value {
                Json::Array(items) if items.iter().any(|item| matches!(item, Json::Object(_))) => {
                    Some(items.as_slice())
                }
                Json::Array(_)
                | Json::Null
                | Json::Bool(_)
                | Json::Number(_)
                | Json::Str(_)
                | Json::Object(_) => None,
            })
        }
        Json::Null | Json::Bool(_) | Json::Number(_) | Json::Str(_) => None,
    }
}

/// Looks up an object field by exact name.
fn field<'a>(fields: &'a [(String, Json)], name: &str) -> Option<&'a Json> {
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// Looks up the first alias present as a string field.
fn field_str<'a>(fields: &'a [(String, Json)], names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| match field(fields, name) {
        Some(Json::Str(text)) => Some(text.as_str()),
        _ => None,
    })
}
/// Decoded JSON value (only what the registry document needs).
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(String),
    Str(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// Minimal recursive-descent JSON reader.
struct JsonParser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            src: text.as_bytes(),
            pos: 0,
        }
    }

    fn err(&self, message: &str) -> String {
        format!("invalid JSON at byte {}: {message}", self.pos)
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek();
        if byte.is_some() {
            self.pos += 1;
        }
        byte
    }

    fn skip_ws(&mut self) {
        while let Some(byte) = self.peek() {
            if matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn parse_value(&mut self) -> Result<Json, String> {
        self.skip_ws();
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_string().map(Json::Str),
            Some(b't') => self.parse_literal("true", Json::Bool(true)),
            Some(b'f') => self.parse_literal("false", Json::Bool(false)),
            Some(b'n') => self.parse_literal("null", Json::Null),
            Some(b'-') | Some(b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.err("unexpected character")),
            None => Err(self.err("unexpected end of input")),
        }
    }

    fn parse_literal(&mut self, literal: &str, value: Json) -> Result<Json, String> {
        let end = self.pos + literal.len();
        if self.src.get(self.pos..end) == Some(literal.as_bytes()) {
            self.pos = end;
            Ok(value)
        } else {
            Err(self.err("invalid literal"))
        }
    }

    fn parse_array(&mut self) -> Result<Json, String> {
        self.pos += 1; // consume '['
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.bump() {
                Some(b',') => self.skip_ws(),
                Some(b']') => return Ok(Json::Array(items)),
                Some(_) => return Err(self.err("expected ',' or ']'")),
                None => return Err(self.err("unterminated array")),
            }
        }
    }

    fn parse_object(&mut self) -> Result<Json, String> {
        self.pos += 1; // consume '{'
        let mut fields: Vec<(String, Json)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_ws();
            let key = self.parse_string()?;
            self.skip_ws();
            if self.bump() != Some(b':') {
                return Err(self.err("expected ':' after object key"));
            }
            let value = self.parse_value()?;
            fields.push((key, value));
            self.skip_ws();
            match self.bump() {
                Some(b',') => self.skip_ws(),
                Some(b'}') => return Ok(Json::Object(fields)),
                Some(_) => return Err(self.err("expected ',' or '}'")),
                None => return Err(self.err("unterminated object")),
            }
        }
    }
    fn parse_string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(self.err("expected string"));
        }
        self.pos += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let byte = self.bump().ok_or_else(|| self.err("unterminated string"))?;
            match byte {
                b'"' => {
                    return String::from_utf8(out)
                        .map_err(|_| self.err("string is not valid UTF-8"))
                }
                b'\\' => {
                    let escape = self
                        .bump()
                        .ok_or_else(|| self.err("unterminated escape sequence"))?;
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let ch = self.parse_unicode_escape()?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err(self.err("invalid escape sequence")),
                    }
                }
                other => out.push(other),
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<char, String> {
        let high = self.parse_hex4()?;
        let code = if (0xd800..=0xdbff).contains(&high) {
            if self.bump() != Some(b'\\') || self.bump() != Some(b'u') {
                return Err(self.err("invalid surrogate pair"));
            }
            let low = self.parse_hex4()?;
            if !(0xdc00..=0xdfff).contains(&low) {
                return Err(self.err("invalid surrogate pair"));
            }
            0x1_0000 + ((u32::from(high) - 0xd800) << 10) + (u32::from(low) - 0xdc00)
        } else {
            u32::from(high)
        };
        char::from_u32(code).ok_or_else(|| self.err("invalid unicode code point"))
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        let mut value: u16 = 0;
        for _ in 0..4 {
            let byte = self
                .bump()
                .ok_or_else(|| self.err("truncated unicode escape"))?;
            let digit = match byte {
                b'0'..=b'9' => u16::from(byte - b'0'),
                b'a'..=b'f' => u16::from(byte - b'a') + 10,
                b'A'..=b'F' => u16::from(byte - b'A') + 10,
                _ => return Err(self.err("invalid hex digit")),
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Json, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        let mut digits = 0usize;
        while let Some(byte) = self.peek() {
            match byte {
                b'0'..=b'9' => {
                    self.pos += 1;
                    digits += 1;
                }
                b'.' | b'e' | b'E' | b'+' | b'-' => self.pos += 1,
                _ => break,
            }
        }
        if digits == 0 {
            return Err(self.err("invalid number"));
        }
        let text = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| self.err("invalid number"))?;
        Ok(Json::Number(text.to_string()))
    }
}
#[cfg(test)]
mod tests {
    use super::{diff_new_official, parse_published, PublishedExtension, RoadmapItem};

    fn published(id: &str, status: &str) -> PublishedExtension {
        PublishedExtension {
            id: id.to_string(),
            status: status.to_string(),
            spec_url: format!("https://specs.example/{id}.json"),
        }
    }

    fn keys(items: &[RoadmapItem]) -> Vec<String> {
        items.iter().map(|item| item.key.clone()).collect()
    }

    #[test]
    fn extension_watch_new_official_yields_one_item() {
        let published_list = [published("acme/tool", "official")];
        let items = diff_new_official(&published_list, &[]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "ext-watch-acme-tool");
        assert_eq!(items[0].extension_id, "acme/tool");
        assert!(!items[0].title.is_empty());
    }

    #[test]
    fn extension_watch_status_match_is_case_insensitive() {
        let published_list = [published("acme/tool", "Official")];
        assert_eq!(
            keys(&diff_new_official(&published_list, &[])),
            vec!["ext-watch-acme-tool".to_string()]
        );
    }

    #[test]
    fn extension_watch_experimental_is_skipped() {
        let published_list = [
            published("acme/exp", "experimental"),
            published("acme/official", "official"),
        ];
        assert_eq!(
            keys(&diff_new_official(&published_list, &[])),
            vec!["ext-watch-acme-official".to_string()]
        );
    }

    #[test]
    fn extension_watch_known_id_is_skipped() {
        let published_list = [published("acme/known", "official")];
        let known = vec!["acme/known".to_string()];
        assert!(diff_new_official(&published_list, &known).is_empty());
    }

    #[test]
    fn extension_watch_duplicate_dedupe_yields_single_item() {
        let published_list = [
            published("acme/tool", "official"),
            published("acme/tool", "official"),
        ];
        let items = diff_new_official(&published_list, &[]);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].key, "ext-watch-acme-tool");
    }

    #[test]
    fn extension_watch_ordering_is_deterministic() {
        let published_list = [
            published("zeta/tool", "official"),
            published("alpha/tool", "official"),
            published("mid/tool", "official"),
        ];
        let forward = diff_new_official(&published_list, &[]);
        let mut reversed_input = published_list.to_vec();
        reversed_input.reverse();
        let backward = diff_new_official(&reversed_input, &[]);
        assert_eq!(forward, backward);
        assert_eq!(
            keys(&forward),
            vec![
                "ext-watch-alpha-tool".to_string(),
                "ext-watch-mid-tool".to_string(),
                "ext-watch-zeta-tool".to_string(),
            ]
        );
    }

    #[test]
    fn extension_watch_parse_published_reads_ids_status_and_spec_url() {
        let json = r#"[
            {"id": "acme/tool", "status": "official", "spec_url": "https://specs.example/a.json"},
            {"id": "acme/exp", "status": "experimental", "spec_url": "https://specs.example/b.json"}
        ]"#;
        let parsed = parse_published(json).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "acme/tool");
        assert_eq!(parsed[0].status, "official");
        assert_eq!(parsed[0].spec_url, "https://specs.example/a.json");
        assert_eq!(parsed[1].id, "acme/exp");
        assert_eq!(parsed[1].status, "experimental");
        assert_eq!(parsed[1].spec_url, "https://specs.example/b.json");
        assert_eq!(
            keys(&diff_new_official(&parsed, &[])),
            vec!["ext-watch-acme-tool".to_string()]
        );
    }

    #[test]
    fn extension_watch_parse_published_accepts_wrapped_document() {
        let json =
            r#"{"extensions": [{"id": "acme/tool", "status": "official", "spec_url": "u"}]}"#;
        let parsed = parse_published(json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "acme/tool");
        assert_eq!(parsed[0].spec_url, "u");
    }

    #[test]
    fn extension_watch_malformed_json_is_an_error() {
        for bad in [
            "",
            "not json",
            "[{\"id\": \"acme/tool\", \"status\": \"official\"",
            "[{\"id\": }]",
            "{\"extensions\": [}]",
            "[{\"name\": \"acme/tool\"}]",
            "[] trailing",
        ] {
            let err = parse_published(bad).unwrap_err();
            assert!(!err.is_empty(), "expected an error message for {bad:?}");
        }
        // Well-formed but extension-free documents are an error too.
        assert!(parse_published("\"just a string\"").is_err());
        assert!(parse_published("[]").unwrap().is_empty());
    }
}
