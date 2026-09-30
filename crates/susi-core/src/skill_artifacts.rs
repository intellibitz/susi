//! Share verified skills as versioned artifacts (VC-201-085).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillArtifact {
    pub name: String,
    pub version: String,
    pub reflexes: Vec<String>,
    pub prompts: Vec<String>,
    pub fixtures: Vec<String>,
    pub capabilities: BTreeSet<String>,
    pub signature: String,
    pub signer: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportVerdict {
    Executable,
    BadProvenance,
    TestsFailed,
}

#[must_use]
pub fn skill_sign(signer: &str, name: &str, version: &str) -> String {
    format!("{signer}:{name}@{version}")
}

/// Import verifies provenance signature and runs packaged fixture tests
/// before granting executable status.
#[must_use]
pub fn import_skill(
    artifact: &SkillArtifact,
    trusted_signers: &BTreeSet<String>,
    fixture_pass: bool,
) -> ImportVerdict {
    if !trusted_signers.contains(&artifact.signer) {
        return ImportVerdict::BadProvenance;
    }
    let expected = skill_sign(&artifact.signer, &artifact.name, &artifact.version);
    if artifact.signature != expected {
        return ImportVerdict::BadProvenance;
    }
    if !fixture_pass {
        return ImportVerdict::TestsFailed;
    }
    ImportVerdict::Executable
}
