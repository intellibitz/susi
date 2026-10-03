//! Mastery verification for VC-201-066: secrets referenced, not embedded.
//!
//! The claim under test is not "secret:// URIs parse" — the cited
//! `vc_201_066` tests show that. The distinguishing properties are that
//! resolved values can never reach plan/export/tracing output, that the
//! scope check is an enforced boundary rather than a caller-asserted
//! string, and that credentials stored in config resolve through scoped
//! references — these tests pin all three.

use crate::secret_ref::{redact_for_export, SecretRef, SecretVault};
use crate::SusiConfig;

/// Export redaction covers the shapes plan/tracing actually emit:
/// credential-named JSON fields, quoted `resolved=` values, and bare
/// `resolved=` bodies containing whitespace all lose their value — only
/// the field name survives — and the vault scrubs stored bodies wherever
/// they appear.
#[test]
fn vc_201_066_mastery_export_redaction_covers_every_shape() {
    let secret = "sk-live-9f3e-secret";
    let json_out = redact_for_export(&format!(r#"{{"api_key": "{secret}"}}"#));
    assert!(
        !json_out.contains(secret),
        "a resolved value inside a JSON export must be redacted: {json_out:?}"
    );
    assert!(json_out.contains(r#""api_key": "[REDACTED]""#));
    // A resolved value containing spaces loses the whole body, not just
    // the first whitespace-delimited token.
    let out = redact_for_export("resolved=sk live-9f3e");
    assert!(
        !out.contains("live-9f3e"),
        "the whitespace tail of a resolved value must not survive: {out:?}"
    );
    let out = redact_for_export(r#"resolved="sk live-9f3e""#);
    assert!(!out.contains("sk live-9f3e"));
    // The vault-aware pass masks a stored body wherever it appears, in
    // any surrounding shape.
    let mut vault = SecretVault::default();
    vault.put("deploy", "api_key", "sk live-9f3e");
    let out = vault.redact_for_export(r#"{"note": "value sk live-9f3e here"}"#);
    assert!(!out.contains("sk live-9f3e"));
}

/// The authorized execution boundary is an issued capability: the vault
/// exposes no resolve-with-a-scope method, `ScopeBoundary::resolve` takes
/// no scope argument, and a boundary refuses refs outside the scope it
/// was bound to at issuance — a shared `&SecretVault` cannot even mint
/// one (issuing needs `&mut`).
#[test]
fn vc_201_066_mastery_scope_is_a_held_capability() {
    let mut vault = SecretVault::default();
    vault.put("deploy", "api_key", "s3cr3t-body");
    let r = SecretRef::parse("secret://deploy/api_key").unwrap();
    // The owner delegates a deploy-scope boundary; it resolves deploy refs.
    let deploy = vault.issue_boundary("deploy");
    assert_eq!(deploy.scope(), "deploy");
    assert_eq!(deploy.resolve(&r).unwrap().expose(), "s3cr3t-body");
    // A boundary bound to another scope cannot reach deploy secrets —
    // authority lives in the held capability, not a call-site string.
    let other = vault.issue_boundary("other");
    assert!(other.resolve(&r).is_err());
    // A resolved body cannot be formatted into output: Debug renders the
    // marker, never the value.
    let resolved = deploy.resolve(&r).unwrap();
    assert!(!format!("{resolved:?}").contains("s3cr3t-body"));
}

/// What holds from the original claim: display is the URI, never a value;
/// wrong-scope resolution is refused; the canonical `resolved=` shape is
/// redacted; and the `secret://` reference itself survives export —
/// references are the allowed form, only bodies are masked.
#[test]
fn vc_201_066_mastery_reference_display_and_wrong_scope_hold() {
    let r = SecretRef::parse("secret://deploy/api_key").unwrap();
    assert_eq!(r.display(), "secret://deploy/api_key");
    let mut vault = SecretVault::default();
    vault.put("deploy", "api_key", "s3cr3t-body");
    assert!(vault.issue_boundary("other").resolve(&r).is_err());
    let out = redact_for_export("using secret://deploy/api_key resolved=s3cr3t-body");
    assert!(!out.contains("s3cr3t-body"));
    assert!(out.contains("resolved=[REDACTED]"));
    assert!(out.contains("secret://deploy/api_key"));
}

/// The config boundary the claim is about: a credential stored in config
/// is read back only as a scoped reference. Embedded plaintext fails the
/// credential parse; `key://`/`secret://`/`ENV_NAME` references resolve
/// through the scoped paths — the stored value is never returned
/// verbatim as a usable credential.
#[test]
fn vc_201_066_mastery_config_credentials_are_references_not_bodies() {
    let _lock = crate::env_test_lock();
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert(
        "custom_provider_key".into(),
        crate::json_util::DynamicValue::String("sk-plaintext-stored".into()),
    );
    let cfg = SusiConfig { settings };
    assert!(
        cfg.credential_ref("custom_provider_key").is_err(),
        "embedded plaintext is refused at the credential boundary"
    );

    // A key:// reference resolves through the scoped path, returning a
    // ScopedSecret whose render is redacted.
    unsafe {
        std::env::set_var("SUSI_VC066_CRED_TEST", "s3cr3t-via-ref");
    }
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert(
        "custom_provider_key".into(),
        crate::json_util::DynamicValue::String("key://env/SUSI_VC066_CRED_TEST".into()),
    );
    let cfg = SusiConfig { settings };
    let resolved = cfg
        .resolve_credential("custom_provider_key", std::path::Path::new("/nonexistent"))
        .unwrap();
    unsafe {
        std::env::remove_var("SUSI_VC066_CRED_TEST");
    }
    assert_eq!(resolved.value, "s3cr3t-via-ref");
    assert_eq!(
        resolved.render(),
        "key://env/SUSI_VC066_CRED_TEST = [redacted]"
    );

    // A secret:// ref resolves only through an issued boundary — no
    // unscoped resolution exists.
    let mut settings = SusiConfig::heal_defaults().clone();
    settings.insert(
        "deploy_key".into(),
        crate::json_util::DynamicValue::String("secret://deploy/api_key".into()),
    );
    let cfg = SusiConfig { settings };
    assert!(cfg
        .resolve_credential("deploy_key", std::path::Path::new("/nonexistent"))
        .is_err());
    let mut vault = SecretVault::default();
    vault.put("deploy", "api_key", "s3cr3t-body");
    let deploy = vault.issue_boundary("deploy");
    assert_eq!(
        cfg.resolve_scoped_credential("deploy_key", &deploy)
            .unwrap()
            .expose(),
        "s3cr3t-body"
    );
}
