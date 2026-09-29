//! Coverage of what susi actually meets on this host.
//!
//! The host scan (`crate::local_ecosystem`) and other detectors see real
//! engines, endpoints, credential-bearing vendors and agents. This module
//! reports which of those the knowledge base does not describe yet —
//! ranked by how often each appears — so the gap list is a work queue, not
//! a shrug.
use crate::eco_schema::{Entity, KnowledgeBase};
use serde::Serialize;
use std::collections::BTreeMap;

/// What kind of thing was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ObservedKind {
    /// Local inference engine / runtime (ollama, vllm, …).
    Engine,
    /// A live OpenAI-compatible or vendor endpoint.
    Endpoint,
    /// A vendor whose credential env var is set.
    Vendor,
    /// An agent CLI or framework on the host.
    Agent,
}

/// One observed thing: what it is, a stable id, a display name, and how many
/// times it was seen (higher = more worth documenting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub kind: ObservedKind,
    pub id: String,
    pub name: String,
    pub count: usize,
}

/// A gap: observed on the host, undocumented in the knowledge base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Gap {
    pub kind: ObservedKind,
    pub id: String,
    pub name: String,
    pub count: usize,
}

/// Turn a host [`crate::local_ecosystem::Ecosystem`] scan into observations
/// (installed or running engines count once).
#[must_use]
pub fn observe_engines(eco: &crate::local_ecosystem::Ecosystem) -> Vec<Observation> {
    eco.engines
        .iter()
        .filter(|e| e.installed || e.running)
        .map(|e| Observation {
            kind: ObservedKind::Engine,
            id: e.id.clone(),
            name: e.name.clone(),
            count: usize::from(e.installed) + usize::from(e.running),
        })
        .collect()
}

/// Vendor env-var detections — each vendor name seen, counted.
#[must_use]
pub fn observe_vendors(ids: &[&str]) -> Vec<Observation> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for id in ids {
        *counts.entry(id).or_default() += 1;
    }
    counts
        .into_iter()
        .map(|(id, count)| Observation {
            kind: ObservedKind::Vendor,
            id: id.to_string(),
            name: id.to_string(),
            count,
        })
        .collect()
}

/// Report observations with no matching knowledge-base entity, ranked by
/// count descending then id. "Matching" means the observation id equals an
/// entity id or case-folded entity name.
#[must_use]
pub fn gaps(kb: &KnowledgeBase, observations: &[Observation]) -> Vec<Gap> {
    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &kb.entities {
        known.insert(e.id().to_string());
        known.insert(e.name().to_ascii_lowercase());
        if let Entity::Component(_) | Entity::Vendor(_) = e {
            // slug form of the display name also counts ("Ollama" -> "ollama")
            known.insert(e.name().to_ascii_lowercase().replace(' ', "-"));
        }
    }
    let mut out: Vec<Gap> = observations
        .iter()
        .filter(|o| !known.contains(&o.id) && !known.contains(&o.id.to_ascii_lowercase()))
        .map(|o| Gap {
            kind: o.kind,
            id: o.id.clone(),
            name: o.name.clone(),
            count: o.count,
        })
        .collect();
    out.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.id.cmp(&b.id)));
    out
}

/// Fraction of observations the base documents (1.0 = full coverage).
/// `None` when there is nothing observed to cover.
#[must_use]
pub fn coverage_ratio(kb: &KnowledgeBase, observations: &[Observation]) -> Option<f64> {
    if observations.is_empty() {
        return None;
    }
    let missing = gaps(kb, observations).len();
    Some(1.0 - missing as f64 / observations.len() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{
        Component, ComponentCategory, Confidence, EntityKind, Provenance, SCHEMA_VERSION,
    };

    fn prov() -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        }
    }

    fn kb_with_ollama() -> KnowledgeBase {
        KnowledgeBase {
            version: SCHEMA_VERSION.into(),
            entities: vec![Entity::Component(Component {
                id: "ollama".into(),
                name: "Ollama".into(),
                category: ComponentCategory::InferenceEngine,
                provenance: prov(),
            })],
            relations: vec![],
        }
    }

    fn obs(kind: ObservedKind, id: &str, count: usize) -> Observation {
        Observation {
            kind,
            id: id.into(),
            name: id.into(),
            count,
        }
    }

    #[test]
    fn documented_things_are_not_gaps() {
        let kb = kb_with_ollama();
        let obs = vec![obs(ObservedKind::Engine, "ollama", 2)];
        assert!(gaps(&kb, &obs).is_empty());
        assert_eq!(coverage_ratio(&kb, &obs), Some(1.0));
    }

    #[test]
    fn undetected_or_undocumented_items_rank_by_count() {
        let kb = kb_with_ollama();
        let obs = vec![
            obs(ObservedKind::Engine, "vllm", 1),
            obs(ObservedKind::Engine, "ollama", 2),
            obs(ObservedKind::Vendor, "anthropic", 5),
            obs(ObservedKind::Endpoint, "sglang", 3),
        ];
        let g = gaps(&kb, &obs);
        assert_eq!(
            g.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(),
            ["anthropic", "sglang", "vllm"]
        );
        assert_eq!(coverage_ratio(&kb, &obs), Some(0.25));
    }

    #[test]
    fn name_aliases_count_as_documented() {
        // Entity id "ollama" documented under display name matching an
        // observed id spelled differently.
        let kb = kb_with_ollama();
        assert!(gaps(&kb, &[obs(ObservedKind::Engine, "Ollama", 1)]).is_empty());
    }

    #[test]
    fn observe_engines_counts_installed_and_running() {
        let eco = crate::local_ecosystem::Ecosystem {
            engines: vec![
                crate::local_ecosystem::Detected {
                    id: "ollama".into(),
                    name: "Ollama".into(),
                    installed: true,
                    running: true,
                    binary: None,
                    version: None,
                    dirs: vec![],
                    endpoint: None,
                    startable: false,
                },
                crate::local_ecosystem::Detected {
                    id: "vllm".into(),
                    name: "vLLM".into(),
                    installed: false,
                    running: false,
                    binary: None,
                    version: None,
                    dirs: vec![],
                    endpoint: None,
                    startable: false,
                },
            ],
            accelerators: vec![],
        };
        let obs = observe_engines(&eco);
        assert_eq!(obs.len(), 1);
        assert_eq!(obs[0].id, "ollama");
        assert_eq!(obs[0].count, 2); // installed + running
    }

    #[test]
    fn observe_vendors_counts_duplicates() {
        let obs = observe_vendors(&["openai", "anthropic", "openai"]);
        assert_eq!(obs.len(), 2);
        assert_eq!(
            obs.iter().find(|o| o.id == "openai").map(|o| o.count),
            Some(2)
        );
    }

    #[test]
    fn empty_observation_set_is_full_coverage() {
        let kb = kb_with_ollama();
        assert_eq!(coverage_ratio(&kb, &[]), None);
        assert!(gaps(&kb, &[]).is_empty());
    }

    #[test]
    fn entity_kind_reference() {
        let kb = kb_with_ollama();
        assert_eq!(kb.entities[0].kind(), EntityKind::Component);
    }
}
