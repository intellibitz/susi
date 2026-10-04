//! Mastery verification for VC-201-085: share verified skills as versioned
//! artifacts with signatures.
//!
//! The claim is that importing a skill "verifies provenance and tests
//! before granting it executable status". The cited tests exercise the
//! verdict enum on self-consistent inputs. The distinguishing properties
//! are that the signature actually authenticates content and that tests
//! are actually run — not that a caller-supplied flag is trusted.

use crate::skill_artifacts::{
    import_skill, import_skill_with_key, skill_sign, ImportVerdict, SkillArtifact,
};
use std::collections::BTreeSet;

fn signed_artifact(name: &str, signer: &str, reflexes: &[&str]) -> SkillArtifact {
    SkillArtifact {
        name: name.into(),
        version: "1.0.0".into(),
        reflexes: reflexes.iter().map(|s| s.to_string()).collect(),
        prompts: vec![],
        fixtures: vec![],
        capabilities: BTreeSet::new(),
        signature: skill_sign(signer, name, "1.0.0"),
        signer: signer.into(),
    }
}

/// Mastery: cryptographic signature binds content so payload tampering is rejected.
#[test]
fn vc_201_085_mastery_signed_content_is_bound_to_the_signature() {
    let trusted = BTreeSet::from(["alice".into()]);
    let key = b"alice-secret-key-32-bytes-long!!";
    let mut art = signed_artifact("reflex-pack", "alice", &["r1"]);
    art.sign_with_key(key);

    assert_eq!(
        import_skill_with_key(&art, &trusted, true, Some(key)),
        ImportVerdict::Executable
    );

    // Tampering with reflexes breaks the HMAC signature
    art.reflexes = vec!["rm -rf /".into()];
    assert_eq!(
        import_skill_with_key(&art, &trusted, true, Some(key)),
        ImportVerdict::BadProvenance,
        "payload swapped wholesale must fail verification"
    );
}

/// Mastery: signature cannot be forged without key material.
#[test]
fn vc_201_085_mastery_signature_requires_valid_key() {
    let trusted = BTreeSet::from(["alice".into()]);
    let alice_key = b"alice-secret-key-32-bytes-long!!";
    let attacker_key = b"attacker-key-cannot-forge-alice!";

    let mut forged = signed_artifact("hostile", "alice", &["exfiltrate"]);
    forged.sign_with_key(attacker_key);

    assert_eq!(
        import_skill_with_key(&forged, &trusted, true, Some(alice_key)),
        ImportVerdict::BadProvenance,
        "a signature created without the authentic key must be rejected"
    );
}

/// Mastery: packaged fixtures are executed and unparseable/broken fixtures fail tests.
#[test]
fn vc_201_085_mastery_fixtures_are_executed_and_validated() {
    let trusted = BTreeSet::from(["alice".into()]);
    let mut art = signed_artifact("broken", "alice", &[]);
    art.fixtures = vec!["not-a-fixture{{{".into()];

    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::TestsFailed,
        "an artifact with broken fixtures must fail tests even if caller passed true"
    );

    art.fixtures = vec!["{\"test\": \"ok\"}".into()];
    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::Executable,
        "valid fixtures pass"
    );
}

/// What does hold: an untrusted signer is rejected, and a mismatched
/// signature string is rejected.
#[test]
fn vc_201_085_mastery_trusted_signer_and_string_match_hold() {
    let art = signed_artifact("x", "alice", &["r1"]);
    let trusted = BTreeSet::from(["alice".into()]);
    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::Executable
    );
    let untrusted = BTreeSet::from(["mallory".into()]);
    assert_eq!(
        import_skill(&art, &untrusted, true),
        ImportVerdict::BadProvenance
    );
    let mut tampered = art;
    tampered.signature = "different-string".into();
    assert_eq!(
        import_skill(&tampered, &trusted, true),
        ImportVerdict::BadProvenance
    );
}
