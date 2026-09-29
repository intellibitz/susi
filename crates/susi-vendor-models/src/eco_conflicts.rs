//! Confidence model and conflict resolution between sources.
//!
//! Two knowledge bases — say the bundled layer and a live-probe layer — can
//! disagree about the same fact. This module extracts every fact as a
//! [`Claim`] (`subject / predicate / value` plus provenance), then
//! [`conflicts`] reports every claim where the two bases differ. Both sides
//! are kept, ranked by confidence then recency, and the disagreement is
//! exposed — never silently resolved.
use crate::eco_schema::{date_to_days, Confidence, Entity, KnowledgeBase, Provenance};
use serde::Serialize;
use std::collections::BTreeMap;

/// One atomic assertion extracted from a knowledge base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// Entity id for attribute claims, `from` id for relation claims.
    pub subject: String,
    /// Field name (`name`, `category`, `transports`, …) or relation kind.
    pub predicate: String,
    pub value: String,
    pub provenance: Provenance,
}

fn push(out: &mut Vec<Claim>, subject: &str, predicate: &str, value: String, prov: &Provenance) {
    out.push(Claim {
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        value,
        provenance: prov.clone(),
    });
}

/// Flatten a knowledge base into claims — every entity attribute and every
/// relation becomes one auditable fact.
#[must_use]
pub fn claims_of(kb: &KnowledgeBase) -> Vec<Claim> {
    let mut out = Vec::new();
    for e in &kb.entities {
        let (id, prov) = (e.id(), e.provenance());
        push(&mut out, id, "kind", e.kind().label().to_string(), prov);
        push(&mut out, id, "name", e.name().to_string(), prov);
        match e {
            Entity::Component(c) => {
                push(
                    &mut out,
                    id,
                    "category",
                    serde_json::to_string(&c.category)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string(),
                    prov,
                );
            }
            Entity::Vendor(v) => push(&mut out, id, "home", v.home.clone(), prov),
            Entity::Standard(s) => push(&mut out, id, "body", s.body.clone(), prov),
            Entity::Protocol(p) => push(
                &mut out,
                id,
                "transports",
                p.transports
                    .iter()
                    .map(|t| {
                        serde_json::to_string(t)
                            .unwrap_or_default()
                            .trim_matches('"')
                            .to_string()
                    })
                    .collect::<Vec<_>>()
                    .join(","),
                prov,
            ),
            Entity::SpecVersion(v) => {
                push(&mut out, id, "version", v.version.clone(), prov);
                if let Some(r) = &v.released {
                    push(&mut out, id, "released", r.clone(), prov);
                }
            }
            Entity::Capability(c) => {
                push(
                    &mut out,
                    id,
                    "class",
                    serde_json::to_string(&c.class)
                        .unwrap_or_default()
                        .trim_matches('"')
                        .to_string(),
                    prov,
                );
            }
        }
    }
    for r in &kb.relations {
        push(
            &mut out,
            &r.from,
            r.kind.label(),
            r.to.clone(),
            &r.provenance,
        );
    }
    out
}

/// One side of a disagreement, with its ranking material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Side {
    pub value: String,
    pub source: String,
    pub confidence: Confidence,
    pub retrieved: String,
}

/// A claim where two bases disagree. `sides` is sorted best-first by
/// (confidence, recency); `winner` is `sides[0].value` — advisory only,
/// the disagreement itself is the deliverable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub subject: String,
    pub predicate: String,
    pub sides: Vec<Side>,
    /// Top-ranked value after confidence + recency ordering.
    pub winner: String,
}

/// Confidence ordering: verified > reported > inferred > speculative.
fn confidence_rank(c: Confidence) -> u8 {
    match c {
        Confidence::Verified => 3,
        Confidence::Reported => 2,
        Confidence::Inferred => 1,
        Confidence::Speculative => 0,
    }
}

fn side_of(c: &Claim) -> (i64, Side) {
    (
        confidence_rank(c.provenance.confidence) as i64,
        Side {
            value: c.value.clone(),
            source: c.provenance.source.clone(),
            confidence: c.provenance.confidence,
            retrieved: c.provenance.retrieved.clone(),
        },
    )
}

/// Compare two bases: every (subject, predicate) whose value differs becomes
/// a [`Conflict`] carrying both sides ranked by confidence, then recency.
/// Facts present in only one base are not conflicts — they are coverage.
#[must_use]
pub fn conflicts(a: &KnowledgeBase, b: &KnowledgeBase) -> Vec<Conflict> {
    let mut by_key: BTreeMap<(String, String), Vec<Claim>> = BTreeMap::new();
    for c in claims_of(a).into_iter().chain(claims_of(b)) {
        by_key
            .entry((c.subject.clone(), c.predicate.clone()))
            .or_default()
            .push(c);
    }
    let mut out = Vec::new();
    for ((subject, predicate), claims) in by_key {
        let mut distinct: Vec<Claim> = Vec::new();
        for c in claims {
            if !distinct.iter().any(|d| d.value == c.value) {
                distinct.push(c);
            }
        }
        if distinct.len() < 2 {
            continue;
        }
        let mut ranked: Vec<(i64, i64, Side)> = distinct
            .iter()
            .map(|c| {
                let (rank, side) = side_of(c);
                (
                    rank,
                    date_to_days(&c.provenance.retrieved).unwrap_or(0),
                    side,
                )
            })
            .collect();
        // Confidence first, then recency — both descending.
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
        let sides: Vec<Side> = ranked.into_iter().map(|(_, _, s)| s).collect();
        out.push(Conflict {
            subject,
            predicate,
            winner: sides[0].value.clone(),
            sides,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{Component, ComponentCategory, RelationKind, SCHEMA_VERSION};

    fn prov(source: &str, confidence: Confidence, retrieved: &str) -> Provenance {
        Provenance {
            source: source.into(),
            spec_version: "1.0".into(),
            retrieved: retrieved.into(),
            confidence,
        }
    }

    fn engine(name: &str, category: ComponentCategory, prov: Provenance) -> KnowledgeBase {
        KnowledgeBase {
            version: SCHEMA_VERSION.into(),
            entities: vec![Entity::Component(Component {
                id: "engine".into(),
                name: name.into(),
                category,
                provenance: prov,
            })],
            relations: vec![],
        }
    }

    #[test]
    fn identical_bases_have_no_conflicts() {
        let a = engine(
            "Engine",
            ComponentCategory::InferenceEngine,
            prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
        );
        assert!(conflicts(&a, &a).is_empty());
    }

    #[test]
    fn disagreements_keep_both_sides_ranked() {
        let docs = engine(
            "Engine",
            ComponentCategory::InferenceEngine,
            prov(
                "https://docs.example.test",
                Confidence::Verified,
                "2026-06-01",
            ),
        );
        let probe = engine(
            "Engine",
            ComponentCategory::Service,
            prov("probe://local", Confidence::Reported, "2026-09-01"),
        );
        let cs = conflicts(&docs, &probe);
        let c = cs
            .iter()
            .find(|c| c.predicate == "category")
            .expect("category conflict");
        assert_eq!(c.sides.len(), 2);
        // Verified beats reported even though reported is newer.
        assert_eq!(c.winner, "inference-engine");
        assert_eq!(c.sides[0].confidence, Confidence::Verified);
        assert_eq!(c.sides[1].confidence, Confidence::Reported);
    }

    #[test]
    fn recency_breaks_ties() {
        let old = engine(
            "Engine",
            ComponentCategory::Service,
            prov("https://a.b/old", Confidence::Verified, "2026-01-01"),
        );
        let new = engine(
            "Engine",
            ComponentCategory::CliTool,
            prov("https://a.b/new", Confidence::Verified, "2026-09-01"),
        );
        let c = conflicts(&old, &new)
            .into_iter()
            .find(|c| c.predicate == "category")
            .unwrap();
        assert_eq!(c.winner, "cli-tool");
    }

    #[test]
    fn one_sided_facts_are_coverage_not_conflict() {
        let a = engine(
            "Engine",
            ComponentCategory::InferenceEngine,
            prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
        );
        let mut b = engine(
            "Engine",
            ComponentCategory::InferenceEngine,
            prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
        );
        b.entities
            .push(Entity::Capability(crate::eco_schema::Capability {
                id: "cap-x".into(),
                name: "X".into(),
                class: crate::eco_schema::CapabilityClass::Audio,
                provenance: prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
            }));
        assert!(conflicts(&a, &b).is_empty());
    }

    #[test]
    fn relation_disagreements_conflict_too() {
        let rel = |from: &str, to: &str| crate::eco_schema::Relation {
            kind: RelationKind::HasCapability,
            from: from.into(),
            to: to.into(),
            provenance: prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
        };
        let mut a = engine(
            "Engine",
            ComponentCategory::InferenceEngine,
            prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
        );
        a.relations.push(rel("engine", "cap-a"));
        let mut b = engine(
            "Engine",
            ComponentCategory::InferenceEngine,
            prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
        );
        b.relations.push(rel("engine", "cap-b"));
        let c = conflicts(&a, &b)
            .into_iter()
            .find(|c| c.predicate == "has-capability")
            .unwrap();
        assert_eq!(c.sides.len(), 2);
    }

    #[test]
    fn claims_cover_entity_fields_and_relations() {
        let kb = engine(
            "Engine",
            ComponentCategory::InferenceEngine,
            prov("https://a.b/s", Confidence::Verified, "2026-09-01"),
        );
        let cs = claims_of(&kb);
        assert!(cs
            .iter()
            .any(|c| c.predicate == "kind" && c.value == "component"));
        assert!(cs
            .iter()
            .any(|c| c.predicate == "name" && c.value == "Engine"));
        assert!(cs.iter().any(|c| c.predicate == "category"));
    }
}
