//! Compatibility matrix: for every `component × subject × spec-version`
//! triple, compute `supported / partial / unsupported` and keep the evidence
//! that justifies the cell.
//!
//! Semantics:
//! - **Supported**: the component pins the exact spec-version via
//!   `implements-version`, or — when the subject has no recorded versions —
//!   declares `implements` on the subject.
//! - **Partial**: the component declares `implements` on the subject but
//!   pins a *different* spec-version, or pins none — untested at this
//!   version.
//! - **Unsupported**: neither edge exists.
//!
//! Every cell lists its evidence: the relations that produced the verdict,
//! each with its provenance and freshness band (via
//! [`crate::eco_provenance`]), so a stale or speculative claim is visible
//! rather than averaged away.
use crate::eco_provenance::{freshness, Freshness, Policy};
use crate::eco_relations::Graph;
use crate::eco_schema::{Entity, EntityKind, KnowledgeBase, Relation, RelationKind};
use serde::Serialize;

/// Support verdict for one triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Support {
    /// Pins this exact spec-version (or the subject has no versions and an
    /// `implements` edge exists).
    Supported,
    /// Implements the subject but not pinned to this version.
    Partial,
    /// No implements evidence at all.
    Unsupported,
}

/// One relation that justifies a cell, with its freshness.
#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub relation: Relation,
    /// Freshness of the relation's provenance at evaluation time.
    pub freshness: FreshnessBand,
}

/// Serializable freshness band (mirrors `eco_provenance::Freshness`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FreshnessBand {
    Fresh,
    Aging,
    Stale,
    Future,
    Unreadable,
}

impl From<Freshness> for FreshnessBand {
    fn from(f: Freshness) -> Self {
        match f {
            Freshness::Fresh { .. } => Self::Fresh,
            Freshness::Aging { .. } => Self::Aging,
            Freshness::Stale { .. } => Self::Stale,
            Freshness::Future { .. } => Self::Future,
            Freshness::Unreadable => Self::Unreadable,
        }
    }
}

/// One row of the matrix.
#[derive(Debug, Clone, Serialize)]
pub struct Cell {
    pub component: String,
    /// Standard or protocol id.
    pub subject: String,
    /// The spec-version evaluated; `None` when the subject has none recorded.
    pub spec_version: Option<String>,
    pub support: Support,
    pub evidence: Vec<Evidence>,
}

/// Evaluation context: the freshness policy and the instant cells are
/// assessed at. Bundled so queries stay under the argument limit.
#[derive(Debug, Clone, Copy)]
pub struct Eval {
    pub policy: Policy,
    pub now_unix: i64,
}

fn to_evidence(r: &Relation, eval: &Eval) -> Evidence {
    Evidence {
        relation: r.clone(),
        freshness: freshness(&r.provenance, &eval.policy, eval.now_unix).into(),
    }
}

/// Evaluate one `component × subject × spec-version` cell. `spec_version`
/// must be a `version-of` the subject; when `None`, the verdict is over the
/// subject as a whole (supported iff an `implements` or any
/// `implements-version` edge exists).
#[must_use]
pub fn cell(
    kb: &KnowledgeBase,
    component: &str,
    subject: &str,
    spec_version: Option<&str>,
    eval: &Eval,
) -> Cell {
    let g = Graph::new(kb);
    let implements = kb
        .relations
        .iter()
        .find(|r| r.kind == RelationKind::Implements && r.from == component && r.to == subject);
    let pins: Vec<&Relation> = kb
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::ImplementsVersion && r.from == component)
        .filter(|r| g.subject_of(&r.to).is_some_and(|s| s.id() == subject))
        .collect();
    // The pin counts only when the named version is a version-of this
    // subject.
    let pinned_here = spec_version.is_some_and(|v| {
        kb.relations
            .iter()
            .any(|r| r.kind == RelationKind::ImplementsVersion && r.from == component && r.to == v)
            && g.subject_of(v).is_some_and(|s| s.id() == subject)
    });

    let mut evidence: Vec<Evidence> = Vec::new();
    if let Some(r) = implements {
        evidence.push(to_evidence(r, eval));
    }
    for r in &pins {
        evidence.push(to_evidence(r, eval));
    }

    let support = if pinned_here {
        Support::Supported
    } else if implements.is_some() || !pins.is_empty() {
        match spec_version {
            // Subject-level query with implements evidence.
            None => Support::Supported,
            Some(_) => Support::Partial,
        }
    } else {
        Support::Unsupported
    };
    Cell {
        component: component.to_string(),
        subject: subject.to_string(),
        spec_version: spec_version.map(str::to_string),
        support,
        evidence,
    }
}

/// The full matrix: every component × every standard/protocol × each of its
/// spec-versions (a single `None`-versioned row when none are recorded).
#[must_use]
pub fn matrix(kb: &KnowledgeBase, eval: &Eval) -> Vec<Cell> {
    let g = Graph::new(kb);
    let subjects: Vec<&Entity> = kb
        .entities
        .iter()
        .filter(|e| matches!(e.kind(), EntityKind::Standard | EntityKind::Protocol))
        .collect();
    let mut cells = Vec::new();
    for c in kb
        .entities
        .iter()
        .filter(|e| e.kind() == EntityKind::Component)
    {
        for s in &subjects {
            let versions = g.versions_of(s.id());
            if versions.is_empty() {
                cells.push(cell(kb, c.id(), s.id(), None, eval));
            } else {
                for v in versions {
                    cells.push(cell(kb, c.id(), s.id(), Some(v.id()), eval));
                }
            }
        }
    }
    cells
}

/// Rows where the verdict rests on stale or unreadable evidence — the cells
/// a refresh should re-verify first.
#[must_use]
pub fn stale_cells(cells: &[Cell]) -> Vec<&Cell> {
    cells
        .iter()
        .filter(|c| {
            c.support != Support::Unsupported
                && c.evidence.iter().all(|e| {
                    matches!(
                        e.freshness,
                        FreshnessBand::Stale | FreshnessBand::Unreadable
                    )
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{
        Component, ComponentCategory, Confidence, Protocol, Provenance, SpecVersion, Transport,
        Vendor, SCHEMA_VERSION,
    };

    const NOW: i64 = 1_790_000_000;

    fn eval() -> Eval {
        Eval {
            policy: Policy::default(),
            now_unix: NOW,
        }
    }

    fn prov_at(retrieved: &str) -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: retrieved.into(),
            confidence: Confidence::Verified,
        }
    }

    fn rel_at(kind: RelationKind, from: &str, to: &str, at: &str) -> Relation {
        Relation {
            kind,
            from: from.into(),
            to: to.into(),
            provenance: prov_at(at),
        }
    }

    fn kb() -> KnowledgeBase {
        KnowledgeBase {
            version: SCHEMA_VERSION.into(),
            entities: vec![
                Entity::Vendor(Vendor {
                    id: "acme".into(),
                    name: "Acme".into(),
                    home: "https://acme.example.test".into(),
                    provenance: prov_at("2026-09-01"),
                }),
                Entity::Component(Component {
                    id: "engine-a".into(),
                    name: "Engine A".into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov_at("2026-09-01"),
                }),
                Entity::Component(Component {
                    id: "engine-b".into(),
                    name: "Engine B".into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov_at("2026-09-01"),
                }),
                Entity::Protocol(Protocol {
                    id: "proto-x".into(),
                    name: "Proto X".into(),
                    transports: vec![Transport::Http],
                    provenance: prov_at("2026-09-01"),
                }),
                Entity::Protocol(Protocol {
                    id: "proto-bare".into(),
                    name: "Proto Bare".into(),
                    transports: vec![Transport::Http],
                    provenance: prov_at("2026-09-01"),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "proto-x-v1".into(),
                    name: "v1".into(),
                    version: "1.0".into(),
                    released: None,
                    provenance: prov_at("2026-09-01"),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "proto-x-v2".into(),
                    name: "v2".into(),
                    version: "2.0".into(),
                    released: None,
                    provenance: prov_at("2026-09-01"),
                }),
            ],
            relations: vec![
                rel_at(
                    RelationKind::VersionOf,
                    "proto-x-v1",
                    "proto-x",
                    "2026-09-01",
                ),
                rel_at(
                    RelationKind::VersionOf,
                    "proto-x-v2",
                    "proto-x",
                    "2026-09-01",
                ),
                rel_at(
                    RelationKind::Supersedes,
                    "proto-x-v2",
                    "proto-x-v1",
                    "2026-09-01",
                ),
                // engine-a pins v2 exactly.
                rel_at(
                    RelationKind::ImplementsVersion,
                    "engine-a",
                    "proto-x-v2",
                    "2026-09-01",
                ),
                // engine-b declares implements only.
                rel_at(
                    RelationKind::Implements,
                    "engine-b",
                    "proto-x",
                    "2026-09-01",
                ),
                rel_at(
                    RelationKind::Implements,
                    "engine-b",
                    "proto-bare",
                    "2026-09-01",
                ),
            ],
        }
    }

    #[test]
    fn pinned_version_is_supported() {
        let kb = kb();
        let c = cell(&kb, "engine-a", "proto-x", Some("proto-x-v2"), &eval());
        assert_eq!(c.support, Support::Supported);
        assert_eq!(c.evidence.len(), 1);
        assert_eq!(c.evidence[0].relation.kind, RelationKind::ImplementsVersion);
    }

    #[test]
    fn implements_without_pin_is_partial_at_a_version() {
        let kb = kb();
        for v in ["proto-x-v1", "proto-x-v2"] {
            let c = cell(&kb, "engine-b", "proto-x", Some(v), &eval());
            assert_eq!(c.support, Support::Partial, "{v}");
        }
        // engine-a at v1: pins v2, not v1 -> partial.
        let c = cell(&kb, "engine-a", "proto-x", Some("proto-x-v1"), &eval());
        assert_eq!(c.support, Support::Partial);
    }

    #[test]
    fn no_evidence_is_unsupported() {
        let kb = kb();
        let c = cell(&kb, "engine-a", "proto-x", Some("proto-x-v1"), &eval());
        assert_eq!(c.support, Support::Partial); // has a pin, just not this one
        let c = cell(&kb, "engine-a", "proto-bare", None, &eval());
        assert_eq!(c.support, Support::Unsupported);
        assert!(c.evidence.is_empty());
    }

    #[test]
    fn versionless_subject_uses_implements_edge() {
        let kb = kb();
        let c = cell(&kb, "engine-b", "proto-bare", None, &eval());
        assert_eq!(c.support, Support::Supported);
        let c = cell(&kb, "engine-b", "proto-x", None, &eval());
        assert_eq!(c.support, Support::Supported); // subject-level implements
    }

    #[test]
    fn matrix_covers_every_triple() {
        let kb = kb();
        let m = matrix(&kb, &eval());
        // 2 components × (proto-x: v1+v2, proto-bare: None) = 2×3 = 6
        assert_eq!(m.len(), 6);
        let supported = m.iter().filter(|c| c.support == Support::Supported).count();
        let partial = m.iter().filter(|c| c.support == Support::Partial).count();
        let unsupported = m
            .iter()
            .filter(|c| c.support == Support::Unsupported)
            .count();
        assert_eq!(supported, 2); // engine-a@v2, engine-b@proto-bare
        assert_eq!(partial, 3); // engine-b@v1, engine-b@v2, engine-a@v1
        assert_eq!(unsupported, 1); // engine-a@proto-bare
    }

    #[test]
    fn evidence_carries_freshness() {
        let mut kb = kb();
        // Make engine-a's pin stale.
        for r in &mut kb.relations {
            if r.kind == RelationKind::ImplementsVersion {
                r.provenance = prov_at("2026-01-01");
            }
        }
        let c = cell(&kb, "engine-a", "proto-x", Some("proto-x-v2"), &eval());
        assert_eq!(c.support, Support::Supported);
        assert!(c
            .evidence
            .iter()
            .all(|e| e.freshness == FreshnessBand::Stale));
        let cells = [c];
        let stale = stale_cells(&cells);
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn unsupported_cells_are_never_stale() {
        let kb = kb();
        let c = cell(&kb, "engine-a", "proto-bare", None, &eval());
        assert!(stale_cells(&[c]).is_empty());
    }
}
