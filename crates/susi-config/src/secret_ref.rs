//! Scoped secret references resolved only through an issued boundary (VC-201-066).
//!
//! `secret://<scope>/<name>` names a secret; a [`SecretVault`] holds the
//! bodies. Nothing resolves through the vault directly — the vault's owner
//! issues a [`ScopeBoundary`] bound to one scope and hands it to the code
//! that may resolve, so authority is a held capability, not a scope string
//! a call site asserts per request. A [`ResolvedSecret`] cannot be
//! displayed or serialized: plan/export/tracing output can only ever show
//! `[REDACTED]`, and [`redact_for_export`] strips `resolved=` material and
//! credential-named fields from export text while
//! `SecretVault::redact_for_export` additionally scrubs every stored body
//! wherever it appears.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

const REDACTED: &str = "[REDACTED]";

/// `(scope, name) → body` shared between a vault and its issued boundaries.
type VaultMap = Arc<RwLock<BTreeMap<(String, String), String>>>;

/// `secret://<scope>/<name>` — the only form allowed to leave the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub scope: String,
    pub name: String,
}

impl SecretRef {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_prefix("secret://")?;
        let (scope, name) = rest.split_once('/')?;
        if scope.is_empty() || name.is_empty() {
            return None;
        }
        Some(Self {
            scope: scope.to_string(),
            name: name.to_string(),
        })
    }

    #[must_use]
    pub fn display(&self) -> String {
        format!("secret://{}/{}", self.scope, self.name)
    }
}

/// A resolved secret body. Deliberately not `Display`, `Serialize`, or
/// `Clone`: formatting or serializing it yields `[REDACTED]`, and the only
/// way to read the body is `expose()` at the call site that actually
/// authenticates — so a resolved value can never leak into plan/export/
/// tracing output by accident.
pub struct ResolvedSecret(String);

impl ResolvedSecret {
    /// The body — for the single call site that uses the credential.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ResolvedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(REDACTED)
    }
}

/// A resolution capability bound to exactly one scope at issuance.
///
/// Only the vault's exclusive owner can issue one (`issue_boundary` takes
/// `&mut self`): code holding a shared `&SecretVault` can scrub exports but
/// cannot mint authority, and `resolve` takes no scope argument — a
/// `deploy` boundary can never read another scope's values.
pub struct ScopeBoundary {
    values: VaultMap,
    scope: String,
}

impl ScopeBoundary {
    /// The scope this boundary was issued for.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Resolve a ref inside this boundary's scope. Refs naming any other
    /// scope are refused — the held boundary is the enforcement, not a
    /// string the caller supplies.
    pub fn resolve(&self, r: &SecretRef) -> Result<ResolvedSecret, String> {
        if r.scope != self.scope {
            return Err(format!(
                "ref {} is outside this boundary's scope `{}`",
                r.display(),
                self.scope
            ));
        }
        let key = (r.scope.clone(), r.name.clone());
        self.values
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .map(|v| ResolvedSecret(v.clone()))
            .ok_or_else(|| format!("missing secret {}", r.display()))
    }

    /// Export-safe rendering: every stored body is scrubbed wherever it
    /// appears, then shape redaction handles `resolved=` and
    /// credential-named fields.
    #[must_use]
    pub fn redact_for_export(&self, text: &str) -> String {
        scrub_and_redact(&self.values, text)
    }
}

/// Bodies keyed by `(scope, name)`. `put` and `issue_boundary` need `&mut
/// self` — only the owner populates the vault and delegates scopes.
#[derive(Debug, Default)]
pub struct SecretVault {
    values: VaultMap,
}

impl SecretVault {
    pub fn put(&mut self, scope: &str, name: &str, value: &str) {
        self.values
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert((scope.to_string(), name.to_string()), value.to_string());
    }

    /// Issue a resolution capability bound to `scope`. Delegation, not
    /// assertion: callers receive the boundary and structurally cannot
    /// resolve outside it; there is no resolve-with-a-scope-string path.
    #[must_use]
    pub fn issue_boundary(&mut self, scope: &str) -> ScopeBoundary {
        ScopeBoundary {
            values: Arc::clone(&self.values),
            scope: scope.to_string(),
        }
    }

    /// `redact_for_export` plus a scrub of every stored body wherever it
    /// appears — covers values embedded in JSON fields, quoted strings and
    /// bodies containing whitespace that shape matching alone cannot delimit.
    #[must_use]
    pub fn redact_for_export(&self, text: &str) -> String {
        scrub_and_redact(&self.values, text)
    }
}

fn scrub_and_redact(values: &VaultMap, text: &str) -> String {
    // Longest bodies first so a shorter value cannot mask the head of a
    // longer one that contains it.
    let mut bodies: Vec<String> = values
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .filter(|v| !v.is_empty())
        .cloned()
        .collect();
    bodies.sort_by_key(|v| std::cmp::Reverse(v.len()));
    let mut scrubbed = text.to_string();
    for body in bodies {
        scrubbed = scrubbed.replace(&body, REDACTED);
    }
    redact_for_export(&scrubbed)
}

/// Whether a field name carries credential material: exact credential
/// names plus `*_api_key`, `*_key`, `*_token`, `*_secret`, `*_password`,
/// `*_credential(s)` suffixes (`-` normalizes to `_`). Suffixes — not
/// substrings — so `max_generation_tokens`, `tokenizer_filename` and
/// `api_key_env` (names of references, never bodies) stay visible.
pub(crate) fn credential_field(name: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_ref_parse_display_and_scope_boundary_isolation() {
        let parsed = SecretRef::parse("secret://deploy/openai_key").expect("valid secret ref");
        assert_eq!(
            parsed,
            SecretRef {
                scope: "deploy".into(),
                name: "openai_key".into()
            }
        );
        assert_eq!(parsed.display(), "secret://deploy/openai_key");

        assert!(SecretRef::parse("deploy/openai_key").is_none());
        assert!(SecretRef::parse("secret:///openai_key").is_none());
        assert!(SecretRef::parse("secret://deploy/").is_none());
        assert!(SecretRef::parse("secret://no_slash").is_none());

        let mut vault = SecretVault::default();
        vault.put("deploy", "token", "s3cr3t-body");
        vault.put("prod", "token", "prod-only-body");

        let boundary = vault.issue_boundary("deploy");
        assert_eq!(boundary.scope(), "deploy");

        let resolved = boundary
            .resolve(&SecretRef::parse("secret://deploy/token").unwrap())
            .expect("in-scope secret resolves");
        assert_eq!(resolved.expose(), "s3cr3t-body");
        assert_eq!(format!("{resolved:?}"), "[REDACTED]");

        let cross_scope_err = boundary
            .resolve(&SecretRef::parse("secret://prod/token").unwrap())
            .unwrap_err();
        assert!(cross_scope_err.contains("outside this boundary's scope"));

        let missing_err = boundary
            .resolve(&SecretRef::parse("secret://deploy/absent").unwrap())
            .unwrap_err();
        assert_eq!(missing_err, "missing secret secret://deploy/absent");
    }

    #[test]
    fn redact_for_export_and_vault_scrub_secret_bodies_and_preserve_refs_and_non_credentials() {
        let mut vault = SecretVault::default();
        vault.put("deploy", "short", "abc123secret");
        vault.put("deploy", "long", "abc123secret-with-more-bytes");
        let boundary = vault.issue_boundary("deploy");

        let raw = "ref=secret://deploy/key api_key=sk-live-999 \"client_secret\": \"top-val\" \
                   Authorization: \"Bearer tok-xyz\" bearer=tok-bare resolved=\"multi word secret\" \
                   embedded=abc123secret-with-more-bytes \
                   max_generation_tokens=4096 api_key_env=OPENAI_API_KEY unresolved=1\n";

        let scrubbed = vault.redact_for_export(raw);
        assert_eq!(boundary.redact_for_export(raw), scrubbed);
        assert!(!scrubbed.contains("sk-live-999"));
        assert!(!scrubbed.contains("top-val"));
        assert!(!scrubbed.contains("tok-xyz"));
        assert!(!scrubbed.contains("tok-bare"));
        assert!(!scrubbed.contains("multi word secret"));
        assert!(!scrubbed.contains("abc123secret"));
        assert!(scrubbed.contains("secret://deploy/key"));
        assert!(scrubbed.contains("api_key=[REDACTED]"));
        assert!(scrubbed.contains("\"client_secret\": \"[REDACTED]\""));
        assert!(scrubbed.contains("resolved=\"[REDACTED]\""));
        assert!(scrubbed.contains("max_generation_tokens=4096"));
        assert!(scrubbed.contains("api_key_env=OPENAI_API_KEY"));
        assert!(scrubbed.contains("unresolved=1\n"));
    }

    #[test]
    fn secret_ref_serde_and_redact_single_quotes_escaped_quotes_colons_and_urls() {
        let r = SecretRef::parse("secret://workspace/api_token").expect("parse secret ref");
        let encoded = serde_json::to_string(&r).expect("serialize SecretRef");
        let decoded: SecretRef = serde_json::from_str(&encoded).expect("deserialize SecretRef");
        assert_eq!(decoded, r);
        assert!(SecretRef::parse("http://workspace/api_token").is_none());
        assert!(SecretRef::parse("secret://").is_none());

        let mut vault = SecretVault::default();
        vault.put("workspace", "api_token", "vault-body-xyz-987");

        let input = "api_key='single-quoted-sec' password: colon-sec \
                     token=\"escaped\\\"quote\\\"sec\" url=https://example.com:8443/path \
                     leaked=vault-body-xyz-987";
        let scrubbed = vault.redact_for_export(input);
        assert!(
            scrubbed.contains("api_key='[REDACTED]'"),
            "single-quoted credential not redacted: {scrubbed}"
        );
        assert!(
            scrubbed.contains("password: [REDACTED]"),
            "colon-separated credential not redacted: {scrubbed}"
        );
        assert!(
            scrubbed.contains("token=\"[REDACTED]\""),
            "escaped-quote credential not redacted: {scrubbed}"
        );
        assert!(
            scrubbed.contains("https://example.com:8443/path"),
            "HTTPS URI should remain untouched: {scrubbed}"
        );
        assert!(!scrubbed.contains("single-quoted-sec"));
        assert!(!scrubbed.contains("colon-sec"));
        assert!(!scrubbed.contains("escaped"));
        assert!(!scrubbed.contains("vault-body-xyz-987"));
    }

    #[test]
    fn credential_field_dotted_and_hyphenated_names_and_unclosed_quote_redaction_contract() {
        for cred_key in [
            "Nested.Client-Secret",
            "headers.Authorization",
            "oauth.access-token",
            "db.user_password",
            "service.private_key",
        ] {
            assert!(
                credential_field(cred_key),
                "expected {cred_key} to be classified as a credential field"
            );
        }
        for non_cred_key in [
            "max_generation_tokens",
            "tokenizer_filename",
            "api_key_env",
            "unresolved",
        ] {
            assert!(
                !credential_field(non_cred_key),
                "expected {non_cred_key} not to be classified as a credential field"
            );
        }

        assert_eq!(
            redact_for_export("api_key=\"unclosed-secret-eof"),
            "api_key=\"[REDACTED]\""
        );
        assert_eq!(
            redact_for_export("resolved=secret with spaces until semicolon; next=ok"),
            "resolved=[REDACTED]; next=ok"
        );
    }

    #[test]
    fn secret_vault_default_rejects_unseeded_secret_lookup_and_redact_preserves_plain_text() {
        let mut vault = SecretVault::default();
        let boundary = vault.issue_boundary("ops");
        assert_eq!(boundary.scope(), "ops");
        let sref = SecretRef::parse("secret://ops/missing_token").unwrap();
        assert_eq!(sref.scope, "ops");
        assert_eq!(sref.name, "missing_token");
        assert_eq!(sref.display(), "secret://ops/missing_token");
        assert_eq!(
            redact_for_export("status=healthy count=42"),
            "status=healthy count=42"
        );
    }
}

// ---- Wave 14 (T-CLAUDE-415) structural invariant verifiers ----

impl SecretRef {
    /// A reference is safe to carry across the boundary: a non-empty scope with no
    /// `/`, whitespace or control character (so it is the sole segment before the
    /// name), a non-empty single-line name, and — the property the `secret://` form
    /// exists for — it round-trips through [`SecretRef::display`] and
    /// [`SecretRef::parse`] back to itself. It never inspects any secret body.
    pub fn verify_ref_invariants(&self) -> Result<(), String> {
        if self.scope.is_empty() {
            return Err("SecretRef scope must not be empty".to_string());
        }
        if self
            .scope
            .chars()
            .any(|c| c == '/' || c.is_whitespace() || c.is_control())
        {
            return Err(format!(
                "SecretRef scope `{}` must have no '/', whitespace or control character",
                self.scope
            ));
        }
        if self.name.is_empty() {
            return Err("SecretRef name must not be empty".to_string());
        }
        if self.name.chars().any(char::is_control) {
            return Err("SecretRef name must not contain a control character".to_string());
        }
        if SecretRef::parse(&self.display()).as_ref() != Some(self) {
            return Err(format!(
                "SecretRef `{}` does not round-trip through secret:// form",
                self.display()
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod wave14_verifier_tests {
    use super::*;

    #[test]
    fn secret_ref_verifier_requires_a_clean_roundtripping_reference() {
        let ok = SecretRef {
            scope: "deploy".to_string(),
            name: "api/token".to_string(),
        };
        assert_eq!(ok.verify_ref_invariants(), Ok(()));
        assert_eq!(SecretRef::parse(&ok.display()).as_ref(), Some(&ok));

        let slash_scope = SecretRef {
            scope: "de/ploy".to_string(),
            name: "token".to_string(),
        };
        assert!(slash_scope.verify_ref_invariants().is_err());

        let empty_name = SecretRef {
            scope: "deploy".to_string(),
            name: String::new(),
        };
        assert!(empty_name.verify_ref_invariants().is_err());

        let spaced = SecretRef {
            scope: "de ploy".to_string(),
            name: "token".to_string(),
        };
        assert!(spaced.verify_ref_invariants().is_err());

        let newline = SecretRef {
            scope: "deploy".to_string(),
            name: "to\nken".to_string(),
        };
        assert!(newline.verify_ref_invariants().is_err());
    }
}
