use crate::skill_artifacts::{import_skill, skill_sign, ImportVerdict, SkillArtifact};
use std::collections::BTreeSet;

#[test]
fn vc_201_085_executable_after_provenance_and_tests() {
    let art = SkillArtifact {
        name: "reflex-pack".into(),
        version: "1.0.0".into(),
        reflexes: vec!["r1".into()],
        prompts: vec!["p1".into()],
        fixtures: vec!["f1".into()],
        capabilities: BTreeSet::from(["exec".into()]),
        signature: skill_sign("alice", "reflex-pack", "1.0.0"),
        signer: "alice".into(),
    };
    let trusted = BTreeSet::from(["alice".into()]);
    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::Executable
    );
}

#[test]
fn vc_201_085_rejects_bad_provenance_or_failed_tests() {
    let art = SkillArtifact {
        name: "x".into(),
        version: "1".into(),
        reflexes: vec![],
        prompts: vec![],
        fixtures: vec![],
        capabilities: BTreeSet::new(),
        signature: "bad".into(),
        signer: "alice".into(),
    };
    let trusted = BTreeSet::from(["alice".into()]);
    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::BadProvenance
    );
    let good = SkillArtifact {
        signature: skill_sign("alice", "x", "1"),
        ..art
    };
    assert_eq!(
        import_skill(&good, &trusted, false),
        ImportVerdict::TestsFailed
    );
    let untrusted = BTreeSet::from(["bob".into()]);
    assert_eq!(
        import_skill(&good, &untrusted, true),
        ImportVerdict::BadProvenance
    );
}

#[test]
fn vc_201_085_hmac_key_signing_and_verification_bind_full_artifact_payload() {
    use crate::skill_artifacts::import_skill_with_key;

    let mut art = SkillArtifact {
        name: "skill-hmac".into(),
        version: "1.0.0".into(),
        reflexes: vec!["r1".into()],
        prompts: vec!["p1".into()],
        fixtures: vec!["{\"ok\":true}".into()],
        capabilities: BTreeSet::from(["exec".into()]),
        signature: String::new(),
        signer: "alice".into(),
    };
    let key = b"hmac-signing-key-2026";
    art.sign_with_key(key);
    assert!(art.verify_signature_with_key(Some(key)));
    assert!(!art.verify_signature_with_key(Some(b"wrong-key")));
    assert!(!art.verify_signature_with_key(None));

    let trusted = BTreeSet::from(["alice".into()]);
    assert_eq!(
        import_skill_with_key(&art, &trusted, true, Some(key)),
        ImportVerdict::Executable
    );

    art.version = "2.0.0".into();
    assert!(!art.verify_signature_with_key(Some(key)));
    assert_eq!(
        import_skill_with_key(&art, &trusted, true, Some(key)),
        ImportVerdict::BadProvenance
    );
}

#[test]
fn vc_201_085_run_fixtures_rejects_broken_or_unparseable_json_fixtures() {
    let base = SkillArtifact {
        name: "s".into(),
        version: "1.0.0".into(),
        reflexes: vec![],
        prompts: vec![],
        fixtures: vec!["broken_fixture".into()],
        capabilities: BTreeSet::new(),
        signature: skill_sign("alice", "s", "1.0.0"),
        signer: "alice".into(),
    };
    assert!(!base.run_fixtures());

    let invalid_json = SkillArtifact {
        fixtures: vec!["{not-valid-json".into()],
        ..base.clone()
    };
    assert!(!invalid_json.run_fixtures());

    let valid_fixtures = SkillArtifact {
        fixtures: vec!["{\"step\":1}".into(), "[1,2,3]".into()],
        ..base
    };
    assert!(valid_fixtures.run_fixtures());
}
