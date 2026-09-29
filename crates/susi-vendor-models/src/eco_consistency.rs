//! Knowledge-base consistency checks — the suite a reviewer (or CI) runs over
//! a knowledge base before trusting it.
//!
//! [`check`] composes [`crate::eco_schema::validate`] with structural rules
//! about completeness: every standard/protocol has a spec link (an inbound
//! `version-of`), and no entity is an orphan (untouched by any relation —
//! a fact about nothing relates to nothing).
use crate::eco_schema::{validate, Entity, EntityKind, Issue, KnowledgeBase, RelationKind};
use std::collections::{HashMap, HashSet};

/// Full consistency pass: schema validation plus completeness rules.
/// Empty means consistent.
#[must_use]
pub fn check(kb: &KnowledgeBase) -> Vec<Issue> {
    let mut issues = validate(kb);

    // Referenced counts per entity id, and which specs have versions.
    let mut inbound: HashMap<&str, usize> = HashMap::new();
    let mut outbound: HashMap<&str, usize> = HashMap::new();
    let mut versioned: HashSet<&str> = HashSet::new();
    for r in &kb.relations {
        *inbound.entry(r.to.as_str()).or_default() += 1;
        *outbound.entry(r.from.as_str()).or_default() += 1;
        if r.kind == RelationKind::VersionOf {
            versioned.insert(r.to.as_str());
        }
    }

    for e in &kb.entities {
        let touched =
            inbound.get(e.id()).copied().unwrap_or(0) + outbound.get(e.id()).copied().unwrap_or(0);
        match e.kind() {
            EntityKind::Standard | EntityKind::Protocol => {
                if !versioned.contains(e.id()) {
                    issues.push(Issue {
                        stage: crate::eco_schema::Stage::Model,
                        path: format!("/entities/{}", e.id()),
                        message: format!(
                            "{} {:?} has no spec-version link (no version-of edge)",
                            e.kind().label(),
                            e.id()
                        ),
                    });
                }
            }
            EntityKind::Capability => {
                if let Entity::Capability(c) = e {
                    if let Some(expected) = crate::eco_taxonomy::class_of(&c.id) {
                        if c.class != expected {
                            issues.push(Issue {
                                stage: crate::eco_schema::Stage::Model,
                                path: format!("/entities/{}/class", e.id()),
                                message: format!(
                                    "capability {:?} has class {:?} but the taxonomy declares it for a different class",
                                    e.id(),
                                    c.class
                                ),
                            });
                        }
                    }
                }
            }
            EntityKind::Component | EntityKind::Vendor | EntityKind::SpecVersion => {}
        }
        if touched == 0 {
            issues.push(Issue {
                stage: crate::eco_schema::Stage::Model,
                path: format!("/entities/{}", e.id()),
                message: format!(
                    "orphan entity {:?} ({}): no relation mentions it",
                    e.id(),
                    e.kind().label()
                ),
            });
        }
    }
    issues
}

/// Just the completeness rules, for callers that already ran
/// [`crate::eco_schema::validate`] themselves.
#[must_use]
pub fn orphans(kb: &KnowledgeBase) -> Vec<&str> {
    let mut touched: HashSet<&str> = HashSet::new();
    for r in &kb.relations {
        touched.insert(r.from.as_str());
        touched.insert(r.to.as_str());
    }
    kb.entities
        .iter()
        .filter(|e| !touched.contains(e.id()))
        .map(|e| e.id())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{
        Capability, CapabilityClass, Component, ComponentCategory, Confidence, Entity, Protocol,
        Provenance, Relation, SpecVersion, Transport, Vendor, SCHEMA_VERSION,
    };

    fn prov() -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        }
    }

    fn rel(kind: RelationKind, from: &str, to: &str) -> Relation {
        Relation {
            kind,
            from: from.into(),
            to: to.into(),
            provenance: prov(),
        }
    }

    /// Consistent base: vendor provides engine; protocol has a version;
    /// engine implements it.
    fn kb() -> KnowledgeBase {
        KnowledgeBase {
            version: SCHEMA_VERSION.into(),
            entities: vec![
                Entity::Vendor(Vendor {
                    id: "acme".into(),
                    name: "Acme".into(),
                    home: "https://acme.example.test".into(),
                    provenance: prov(),
                }),
                Entity::Component(Component {
                    id: "engine".into(),
                    name: "Engine".into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov(),
                }),
                Entity::Protocol(Protocol {
                    id: "proto".into(),
                    name: "Proto".into(),
                    transports: vec![Transport::Http],
                    provenance: prov(),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "proto-v1".into(),
                    name: "v1".into(),
                    version: "1".into(),
                    released: None,
                    provenance: prov(),
                }),
            ],
            relations: vec![
                rel(RelationKind::Provides, "acme", "engine"),
                rel(RelationKind::VersionOf, "proto-v1", "proto"),
                rel(RelationKind::ImplementsVersion, "engine", "proto-v1"),
            ],
        }
    }

    #[test]
    fn consistent_base_passes() {
        assert!(check(&kb()).is_empty(), "{:?}", check(&kb()));
    }

    #[test]
    fn protocol_without_spec_version_is_flagged() {
        let mut kb = kb();
        kb.relations
            .retain(|r| !(r.kind == RelationKind::VersionOf && r.to == "proto"));
        kb.entities.retain(|e| e.id() != "proto-v1");
        let issues = check(&kb);
        assert!(issues
            .iter()
            .any(|i| i.message.contains("no spec-version link") && i.message.contains("proto")));
    }

    #[test]
    fn orphan_entities_are_flagged() {
        let mut kb = kb();
        kb.entities.push(Entity::Capability(Capability {
            id: "cap-lonely".into(),
            name: "Lonely".into(),
            class: CapabilityClass::Audio,
            provenance: prov(),
        }));
        let issues = check(&kb);
        assert!(issues
            .iter()
            .any(|i| i.message.contains("orphan entity") && i.message.contains("cap-lonely")));
        assert_eq!(orphans(&kb), ["cap-lonely"]);
    }

    #[test]
    fn dangling_refs_and_dup_ids_surface_from_the_schema_layer() {
        let mut dangling = kb();
        dangling
            .relations
            .push(rel(RelationKind::DependsOn, "engine", "ghost"));
        assert!(check(&dangling).iter().any(|i| i.path.ends_with("/to")));

        let mut dup = kb();
        dup.entities.push(dup.entities[0].clone());
        assert!(check(&dup)
            .iter()
            .any(|i| i.message.contains("duplicate entity id")));
    }

    #[test]
    fn spec_version_without_version_of_is_flagged_by_schema() {
        let mut kb = kb();
        kb.relations.retain(|r| r.kind != RelationKind::VersionOf);
        // proto-v1 remains but loses its version-of edge.
        assert!(check(&kb)
            .iter()
            .any(|i| i.message.contains("exactly one version-of")));
    }
}
