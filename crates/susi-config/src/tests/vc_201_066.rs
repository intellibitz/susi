use crate::secret_ref::{redact_for_export, SecretRef, SecretVault};

#[test]
fn vc_201_066_parse_and_scope_guard() {
    let r = SecretRef::parse("secret://deploy/api_key").expect("parse");
    assert_eq!(r.scope, "deploy");
    assert_eq!(r.name, "api_key");
    let mut v = SecretVault::default();
    v.put("deploy", "api_key", "super-secret");
    assert!(v.issue_boundary("other").resolve(&r).is_err());
    assert_eq!(
        v.issue_boundary("deploy").resolve(&r).unwrap().expose(),
        "super-secret"
    );
}

#[test]
fn vc_201_066_plan_export_never_discloses() {
    let out = redact_for_export("using secret://deploy/api_key resolved=super-secret");
    assert!(!out.contains("super-secret"));
    assert!(out.contains("[REDACTED]"));
}

#[test]
fn vc_201_066_display_is_reference_not_value() {
    let r = SecretRef::parse("secret://a/b").unwrap();
    assert_eq!(r.display(), "secret://a/b");
}

#[test]
fn vc_201_066_invalid_secret_ref_syntax_and_missing_secret_resolution_fail_cleanly() {
    assert!(SecretRef::parse("http://not-secret/a").is_none());
    assert!(SecretRef::parse("secret://missing_slash").is_none());
    assert!(SecretRef::parse("secret:///empty_scope").is_none());
    assert!(SecretRef::parse("secret://empty_name/").is_none());

    let mut v = SecretVault::default();
    let boundary = v.issue_boundary("agent_a");
    assert_eq!(boundary.scope(), "agent_a");
    let missing_ref = SecretRef::parse("secret://agent_a/missing_key").unwrap();
    assert!(boundary.resolve(&missing_ref).is_err());
}
