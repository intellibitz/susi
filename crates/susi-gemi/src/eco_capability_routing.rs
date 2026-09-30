//! Capability-constrained routing (VC-201-088 / T-CLAUDE-347).
//!
//! Routing asks "which component can do this" — vision, tool calling,
//! structured output — and the answer must come from recorded capability
//! facts and their freshness, not name heuristics. `route` scores every
//! candidate component: full coverage + fresh evidence first, stale or
//! partial coverage after, and the score explains itself.

use crate::models::eco_provenance::{freshness, Freshness, Policy};
use crate::models::eco_schema::{KnowledgeBase, RelationKind};

/// One routed candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    pub component: String,
    /// Required `cap-*` ids the component records.
    pub covered: Vec<String>,
    /// Required `cap-*` ids with no recorded fact.
    pub missing: Vec<String>,
    /// Oldest evidence band across the capability facts used.
    pub freshness: &'static str,
    /// Sources cited for the capability facts (reviewable).
    pub sources: Vec<String>,
}

impl Route {
    /// Rank key: full coverage beats partial; fresh beats stale.
    #[must_use]
    fn rank(&self) -> (bool, bool, std::cmp::Reverse<String>) {
        (
            self.missing.is_empty(),
            self.freshness != "stale",
            std::cmp::Reverse(self.component.clone()),
        )
    }
}

/// Route a request needing `required` capabilities across the components
/// that record any capability at all. `now_unix` feeds the freshness policy.
#[must_use]
pub fn route(kb: &KnowledgeBase, required: &[&str], policy: &Policy, now_unix: i64) -> Vec<Route> {
    let mut components: Vec<String> = kb
        .relations
        .iter()
        .filter(|r| r.kind == RelationKind::HasCapability)
        .map(|r| r.from.clone())
        .collect();
    components.sort();
    components.dedup();
    let mut routes = Vec::new();
    for c in components {
        let facts: Vec<_> = kb
            .relations
            .iter()
            .filter(|r| r.kind == RelationKind::HasCapability && r.from == c)
            .collect();
        let have: Vec<String> = facts.iter().map(|r| r.to.clone()).collect();
        let covered: Vec<String> = required
            .iter()
            .filter(|cap| have.iter().any(|h| h == *cap))
            .map(|s| (*s).to_string())
            .collect();
        if covered.is_empty() {
            continue; // no recorded overlap — not a candidate
        }
        let missing: Vec<String> = required
            .iter()
            .filter(|cap| !have.iter().any(|h| h == *cap))
            .map(|s| (*s).to_string())
            .collect();
        let mut worst = "fresh";
        let mut sources = Vec::new();
        for f in &facts {
            if covered.contains(&f.to) {
                match freshness(&f.provenance, policy, now_unix) {
                    Freshness::Stale { .. } | Freshness::Unreadable => worst = "stale",
                    Freshness::Fresh { .. } => {}
                    Freshness::Future { .. } => {}
                    Freshness::Aging { .. } if worst == "fresh" => worst = "aging",
                    Freshness::Aging { .. } => {}
                }
                sources.push(f.provenance.source.clone());
            }
        }
        sources.sort();
        sources.dedup();
        routes.push(Route {
            component: c,
            covered,
            missing,
            freshness: worst,
            sources,
        });
    }
    routes.sort_by_key(|r| std::cmp::Reverse(r.rank()));
    routes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::eco_provenance::Policy;
    use crate::models::eco_store;

    fn kb() -> KnowledgeBase {
        eco_store::load_dir(&eco_store::bundled_source_dir())
            .expect("bundled ecosystem store loads")
    }

    #[test]
    fn eco_capability_routing_prefers_full_coverage() {
        let kb = kb();
        let routes = route(
            &kb,
            &["cap-chat", "cap-tool-calling"],
            &Policy::default(),
            i64::MAX / 4,
        );
        assert!(!routes.is_empty());
        let first = &routes[0];
        assert!(first.missing.is_empty(), "{first:?}");
        assert!(first.covered.contains(&"cap-tool-calling".to_string()));
    }

    #[test]
    fn eco_capability_routing_reports_missing_capabilities() {
        let kb = kb();
        // llama.cpp server records chat but not tool-calling — partial route
        let routes = route(
            &kb,
            &["cap-chat", "cap-tool-calling"],
            &Policy::default(),
            i64::MAX / 4,
        );
        let lcpp = routes.iter().find(|r| r.component == "llama-cpp-server");
        if let Some(lcpp) = lcpp {
            assert!(lcpp.missing.contains(&"cap-tool-calling".to_string()));
        }
    }

    #[test]
    fn eco_capability_routing_no_candidates_for_unknown_caps() {
        let kb = kb();
        let routes = route(&kb, &["cap-teleportation"], &Policy::default(), 0);
        assert!(routes.is_empty());
    }

    #[test]
    fn eco_capability_routing_cites_sources() {
        let kb = kb();
        let routes = route(&kb, &["cap-chat"], &Policy::default(), i64::MAX / 4);
        for r in &routes {
            assert!(!r.sources.is_empty(), "{} cites nothing", r.component);
        }
    }

    #[test]
    fn eco_capability_routing_stale_evidence_is_flagged() {
        let kb = kb();
        // a policy that stales everything marks routes "stale"
        let p = Policy {
            stale_after_days: 0,
            warn_after_days: 0,
            ..Policy::default()
        };
        let routes = route(&kb, &["cap-chat"], &p, i64::MAX / 4);
        assert!(routes.iter().all(|r| r.freshness == "stale"));
    }
}
