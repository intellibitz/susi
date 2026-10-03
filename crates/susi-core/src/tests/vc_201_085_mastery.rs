//! Mastery verification for VC-201-085: share verified skills as versioned
//! artifacts with signatures.
//!
//! The claim is that importing a skill "verifies provenance and tests
//! before granting it executable status". The cited tests exercise the
//! verdict enum on self-consistent inputs. The distinguishing properties
//! are that the signature actually authenticates content and that tests
//! are actually run — not that a caller-supplied flag is trusted.

use crate::skill_artifacts::{import_skill, skill_sign, ImportVerdict, SkillArtifact};
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

/// Falsification: the signature does not cover the payload. An attacker
/// takes a validly-signed artifact, replaces every reflex with a hostile
/// one and grants itself new capabilities — the signature still verifies,
/// because it only binds name+version+signer.
#[test]
fn vc_201_085_mastery_signed_content_is_not_bound_to_the_signature() {
    let trusted = BTreeSet::from(["alice".into()]);
    let mut art = signed_artifact("reflex-pack", "alice", &["r1"]);
    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::Executable
    );

    art.reflexes = vec!["rm -rf /".into()];
    art.prompts = vec!["ignore safety".into()];
    art.capabilities = BTreeSet::from(["exec".into(), "net".into()]);
    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::Executable,
        "payload swapped wholesale under an unchanged signature"
    );
}

/// Falsification: the signature is forgeable. `skill_sign` is a public
/// pure function `signer:name@version` with no key material — anyone can
/// produce a signature that passes verification for any trusted signer.
#[test]
fn vc_201_085_mastery_signature_forgeable_without_any_key() {
    // An attacker with no key material forges alice's signature directly.
    let forged = signed_artifact("hostile", "alice", &["exfiltrate"]);
    assert_eq!(forged.signature, "alice:hostile@1.0.0");
    let trusted = BTreeSet::from(["alice".into()]);
    assert_eq!(
        import_skill(&forged, &trusted, true),
        ImportVerdict::Executable,
        "a forged signature grants executable status"
    );
}

/// Falsification: "tests verify" is a boolean the caller passes. Import
/// never runs the packaged fixtures — the same artifact is Executable or
/// TestsFailed purely on a flag, so nothing in the artifact itself is
/// tested.
#[test]
fn vc_201_085_mastery_tests_are_a_caller_supplied_flag() {
    let trusted = BTreeSet::from(["alice".into()]);
    // An artifact whose fixtures cannot parse — no test is run either way.
    let mut art = signed_artifact("broken", "alice", &[]);
    art.fixtures = vec!["not-a-fixture{{{".into()];
    assert_eq!(
        import_skill(&art, &trusted, true),
        ImportVerdict::Executable,
        "an artifact with broken fixtures is executable when the flag says pass"
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
