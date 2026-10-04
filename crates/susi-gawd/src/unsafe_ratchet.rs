//! Ratchet dependency unsafe exposure and provenance (VC-201-097).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsafeExposure {
    pub crate_name: String,
    pub unsafe_fns: u32,
    pub first_party_exception: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SbomEntry {
    pub crate_name: String,
    pub version: String,
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeigerVerdict {
    Clean,
    NeedsReview { new_crates: Vec<String> },
}

/// The pinned baseline file: the feature set it measures, the per-crate
/// unsafe exposure, and the SBOM (crate, version, provenance) it was
/// recorded against. A human refreshes it via `unsafe_ratchet
/// --write-baseline` and commits the diff — that commit is the auditable
/// review path for new exposure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsafeBaseline {
    /// Feature set the measurement pins (e.g. "default", "cuda").
    #[serde(default = "default_features")]
    pub features: String,
    #[serde(default)]
    pub exposure: Vec<UnsafeExposure>,
    #[serde(default)]
    pub sbom: Vec<SbomEntry>,
    #[serde(default)]
    pub recorded_unix: u64,
}

fn default_features() -> String {
    "default".to_string()
}

/// Transitive change observed between baseline and current SBOM: a crate
/// whose version moved, appeared, or disappeared. Recorded in the gate
/// output — version drift alone does not flag, new unsafe exposure does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SbomChange {
    pub crate_name: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Diff two SBOMs: every crate whose (name, version) pair changed.
#[must_use]
pub fn sbom_changes(baseline: &[SbomEntry], current: &[SbomEntry]) -> Vec<SbomChange> {
    use std::collections::BTreeMap;
    let base: BTreeMap<&str, &str> = baseline
        .iter()
        .map(|s| (s.crate_name.as_str(), s.version.as_str()))
        .collect();
    let cur: BTreeMap<&str, &str> = current
        .iter()
        .map(|s| (s.crate_name.as_str(), s.version.as_str()))
        .collect();
    let mut changes = Vec::new();
    for (name, to) in &cur {
        match base.get(name) {
            Some(from) if *from != *to => changes.push(SbomChange {
                crate_name: (*name).to_string(),
                from: Some((*from).to_string()),
                to: Some((*to).to_string()),
            }),
            None => changes.push(SbomChange {
                crate_name: (*name).to_string(),
                from: None,
                to: Some((*to).to_string()),
            }),
            _ => {}
        }
    }
    for (name, from) in &base {
        if !cur.contains_key(name) {
            changes.push(SbomChange {
                crate_name: (*name).to_string(),
                from: Some((*from).to_string()),
                to: None,
            });
        }
    }
    changes
}

/// Compare current geiger+SBOM against a pinned baseline; new exposure needs review.
#[must_use]
pub fn ratchet_unsafe(
    baseline: &[UnsafeExposure],
    current: &[UnsafeExposure],
    baseline_sbom: &[SbomEntry],
    current_sbom: &[SbomEntry],
) -> GeigerVerdict {
    let base_names: BTreeSet<_> = baseline
        .iter()
        .filter(|e| !e.first_party_exception)
        .map(|e| (e.crate_name.as_str(), e.unsafe_fns))
        .collect();
    let mut new_crates = Vec::new();
    for e in current {
        if e.first_party_exception {
            continue;
        }
        match baseline.iter().find(|b| b.crate_name == e.crate_name) {
            Some(b) if e.unsafe_fns > b.unsafe_fns => new_crates.push(e.crate_name.clone()),
            None => new_crates.push(e.crate_name.clone()),
            _ => {}
        }
        let _ = base_names;
    }
    let base_sbom: BTreeSet<_> = baseline_sbom
        .iter()
        .map(|s| (s.crate_name.as_str(), s.version.as_str()))
        .collect();
    for s in current_sbom {
        if !base_sbom.contains(&(s.crate_name.as_str(), s.version.as_str()))
            && !new_crates.contains(&s.crate_name)
        {
            // Transitive version change alone is recorded but only flags
            // when paired with new unsafe — already handled above.
        }
    }
    if new_crates.is_empty() {
        GeigerVerdict::Clean
    } else {
        GeigerVerdict::NeedsReview { new_crates }
    }
}
