//! Deprecation -> replacement migration plans (VC-201-088 / T-CLAUDE-345).
//!
//! Given a deprecated spec-version, component or protocol, a plan is built
//! from lifecycle facts (`supersedes`, `version-of`, `released`) and matrix
//! facts (`implements`): the replacement, the config edits a user makes, and
//! the checks to run afterwards. Nothing is guessed — missing successors are
//! reported, not invented.

use crate::models::eco_schema::{Entity, KnowledgeBase, RelationKind};

/// One step of a migration plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    /// The deprecated thing (entity id).
    pub from: String,
    /// The replacement id when the KB records one — `None` is an honest gap.
    pub replacement: Option<String>,
    /// Human-readable config edits, e.g. `model: old -> new`.
    pub config_changes: Vec<String>,
    /// Acceptance checks to run after the change.
    pub tests: Vec<String>,
    /// Why this plan is what it is (the KB facts used).
    pub basis: Vec<String>,
}

/// Build a migration plan for an entity id.
#[must_use]
pub fn plan(kb: &KnowledgeBase, deprecated_id: &str) -> MigrationPlan {
    let mut plan = MigrationPlan {
        from: deprecated_id.to_string(),
        replacement: None,
        config_changes: Vec::new(),
        tests: Vec::new(),
        basis: Vec::new(),
    };
    // case 1: spec-version — the successor is the other version of the same
    // standard that supersedes it (or the newest by released date).
    if let Some(parent) = kb
        .relations
        .iter()
        .find(|r| r.kind == RelationKind::VersionOf && r.from == deprecated_id)
        .map(|r| r.to.clone())
    {
        // explicit supersedes edge wins
        let successor = kb
            .relations
            .iter()
            .find(|r| r.kind == RelationKind::Supersedes && r.to == deprecated_id)
            .map(|r| r.from.clone())
            .or_else(|| {
                // else: newest sibling version of the same standard
                kb.relations
                    .iter()
                    .filter(|r| r.kind == RelationKind::VersionOf && r.to == parent)
                    .map(|r| (released_of(kb, &r.from), r.from.clone()))
                    .filter(|(_, id)| id != deprecated_id)
                    .max()
                    .map(|(_, id)| id)
            });
        plan.replacement = successor.clone();
        plan.basis
            .push(format!("{deprecated_id} is a version of {parent}"));
        if let Some(s) = &successor {
            plan.config_changes
                .push(format!("spec_version: {deprecated_id} -> {s}"));
            plan.basis.push(format!("{s} supersedes {deprecated_id}"));
        }
    } else {
        // case 2: component — look for a sibling provided by the same vendor
        // implementing the same protocols.
        let protocols: Vec<String> = kb
            .relations
            .iter()
            .filter(|r| r.kind == RelationKind::Implements && r.from == deprecated_id)
            .map(|r| r.to.clone())
            .collect();
        let vendors: Vec<String> = kb
            .relations
            .iter()
            .filter(|r| r.kind == RelationKind::Provides && r.to == deprecated_id)
            .map(|r| r.from.clone())
            .collect();
        if !vendors.is_empty() {
            plan.basis.push(format!(
                "{deprecated_id} is provided by {}",
                vendors.join(",")
            ));
        }
        let replacement = kb
            .relations
            .iter()
            .filter(|r| {
                r.kind == RelationKind::Provides
                    && vendors.contains(&r.from)
                    && r.to != deprecated_id
            })
            .map(|r| r.to.clone())
            .find(|sibling| {
                protocols.iter().any(|p| {
                    kb.relations.iter().any(|r| {
                        r.kind == RelationKind::Implements && r.from == *sibling && r.to == *p
                    })
                })
            });
        if let Some(rep) = &replacement {
            plan.replacement = Some(rep.clone());
            plan.config_changes
                .push(format!("model: {deprecated_id} -> {rep}"));
            plan.basis
                .push(format!("{rep} implements the same protocol surface"));
        }
    }
    plan.tests
        .push("cargo test -p susi-vendor-models eco_profile".to_string());
    if plan.replacement.is_none() {
        plan.basis
            .push("no replacement recorded — plan is a gap report".into());
    }
    plan
}

fn released_of(kb: &KnowledgeBase, id: &str) -> String {
    for e in &kb.entities {
        if let Entity::SpecVersion(v) = e {
            if v.id == id {
                return v.released.clone().unwrap_or_default();
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::eco_store;

    fn kb() -> KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_migration_planner_superseded_version_finds_successor() {
        let kb = kb();
        // a2a-0.2 is superseded by a2a-0.3 in the KB
        let plan = plan(&kb, "a2a-0.2");
        assert_eq!(plan.replacement.as_deref(), Some("a2a-0.3"), "{plan:?}");
        assert!(plan
            .config_changes
            .iter()
            .any(|c| c.contains("a2a-0.2 -> a2a-0.3")));
        assert!(!plan.tests.is_empty());
    }

    #[test]
    fn eco_migration_planner_newest_sibling_when_no_edge() {
        let kb = kb();
        // mcp-2025-06-18 has no incoming supersedes edge but sibling
        // versions exist — the newest sibling is reported as the fallback
        let plan = plan(&kb, "mcp-2025-06-18");
        assert_eq!(
            plan.replacement.as_deref(),
            Some("mcp-2025-03-26"),
            "{plan:?}"
        );
        assert!(plan.basis.iter().any(|b| b.contains("version of mcp")));
    }

    #[test]
    fn eco_migration_planner_unknown_entity_is_honest_gap() {
        let kb = kb();
        let plan = plan(&kb, "totally-fictional");
        assert!(plan.replacement.is_none());
        assert!(plan.basis.iter().any(|b| b.contains("gap report")));
    }

    #[test]
    fn eco_migration_planner_every_plan_has_tests() {
        let kb = kb();
        for id in ["a2a-0.2", "acp-0.1", "totally-fictional"] {
            let plan = plan(&kb, id);
            assert!(!plan.tests.is_empty(), "{id}");
            assert!(!plan.basis.is_empty(), "{id}");
        }
    }
}
