//! Credential masking shared by the public socket and the implementation owner.
use crate::DynamicRegistry;
use serde::{Deserialize, Serialize};
const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct GovernancePatterns {
    #[serde(flatten)]
    pub patterns: DynamicRegistry,
    #[serde(default)]
    pub secret_tokens: Vec<String>,
    #[serde(default)]
    pub destructive_commands: Vec<String>,
    #[serde(default)]
    pub critical_system_paths: Vec<String>,
    #[serde(default)]
    pub exfiltration_vectors: Vec<String>,
}

impl GovernancePatterns {
    pub fn secret_tokens(&self) -> Vec<String> {
        self.secret_tokens.clone()
    }
    pub fn destructive_commands(&self) -> Vec<String> {
        self.destructive_commands.clone()
    }
    pub fn critical_system_paths(&self) -> Vec<String> {
        self.critical_system_paths.clone()
    }
    pub fn exfiltration_vectors(&self) -> Vec<String> {
        self.exfiltration_vectors.clone()
    }
}

/// Whether a field name carries credential material: exact credential
/// names plus `*_api_key`, `*_key`, `*_token`, `*_secret`, `*_password`,
/// `*_credential(s)` suffixes (`-` normalizes to `_`). Suffixes — not
/// substrings — so `max_generation_tokens`, `tokenizer_filename` and
/// `api_key_env` (names of references, never bodies) stay visible.
#[doc(hidden)]
pub fn credential_field(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase().replace('-', "_");
    const EXACT: &[&str] = &[
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "credentials",
        "authorization",
        "proxy_authorization",
        "bearer",
        "auth",
        "resolved",
    ];
    const SUFFIX: &[&str] = &[
        "_api_key",
        "_key",
        "_token",
        "_secret",
        "_password",
        "_credential",
        "_credentials",
    ];
    let last = normalized.rsplit('.').next().unwrap_or(normalized.as_str());
    EXACT.contains(&normalized.as_str())
        || EXACT.contains(&last)
        || SUFFIX.iter().any(|suffix| normalized.ends_with(suffix))
}

fn is_key_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-')
}

struct FieldMatch<'a> {
    name: &'a str,
    /// Offset just past the separator and its trailing whitespace.
    keep_end: usize,
    /// Offset just past the value.
    value_end: usize,
    /// Quote byte wrapping the value, when it was quoted.
    quote: Option<u8>,
}

/// At offset 0 of `text`, match `[quote]name[quote] ws* [=:] ws* value`.
/// `scheme://rest` is a URI reference, not a field — the `//` after the
/// colon marks it, and references are the allowed export form.
fn match_field(text: &str) -> Option<FieldMatch<'_>> {
    let b = text.as_bytes();
    let mut i = 0usize;
    let quoted_name = matches!(b.first(), Some(b'"') | Some(b'\''));
    if quoted_name {
        i += 1;
    }
    let name_start = i;
    while i < b.len() && is_key_byte(b[i]) {
        i += 1;
    }
    if i == name_start {
        return None;
    }
    let name = &text[name_start..i];
    if quoted_name {
        if i >= b.len() || b[i] != b[0] {
            return None;
        }
        i += 1;
    }
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if i >= b.len() || !matches!(b[i], b'=' | b':') {
        return None;
    }
    i += 1;
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    if i >= b.len() || b[i..].starts_with(b"//") {
        return None;
    }
    let keep_end = i;
    let (value_end, quote) = if matches!(b[i], b'"' | b'\'') {
        let q = b[i];
        let mut j = i + 1;
        while j < b.len() {
            if b[j] == b'\\' {
                j = j.saturating_add(2);
                continue;
            }
            if b[j] == q {
                break;
            }
            j += 1;
        }
        (if j < b.len() { j + 1 } else { j }, Some(q))
    } else {
        // Bare value. `resolved=` names the whole resolved body, so its value
        // extends through whitespace; every other credential field is a single
        // whitespace-delimited token, so unrelated trailing text survives.
        let mut j = i;
        let stop = |b: u8| {
            matches!(
                b,
                b',' | b';' | b')' | b']' | b'}' | b'\n' | b'\r' | b'"' | b'\''
            ) || (name != "resolved" && matches!(b, b' ' | b'\t'))
        };
        while j < b.len() && !stop(b[j]) {
            j += 1;
        }
        let token = &text[i..j];
        let is_auth_field = name.eq_ignore_ascii_case("auth")
            || name.to_ascii_lowercase().ends_with("authorization");
        if is_auth_field
            && (token.eq_ignore_ascii_case("bearer")
                || token.eq_ignore_ascii_case("basic")
                || token.eq_ignore_ascii_case("token")
                || token.eq_ignore_ascii_case("digest"))
        {
            let mut k = j;
            while k < b.len() && matches!(b[k], b' ' | b'\t') {
                k += 1;
            }
            if k > j && k < b.len() && !stop(b[k]) {
                j = k;
                while j < b.len() && !stop(b[j]) {
                    j += 1;
                }
            }
        }
        (j, None)
    };
    Some(FieldMatch {
        name,
        keep_end,
        value_end,
        quote,
    })
}

/// `text` with every `resolved=…` token and credential-named field value
/// replaced by `[REDACTED]` — covering `key=value`, `key: value`,
/// `"key": "value"` (JSON), quoted values (`resolved="a b"`) and
/// `Authorization: Bearer …` headers. A bare value is a single
/// whitespace-delimited token, so unrelated trailing text survives. Field
/// names that merely contain credential substrings (`max_generation_tokens`,
/// `api_key_env`, `unresolved`) are not fields and stay visible; `scheme://`
/// URIs are never treated as fields.
#[must_use]
pub fn redact_for_export(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pos = 0usize;
    let mut copied = 0usize;
    while pos < text.len() {
        if !text.is_char_boundary(pos) {
            pos += 1;
            continue;
        }
        // A field name must start a token: `unresolved=x` does not contain
        // a `resolved` field.
        if pos == 0 || !is_key_byte(text.as_bytes()[pos - 1]) {
            if let Some(field) = match_field(&text[pos..]) {
                if credential_field(field.name) {
                    out.push_str(&text[copied..pos + field.keep_end]);
                    match field.quote {
                        Some(q) => {
                            out.push(q as char);
                            out.push_str(REDACTED);
                            out.push(q as char);
                        }
                        None => out.push_str(REDACTED),
                    }
                    pos += field.value_end;
                    copied = pos;
                    continue;
                }
            }
        }
        pos += 1;
    }
    out.push_str(&text[copied..]);
    // Redaction changes secrets only. Trimming an unrelated newline here
    // invalidates exact native-read receipts for otherwise unchanged files.
    out
}

#[allow(
    clippy::expect_used,
    reason = "The bundled configuration is checked source, matching the original SusiConfig fallback"
)]
fn bundled_governance() -> &'static GovernancePatterns {
    static DEFAULTS: std::sync::OnceLock<GovernancePatterns> = std::sync::OnceLock::new();
    DEFAULTS.get_or_init(|| {
        let defaults: serde_json::Value =
            serde_json::from_str(include_str!("../../../config/config.default.json"))
                .expect("bundled config.default.json must be valid JSON");
        serde_json::from_value(defaults["governance"].clone())
            .expect("bundled governance must be valid")
    })
}

fn configured_patterns() -> Vec<String> {
    let defaults = bundled_governance();
    let pattern = || -> Option<Vec<String>> {
        let bytes =
            crate::read_atomic_bytes(&susi_paths::SusiDirs::config_dir().join("config.json"))
                .ok()?;
        let settings: DynamicRegistry = serde_json::from_slice(&bytes).ok()?;
        let mut governance = settings.get("governance")?.clone();
        crate::merge_missing_json_defaults(&mut governance, &serde_json::to_value(defaults).ok()?);
        Some(
            serde_json::from_value::<GovernancePatterns>(governance)
                .ok()?
                .secret_tokens,
        )
    };
    pattern().unwrap_or_else(|| defaults.secret_tokens.clone())
}

/// Mask environment bodies, resolved/credential fields and explicit token patterns.
/// The implementation owner supplies its canonical healed configuration snapshot.
#[must_use]
pub fn redact_credentials_with(patterns: &[String], text: &str) -> String {
    let result = crate::susi_error::redact::mask_env_credentials(text);
    let result = redact_for_export(&result);
    crate::susi_error::redact::redact_patterns(patterns, &result)
}

/// The public socket's credential redactor. Read only the configured governance
/// fragment, heal missing values in memory and retain the original bundled fallback
/// on unreadable or malformed input; never rewrite unrelated configuration fields.
#[must_use]
pub fn redact_credentials(text: &str) -> String {
    redact_credentials_with(&configured_patterns(), text)
}
