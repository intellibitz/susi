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
