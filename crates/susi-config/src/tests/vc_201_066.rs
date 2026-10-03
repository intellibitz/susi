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
