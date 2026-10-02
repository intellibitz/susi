//! Mastery verification for VC-201-066: secrets referenced, not embedded.
//!
//! The claim under test is not "secret:// URIs parse" — the cited
//! `vc_201_066` tests show that. The distinguishing properties are that
//! resolved values can never reach plan/export/tracing output and that the
//! scope check is an enforced boundary rather than a caller-asserted string.
//! These tests attack both.

use crate::secret_ref::{redact_for_export, SecretRef, SecretVault};
use crate::SusiConfig;

/// Falsification: `redact_for_export` only recognizes a whitespace-delimited
/// `resolved=` token. A resolved value appearing in any other export shape —
/// a JSON field, a quoted string — passes through verbatim, so "plan, export,
/// tracing never disclose resolved values" does not hold even for the one
/// function meant to prevent it.
#[test]
fn vc_201_066_mastery_export_redaction_only_masks_the_resolved_token() {
    let secret = "sk-live-9f3e-secret";
    // Any shape other than the bare `resolved=` token leaks the value.
    let json_out = redact_for_export(&format!(r#"{{"api_key": "{secret}"}}"#));
    assert!(
        json_out.contains(secret),
        "resolved value inside a JSON export is not redacted"
    );
    // Even the covered shape leaks: `resolved=` redacts only up to the next
    // whitespace, so a value containing spaces keeps its tail.
    let out = redact_for_export("resolved=sk live-9f3e");
    assert!(
        out.contains("live-9f3e"),
        "the whitespace-split tail of a resolved value survives: {out:?}"
    );
}

/// Falsification: the "authorized execution boundary" is a caller-supplied
/// scope string. Any code that asserts the right string resolves the secret —
/// nothing binds resolution to an actual boundary.
#[test]
fn vc_201_066_mastery_scope_guard_accepts_caller_asserted_scope() {
    let mut vault = SecretVault::default();
    vault.put("deploy", "api_key", "s3cr3t-body");
    let r = SecretRef::parse("secret://deploy/api_key").unwrap();
    // An arbitrary caller supplying the magic string resolves the secret.
    assert_eq!(vault.resolve(&r, "deploy").unwrap(), "s3cr3t-body");
    // The only guard that exists is string inequality.
    assert!(vault.resolve(&r, "other").is_err());
}

/// What does hold: reference display is the URI, never a value; wrong-scope
/// resolution is refused; and the canonical `resolved=<token>` export shape
/// is redacted.
#[test]
fn vc_201_066_mastery_reference_display_and_wrong_scope_hold() {
    let r = SecretRef::parse("secret://deploy/api_key").unwrap();
    assert_eq!(r.display(), "secret://deploy/api_key");
    let mut vault = SecretVault::default();
    vault.put("deploy", "api_key", "s3cr3t-body");
    assert!(vault.resolve(&r, "other").is_err());
    let out = redact_for_export("using secret://deploy/api_key resolved=s3cr3t-body");
    assert!(!out.contains("s3cr3t-body"));
    assert!(out.contains("resolved=<redacted>"));
}

/// The runtime config path the claim is about: plaintext credential material
/// stored in config is returned verbatim — nothing requires or resolves a
/// `secret://`/`key://` reference at the config boundary.
#[test]
fn vc_201_066_mastery_config_stores_and_returns_plaintext_secrets() {
    let mut settings = crate::SusiConfig::heal_defaults().clone();
    settings.insert(
        "custom_provider_key".into(),
        crate::json_util::DynamicValue::String("sk-plaintext-stored".into()),
    );
    let cfg = SusiConfig { settings };
    assert_eq!(
        cfg.get::<String>("custom_provider_key").unwrap(),
        "sk-plaintext-stored",
        "plaintext secret material round-trips through config untouched"
    );
}
