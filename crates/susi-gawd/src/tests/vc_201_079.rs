use crate::audit_evidence::{AuditEvidence, VerifyFailure};
use std::collections::BTreeMap;

#[test]
fn vc_201_079_export_verifies_and_redacts() {
    let mut a = AuditEvidence::new();
    a.register_key("k1", "super-secret");
    a.append("action=login actor=redacted", "k1").unwrap();
    a.append("action=read path=redacted", "k1").unwrap();
    let export = a.export();
    let mut keys = BTreeMap::new();
    keys.insert("k1".into(), "super-secret".into());
    assert!(AuditEvidence::verify(&export, &keys).is_ok());
    assert!(!export[0].redacted_body.contains("super-secret"));
}

#[test]
fn vc_201_079_distinct_failures() {
    let mut a = AuditEvidence::new();
    a.register_key("k1", "sec");
    a.append("a", "k1").unwrap();
    a.append("b", "k1").unwrap();
    let mut keys = BTreeMap::new();
    keys.insert("k1".into(), "sec".into());

    let mut truncated = a.export();
    truncated.pop();
    assert_eq!(
        AuditEvidence::verify_against_length(&truncated, &keys, 2),
        Err(VerifyFailure::Truncation)
    );

    let mut tampered = a.export();
    tampered[1].redacted_body = "evil".into();
    assert_eq!(
        AuditEvidence::verify(&tampered, &keys),
        Err(VerifyFailure::Tampering)
    );

    let mut missing = a.export();
    missing.remove(0);
    assert_eq!(
        AuditEvidence::verify(&missing, &keys),
        Err(VerifyFailure::MissingSegment)
    );

    let mut bad_keys = BTreeMap::new();
    bad_keys.insert("other".into(), "sec".into());
    assert_eq!(
        AuditEvidence::verify(&a.export(), &bad_keys),
        Err(VerifyFailure::UnknownKey)
    );
}
