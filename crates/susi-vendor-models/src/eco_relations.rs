//! Relation-graph queries over the ecosystem knowledge base.
//!
//! Answers the questions operators and other modules ask —
//! "which components implement protocol X", "what supersedes spec-version
//! Y", "what does component Z require" — as small, cycle-safe traversals
//! over [`KnowledgeBase`]. Every traversal carries a visited set, so a
//! malformed (or maliciously cyclic) base yields a partial answer, never
//! a hang.
use crate::eco_schema::{Entity, EntityKind, KnowledgeBase, Relation, RelationKind};
use std::collections::{HashMap, HashSet};

/// Read-only view over a knowledge base with an id index. Cheap to build;
/// queries borrow, nothing is copied.
pub struct Graph<'a> {
    kb: &'a KnowledgeBase,
    by_id: HashMap<&'a str, &'a Entity>,
}

impl<'a> Graph<'a> {
    #[must_use]
    pub fn new(kb: &'a KnowledgeBase) -> Self {
        Self {
            kb,
            by_id: kb.entities.iter().map(|e| (e.id(), e)).collect(),
        }
    }

    /// The entity for `id`, if present.
    #[must_use]
    pub fn entity(&self, id: &str) -> Option<&'a Entity> {
        self.by_id.get(id).copied()
    }

    fn edges(&self, kind: RelationKind) -> impl Iterator<Item = &'a Relation> {
        self.kb.relations.iter().filter(move |r| r.kind == kind)
    }

    /// `id -> entity` for ids that resolve and match `kinds`.
    fn resolve<'b>(
        &'b self,
        ids: impl Iterator<Item = &'b str> + 'b,
        kinds: &[EntityKind],
    ) -> Vec<&'a Entity> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for id in ids {
            if let Some(e) = self.entity(id) {
                if kinds.contains(&e.kind()) && seen.insert(e.id()) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// Components that implement the standard/protocol `subject` — directly
    /// via `implements`, or pinned via `implements-version` to a spec-version
    /// that is `version-of` the subject.
    #[must_use]
    pub fn implementers(&self, subject: &str) -> Vec<&'a Entity> {
        let mut ids: Vec<&str> = self
            .edges(RelationKind::Implements)
            .filter(|r| r.to == subject)
            .map(|r| r.from.as_str())
            .collect();
        let version_ids: HashSet<&str> = self
            .edges(RelationKind::VersionOf)
            .filter(|r| r.to == subject)
            .map(|r| r.from.as_str())
            .collect();
        ids.extend(
            self.edges(RelationKind::ImplementsVersion)
                .filter(move |r| version_ids.contains(r.to.as_str()))
                .map(|r| r.from.as_str()),
        );
        self.resolve(ids.into_iter(), &[EntityKind::Component])
    }

    /// Spec-versions of a standard or protocol (`version-of` sources).
    #[must_use]
    pub fn versions_of(&self, subject: &str) -> Vec<&'a Entity> {
        self.resolve(
            self.edges(RelationKind::VersionOf)
                .filter(|r| r.to == subject)
                .map(|r| r.from.as_str()),
            &[EntityKind::SpecVersion],
        )
    }

    /// The standard/protocol a spec-version versions.
    #[must_use]
    pub fn subject_of(&self, spec_version: &str) -> Option<&'a Entity> {
        self.edges(RelationKind::VersionOf)
            .find(|r| r.from == spec_version)
            .and_then(|r| self.entity(&r.to))
            .filter(|e| matches!(e.kind(), EntityKind::Standard | EntityKind::Protocol))
    }

    /// What supersedes `spec_version`: direct `supersedes` sources (newer
    /// versions replacing it). Cycle-safe.
    #[must_use]
    pub fn superseded_by(&self, spec_version: &str) -> Vec<&'a Entity> {
        self.resolve(
            self.edges(RelationKind::Supersedes)
                .filter(|r| r.to == spec_version)
                .map(|r| r.from.as_str()),
            &[EntityKind::SpecVersion],
        )
    }

    /// What `spec_version` supersedes: direct `supersedes` targets (the
    /// older versions it replaces). Cycle-safe.
    #[must_use]
    pub fn supersedes(&self, spec_version: &str) -> Vec<&'a Entity> {
        self.resolve(
            self.edges(RelationKind::Supersedes)
                .filter(|r| r.from == spec_version)
                .map(|r| r.to.as_str()),
            &[EntityKind::SpecVersion],
        )
    }

    /// The newest known spec-version of a subject: versions that nothing
    /// supersedes them... i.e. spec-versions never appearing as a
    /// `supersedes` *target*. None when the subject has no versions.
    #[must_use]
    pub fn latest_version(&self, subject: &str) -> Vec<&'a Entity> {
        let replaced: HashSet<&str> = self
            .edges(RelationKind::Supersedes)
            .map(|r| r.to.as_str())
            .collect();
        self.versions_of(subject)
            .into_iter()
            .filter(|v| !replaced.contains(v.id()))
            .collect()
    }

    /// Standards/protocols a component implements — `implements` targets plus
    /// the subjects of every spec-version it pins via `implements-version`.
    #[must_use]
    pub fn subjects_implemented(&self, component: &str) -> Vec<&'a Entity> {
        let mut ids: Vec<&str> = self
            .edges(RelationKind::Implements)
            .filter(|r| r.from == component)
            .map(|r| r.to.as_str())
            .collect();
        ids.extend(
            self.edges(RelationKind::ImplementsVersion)
                .filter(|r| r.from == component)
                .filter_map(|r| {
                    self.edges(RelationKind::VersionOf)
                        .find(|v| v.from == r.to)
                        .map(|v| v.to.as_str())
                }),
        );
        self.resolve(
            ids.into_iter(),
            &[EntityKind::Standard, EntityKind::Protocol],
        )
    }

    /// Spec-versions a component pins via `implements-version`.
    #[must_use]
    pub fn implemented_versions(&self, component: &str) -> Vec<&'a Entity> {
        self.resolve(
            self.edges(RelationKind::ImplementsVersion)
                .filter(|r| r.from == component)
                .map(|r| r.to.as_str()),
            &[EntityKind::SpecVersion],
        )
    }

    /// Capabilities a component exposes (`has-capability` targets).
    #[must_use]
    pub fn capabilities_of(&self, component: &str) -> Vec<&'a Entity> {
        self.resolve(
            self.edges(RelationKind::HasCapability)
                .filter(|r| r.from == component)
                .map(|r| r.to.as_str()),
            &[EntityKind::Capability],
        )
    }

    /// Vendors providing a component (`provides` sources).
    #[must_use]
    pub fn providers(&self, component: &str) -> Vec<&'a Entity> {
        self.resolve(
            self.edges(RelationKind::Provides)
                .filter(|r| r.to == component)
                .map(|r| r.from.as_str()),
            &[EntityKind::Vendor],
        )
    }

    /// Vendor that publishes a standard/protocol (`publishes` sources).
    #[must_use]
    pub fn publishers(&self, subject: &str) -> Vec<&'a Entity> {
        self.resolve(
            self.edges(RelationKind::Publishes)
                .filter(|r| r.to == subject)
                .map(|r| r.from.as_str()),
            &[EntityKind::Vendor],
        )
    }

    /// Transitive `depends-on` closure: everything `component` needs,
    /// directly or transitively. Cycle-safe — a cycle yields each member
    /// once rather than a hang.
    #[must_use]
    pub fn requirements(&self, component: &str) -> Vec<&'a Entity> {
        self.transitive(component, RelationKind::DependsOn, true)
    }

    /// Transitive reverse `depends-on`: everything that needs `component`.
    #[must_use]
    pub fn dependents(&self, component: &str) -> Vec<&'a Entity> {
        self.transitive(component, RelationKind::DependsOn, false)
    }

    /// Components `component` interoperates with (`compatible-with`, both
    /// directions — the edge is symmetric).
    #[must_use]
    pub fn compatible_with(&self, component: &str) -> Vec<&'a Entity> {
        let mut ids: Vec<&str> = self
            .edges(RelationKind::CompatibleWith)
            .filter_map(|r| {
                if r.from == component {
                    Some(r.to.as_str())
                } else if r.to == component {
                    Some(r.from.as_str())
                } else {
                    None
                }
            })
            .collect();
        ids.sort_unstable();
        self.resolve(ids.into_iter(), &[EntityKind::Component])
    }

    /// Transitive walk over `kind`; `forward` follows `from -> to`, reverse
    /// follows `to -> from`. The start node is never included.
    fn transitive(&self, start: &str, kind: RelationKind, forward: bool) -> Vec<&'a Entity> {
        let mut seen: HashSet<&str> = HashSet::from([start]);
        let mut stack = vec![start];
        let mut out = Vec::new();
        while let Some(node) = stack.pop() {
            for r in self.edges(kind) {
                let next = if forward {
                    (r.from == node).then_some(r.to.as_str())
                } else {
                    (r.to == node).then_some(r.from.as_str())
                };
                if let Some(next) = next {
                    if seen.insert(next) {
                        if let Some(e) = self.entity(next) {
                            out.push(e);
                        }
                        stack.push(next);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{
        Capability, CapabilityClass, Component, ComponentCategory, Confidence, Protocol,
        Provenance, SpecVersion, Standard, Transport, Vendor,
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

    /// acme (vendor) provides acme-engine; acme-api (protocol) has v1, v2
    /// (v2 supersedes v1); engine implements v1 and the protocol; agent
    /// depends on engine; engine has cap-tools.
    fn kb() -> KnowledgeBase {
        KnowledgeBase {
            version: crate::eco_schema::SCHEMA_VERSION.into(),
            entities: vec![
                Entity::Vendor(Vendor {
                    id: "acme".into(),
                    name: "Acme".into(),
                    home: "https://acme.example.test".into(),
                    provenance: prov(),
                }),
                Entity::Component(Component {
                    id: "acme-engine".into(),
                    name: "Engine".into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov(),
                }),
                Entity::Component(Component {
                    id: "acme-agent".into(),
                    name: "Agent".into(),
                    category: ComponentCategory::AgentRuntime,
                    provenance: prov(),
                }),
                Entity::Protocol(Protocol {
                    id: "acme-api".into(),
                    name: "Acme API".into(),
                    transports: vec![Transport::Http],
                    provenance: prov(),
                }),
                Entity::Standard(Standard {
                    id: "openai-api".into(),
                    name: "OpenAI API".into(),
                    body: "OpenAI".into(),
                    provenance: prov(),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "acme-api-v1".into(),
                    name: "v1".into(),
                    version: "1.0".into(),
                    released: None,
                    provenance: prov(),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "acme-api-v2".into(),
                    name: "v2".into(),
                    version: "2.0".into(),
                    released: None,
                    provenance: prov(),
                }),
                Entity::Capability(Capability {
                    id: "cap-tools".into(),
                    name: "Tools".into(),
                    class: CapabilityClass::ToolCalling,
                    provenance: prov(),
                }),
            ],
            relations: vec![
                rel(RelationKind::Provides, "acme", "acme-engine"),
                rel(RelationKind::Publishes, "acme", "acme-api"),
                rel(RelationKind::VersionOf, "acme-api-v1", "acme-api"),
                rel(RelationKind::VersionOf, "acme-api-v2", "acme-api"),
                rel(RelationKind::Supersedes, "acme-api-v2", "acme-api-v1"),
                rel(RelationKind::Implements, "acme-engine", "openai-api"),
                rel(
                    RelationKind::ImplementsVersion,
                    "acme-engine",
                    "acme-api-v1",
                ),
                rel(RelationKind::HasCapability, "acme-engine", "cap-tools"),
                rel(RelationKind::DependsOn, "acme-agent", "acme-engine"),
                rel(RelationKind::CompatibleWith, "acme-agent", "acme-engine"),
            ],
        }
    }

    fn ids<'a>(v: &[&'a Entity]) -> Vec<&'a str> {
        let mut ids: Vec<_> = v.iter().map(|e| e.id()).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn implementers_finds_direct_and_versioned() {
        let kb = kb();
        let g = Graph::new(&kb);
        // engine implements acme-api only via acme-api-v1
        assert_eq!(ids(&g.implementers("acme-api")), ["acme-engine"]);
        // and openai-api directly
        assert_eq!(ids(&g.implementers("openai-api")), ["acme-engine"]);
        assert!(g.implementers("nobody").is_empty());
    }

    #[test]
    fn versions_and_supersession_chain() {
        let kb = kb();
        let g = Graph::new(&kb);
        assert_eq!(
            ids(&g.versions_of("acme-api")),
            ["acme-api-v1", "acme-api-v2"]
        );
        assert_eq!(
            g.subject_of("acme-api-v1").map(|e| e.id()),
            Some("acme-api")
        );
        assert_eq!(ids(&g.superseded_by("acme-api-v1")), ["acme-api-v2"]);
        assert_eq!(ids(&g.supersedes("acme-api-v2")), ["acme-api-v1"]);
        assert_eq!(ids(&g.latest_version("acme-api")), ["acme-api-v2"]);
    }

    #[test]
    fn subjects_implemented_resolves_pinned_versions() {
        let kb = kb();
        let g = Graph::new(&kb);
        // implements openai-api directly + acme-api via acme-api-v1
        assert_eq!(
            ids(&g.subjects_implemented("acme-engine")),
            ["acme-api", "openai-api"]
        );
        assert_eq!(ids(&g.implemented_versions("acme-engine")), ["acme-api-v1"]);
    }

    #[test]
    fn requirements_and_dependents_are_transitive() {
        let mut kb = kb();
        kb.entities.push(Entity::Component(Component {
            id: "acme-cli".into(),
            name: "CLI".into(),
            category: ComponentCategory::CliTool,
            provenance: prov(),
        }));
        kb.relations
            .push(rel(RelationKind::DependsOn, "acme-cli", "acme-agent"));
        let g = Graph::new(&kb);
        assert_eq!(
            ids(&g.requirements("acme-cli")),
            ["acme-agent", "acme-engine"]
        );
        assert_eq!(
            ids(&g.dependents("acme-engine")),
            ["acme-agent", "acme-cli"]
        );
    }

    #[test]
    fn cycles_terminate() {
        let mut kb = kb();
        // acme-engine depends on acme-agent, which depends on it back.
        kb.relations
            .push(rel(RelationKind::DependsOn, "acme-engine", "acme-agent"));
        let g = Graph::new(&kb);
        let reqs = ids(&g.requirements("acme-agent"));
        assert_eq!(reqs, ["acme-engine"]); // each visited once, no hang
    }

    #[test]
    fn capabilities_providers_publishers() {
        let kb = kb();
        let g = Graph::new(&kb);
        assert_eq!(ids(&g.capabilities_of("acme-engine")), ["cap-tools"]);
        assert_eq!(ids(&g.providers("acme-engine")), ["acme"]);
        assert_eq!(ids(&g.publishers("acme-api")), ["acme"]);
    }

    #[test]
    fn compatible_with_is_symmetric() {
        let kb = kb();
        let g = Graph::new(&kb);
        assert_eq!(ids(&g.compatible_with("acme-engine")), ["acme-agent"]);
        assert_eq!(ids(&g.compatible_with("acme-agent")), ["acme-engine"]);
    }

    #[test]
    fn dangling_endpoints_are_skipped_not_fatal() {
        let mut kb = kb();
        // An edge to a ghost subject still names its real source.
        kb.relations
            .push(rel(RelationKind::Implements, "acme-agent", "ghost"));
        // An edge from a ghost component resolves to nothing.
        kb.relations
            .push(rel(RelationKind::Implements, "ghost-agent", "acme-api"));
        let g = Graph::new(&kb);
        assert_eq!(ids(&g.implementers("ghost")), ["acme-agent"]);
        assert_eq!(ids(&g.implementers("acme-api")), ["acme-engine"]);
        assert!(g.subject_of("ghost-version").is_none());
    }
}
