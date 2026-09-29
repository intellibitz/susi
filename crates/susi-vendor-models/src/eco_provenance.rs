//! Provenance and freshness policy for the ecosystem knowledge base
//! (`crate::eco_schema`).
//!
//! Every fact already *carries* provenance under eco-schema/v1 — this module
//! decides whether that provenance is good enough:
//!
//! - **A fact without a source is rejected.** `source` must be an
//!   `http(s)://` URL pointing at an official machine-readable spec or a
//!   `cite:`-prefixed citation of a primary source (publisher docs,
//!   changelogs, spec PDFs). Bare free text is not a source — it cannot be
//!   audited. This is the machine-checkable form of the rule that facts come
//!   from official specs or cited primary sources, never scraped.
//! - **Facts age.** `retrieved` drives a freshness band: fresh until
//!   [`Policy::warn_after_days`], aging until [`Policy::stale_after_days`],
//!   stale after that. [`check`] reports stale facts as issues so refreshes
//!   are driven by evidence, not vibes.
//! - **Clock sanity.** A `retrieved` date in the future is an error, not a
//!   fact.
//!
//! `now` is always an argument — the wall clock never leaks into the
//! validator, keeping checks deterministic and tests hermetic.
use crate::eco_schema::{date_to_days, Entity, Issue, KnowledgeBase, Relation, Stage};
use serde::Serialize;

const SECS_PER_DAY: i64 = 86_400;

/// Freshness thresholds applied to `provenance.retrieved`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Facts older than this many days are reported as stale.
    pub stale_after_days: i64,
    /// Facts older than this many days are "aging" — reported by
    /// [`freshness`] but not yet an issue.
    pub warn_after_days: i64,
    /// When true (default), `source` must be an `http(s)://` URL or a
    /// `cite:`-prefixed citation so every claim can be audited.
    pub require_citable_source: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            stale_after_days: 90,
            warn_after_days: 30,
            require_citable_source: true,
        }
    }
}

/// Freshness band of a single fact's provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Retrieved within the warn window.
    Fresh { age_days: i64 },
    /// Older than `warn_after_days`, not yet stale.
    Aging { age_days: i64 },
    /// Older than `stale_after_days`; an issue in [`check`].
    Stale { age_days: i64 },
    /// `retrieved` is in the future.
    Future { age_days: i64 },
    /// `retrieved` is not a real `YYYY-MM-DD` date.
    Unreadable,
}

/// Age of a fact in days: `now_unix` (seconds since epoch) minus
/// `retrieved` in whole days. Negative ages are returned as-is — the caller
/// treats them as future-dated.
#[must_use]
pub fn age_days(retrieved: &str, now_unix: i64) -> Option<i64> {
    let retrieved_days = date_to_days(retrieved)?;
    Some(now_unix.div_euclid(SECS_PER_DAY) - retrieved_days)
}

/// Freshness band for one provenance record under `policy`.
#[must_use]
pub fn freshness(
    prov: &crate::eco_schema::Provenance,
    policy: &Policy,
    now_unix: i64,
) -> Freshness {
    match age_days(&prov.retrieved, now_unix) {
        None => Freshness::Unreadable,
        Some(age) if age < 0 => Freshness::Future { age_days: age },
        Some(age) if age > policy.stale_after_days => Freshness::Stale { age_days: age },
        Some(age) if age > policy.warn_after_days => Freshness::Aging { age_days: age },
        Some(age) => Freshness::Fresh { age_days: age },
    }
}

/// True when `source` names something an auditor can open: an `http(s)://`
/// URL or a `cite:`-prefixed citation of a primary source.
#[must_use]
pub fn source_is_citable(source: &str) -> bool {
    let s = source.trim();
    if s.len() > 2048 || s.chars().any(char::is_control) {
        return false;
    }
    if let Some(rest) = s.strip_prefix("cite:") {
        return !rest.trim().is_empty();
    }
    if let Some(rest) = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
    {
        // Must name a host and may not contain whitespace.
        return !rest.is_empty()
            && !rest.chars().any(char::is_whitespace)
            && rest.split('/').next().is_some_and(|h| h.contains('.'));
    }
    false
}

/// Where in a knowledge base a fact's provenance lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactRef {
    /// `/entities/<i>` or `/relations/<i>`.
    pub path: String,
    /// Entity id or `kind:from->to` for relations — readable in reports.
    pub fact: String,
}

impl FactRef {
    fn entity(i: usize, e: &Entity) -> Self {
        Self {
            path: format!("/entities/{i}"),
            fact: e.id().to_string(),
        }
    }
    fn relation(i: usize, r: &Relation) -> Self {
        Self {
            path: format!("/relations/{i}"),
            fact: format!("{}:{}->{}", r.kind.label(), r.from, r.to),
        }
    }
}

/// Iterate every fact in the knowledge base: entities then relations, in
/// document order.
pub fn facts(kb: &KnowledgeBase) -> Vec<(FactRef, &crate::eco_schema::Provenance)> {
    let mut out = Vec::with_capacity(kb.entities.len() + kb.relations.len());
    for (i, e) in kb.entities.iter().enumerate() {
        out.push((FactRef::entity(i, e), e.provenance()));
    }
    for (i, r) in kb.relations.iter().enumerate() {
        out.push((FactRef::relation(i, r), &r.provenance));
    }
    out
}

/// Policy validation over the typed model: rejects facts without a source,
/// facts whose source is not citable, unreadable or future `retrieved`
/// dates, and facts past the staleness window. Composes with
/// [`crate::eco_schema::validate`] — issues here carry `Stage::Policy`.
#[must_use]
pub fn check(kb: &KnowledgeBase, policy: &Policy, now_unix: i64) -> Vec<Issue> {
    let mut issues = Vec::new();
    for (fact, prov) in facts(kb) {
        let base = format!("{}/provenance", fact.path);
        if prov.source.trim().is_empty() {
            issues.push(Issue {
                stage: Stage::Policy,
                path: format!("{base}/source"),
                message: format!(
                    "fact {:?} has no source; every fact must cite one",
                    fact.fact
                ),
            });
        } else if policy.require_citable_source && !source_is_citable(&prov.source) {
            issues.push(Issue {
                stage: Stage::Policy,
                path: format!("{base}/source"),
                message: format!(
                    "fact {:?} source {:?} is not an http(s) URL or cite: citation",
                    fact.fact, prov.source
                ),
            });
        }
        match freshness(prov, policy, now_unix) {
            Freshness::Stale { age_days } => issues.push(Issue {
                stage: Stage::Policy,
                path: base.clone(),
                message: format!(
                    "fact {:?} is {age_days} days old (stale after {})",
                    fact.fact, policy.stale_after_days
                ),
            }),
            Freshness::Future { .. } => issues.push(Issue {
                stage: Stage::Policy,
                path: format!("{base}/retrieved"),
                message: format!(
                    "fact {:?} retrieved {:?} is in the future",
                    fact.fact, prov.retrieved
                ),
            }),
            Freshness::Unreadable => issues.push(Issue {
                stage: Stage::Policy,
                path: format!("{base}/retrieved"),
                message: format!(
                    "fact {:?} retrieved {:?} is not a real YYYY-MM-DD date",
                    fact.fact, prov.retrieved
                ),
            }),
            Freshness::Fresh { .. } | Freshness::Aging { .. } => {}
        }
    }
    issues
}

/// Stale facts only — the refresh worklist. `spec_version` naming the spec
/// the fact was last checked against lets a refresh re-read the same spec.
#[must_use]
pub fn stale_facts(
    kb: &KnowledgeBase,
    policy: &Policy,
    now_unix: i64,
) -> Vec<(FactRef, Freshness)> {
    facts(kb)
        .into_iter()
        .filter_map(|(f, p)| {
            let fresh = freshness(p, policy, now_unix);
            matches!(fresh, Freshness::Stale { .. } | Freshness::Unreadable).then_some((f, fresh))
        })
        .collect()
}

/// Count of facts per freshness band — the summary a dashboard shows.
#[must_use]
pub fn freshness_summary(kb: &KnowledgeBase, policy: &Policy, now_unix: i64) -> FreshnessSummary {
    let mut summary = FreshnessSummary::default();
    for (_, prov) in facts(kb) {
        match freshness(prov, policy, now_unix) {
            Freshness::Fresh { .. } => summary.fresh += 1,
            Freshness::Aging { .. } => summary.aging += 1,
            Freshness::Stale { .. } => summary.stale += 1,
            Freshness::Future { .. } | Freshness::Unreadable => summary.invalid += 1,
        }
    }
    summary
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct FreshnessSummary {
    pub fresh: usize,
    pub aging: usize,
    pub stale: usize,
    pub invalid: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::{
        Capability, CapabilityClass, Component, ComponentCategory, Confidence, EntityKind,
        Provenance, RelationKind, SpecVersion, Vendor,
    };

    const NOW: i64 = 1_790_000_000; // ~2026-09-28

    fn prov_on(retrieved: &str) -> Provenance {
        Provenance {
            source: "https://specs.example.test/v1".into(),
            spec_version: "1.0".into(),
            retrieved: retrieved.into(),
            confidence: Confidence::Verified,
        }
    }

    fn kb_with(prov: Provenance) -> KnowledgeBase {
        KnowledgeBase {
            version: crate::eco_schema::SCHEMA_VERSION.into(),
            entities: vec![
                Entity::Vendor(Vendor {
                    id: "acme".into(),
                    name: "Acme".into(),
                    home: "https://acme.example.test".into(),
                    provenance: prov.clone(),
                }),
                Entity::Component(Component {
                    id: "acme-engine".into(),
                    name: "Engine".into(),
                    category: ComponentCategory::InferenceEngine,
                    provenance: prov.clone(),
                }),
                Entity::SpecVersion(SpecVersion {
                    id: "acme-api-v1".into(),
                    name: "API v1".into(),
                    version: "1.0".into(),
                    released: None,
                    provenance: prov.clone(),
                }),
                Entity::Capability(Capability {
                    id: "cap-x".into(),
                    name: "X".into(),
                    class: CapabilityClass::ToolCalling,
                    provenance: prov.clone(),
                }),
            ],
            relations: vec![
                Relation {
                    kind: RelationKind::Provides,
                    from: "acme".into(),
                    to: "acme-engine".into(),
                    provenance: prov.clone(),
                },
                Relation {
                    kind: RelationKind::VersionOf,
                    from: "acme-api-v1".into(),
                    to: "acme-engine".into(),
                    provenance: prov,
                },
            ],
        }
    }

    #[test]
    fn date_to_days_anchor_points() {
        assert_eq!(date_to_days("1970-01-01"), Some(0));
        assert_eq!(date_to_days("2000-01-01"), Some(10957));
        assert_eq!(date_to_days("2024-02-29"), Some(19782));
        assert_eq!(date_to_days("2025-02-29"), None);
        assert_eq!(date_to_days("garbage"), None);
    }

    #[test]
    fn freshness_bands_follow_the_policy() {
        let policy = Policy {
            warn_after_days: 30,
            stale_after_days: 90,
            require_citable_source: true,
        };
        // NOW = 1_790_000_000s -> day 20717 since epoch.
        // 2026-09-01 -> day 20697 -> age 20: fresh.
        let fresh = freshness(&prov_on("2026-09-01"), &policy, NOW);
        assert!(matches!(fresh, Freshness::Fresh { age_days } if (15..=25).contains(&age_days)));
        // 2026-08-10 -> age 42: aging.
        let aging = freshness(&prov_on("2026-08-10"), &policy, NOW);
        assert!(matches!(aging, Freshness::Aging { age_days } if (30..=60).contains(&age_days)));
        // 2026-03-01 -> age ~204: stale.
        let stale = freshness(&prov_on("2026-03-01"), &policy, NOW);
        assert!(matches!(stale, Freshness::Stale { age_days } if age_days > 90));
        let future = freshness(&prov_on("2027-01-01"), &policy, NOW);
        assert!(matches!(future, Freshness::Future { .. }));
        let bad = freshness(&prov_on("nope"), &policy, NOW);
        assert_eq!(bad, Freshness::Unreadable);
    }

    #[test]
    fn fresh_kb_passes_policy() {
        let kb = kb_with(prov_on("2026-09-01"));
        assert!(check(&kb, &Policy::default(), NOW).is_empty());
        let s = freshness_summary(&kb, &Policy::default(), NOW);
        assert_eq!(s.fresh, 6);
        assert_eq!(s.stale + s.aging + s.invalid, 0);
    }

    #[test]
    fn stale_facts_are_flagged_with_age() {
        let kb = kb_with(prov_on("2026-03-01"));
        let issues = check(&kb, &Policy::default(), NOW);
        assert_eq!(issues.len(), 6, "{issues:?}");
        assert!(issues
            .iter()
            .all(|i| i.stage == Stage::Policy && i.message.contains("days old")));
        let stale = stale_facts(&kb, &Policy::default(), NOW);
        assert_eq!(stale.len(), 6);
    }

    #[test]
    fn aging_is_a_band_not_an_issue() {
        let policy = Policy::default();
        let kb = kb_with(prov_on("2026-08-10"));
        assert!(check(&kb, &policy, NOW).is_empty());
        assert!(freshness_summary(&kb, &policy, NOW).aging == 6);
    }

    #[test]
    fn facts_without_a_source_are_rejected() {
        let mut kb = kb_with(prov_on("2026-09-01"));
        kb.entities[0] = Entity::Vendor(Vendor {
            id: "acme".into(),
            name: "Acme".into(),
            home: "https://acme.example.test".into(),
            provenance: Provenance {
                source: "  ".into(),
                ..prov_on("2026-09-01")
            },
        });
        let issues = check(&kb, &Policy::default(), NOW);
        assert!(issues
            .iter()
            .any(|i| i.path.ends_with("/source") && i.message.contains("no source")));
    }

    #[test]
    fn sources_must_be_citable_when_the_policy_asks() {
        assert!(source_is_citable(
            "https://modelcontextprotocol.io/specification/2025-06-18"
        ));
        assert!(source_is_citable("http://spec.example.test/v2"));
        assert!(source_is_citable("cite: openai platform docs 2026-09"));
        assert!(!source_is_citable("vendor docs"));
        assert!(!source_is_citable("https://"));
        assert!(!source_is_citable("https://nodot"));
        assert!(!source_is_citable("cite:"));
        assert!(!source_is_citable("ftp://example.test/spec"));

        let mut kb = kb_with(prov_on("2026-09-01"));
        kb.entities[0] = Entity::Vendor(Vendor {
            id: "acme".into(),
            name: "Acme".into(),
            home: "https://acme.example.test".into(),
            provenance: Provenance {
                source: "vendor docs".into(),
                ..prov_on("2026-09-01")
            },
        });
        let policy = Policy::default();
        assert!(check(&kb, &policy, NOW)
            .iter()
            .any(|i| i.stage == Stage::Policy && i.message.contains("not an http(s) URL")));
        let relaxed = Policy {
            require_citable_source: false,
            ..policy
        };
        assert!(check(&kb, &relaxed, NOW).is_empty());
    }

    #[test]
    fn future_and_unreadable_dates_are_rejected() {
        for bad in ["2027-06-01", "not-a-date", "2026-02-30"] {
            let kb = kb_with(prov_on(bad));
            let issues = check(&kb, &Policy::default(), NOW);
            assert!(
                issues.iter().any(|i| i.path.ends_with("/retrieved")),
                "{bad}: {issues:?}"
            );
        }
    }

    #[test]
    fn relation_facts_are_checked_too() {
        let mut kb = kb_with(prov_on("2026-09-01"));
        kb.relations[0].provenance = prov_on("2026-01-01");
        let issues = check(&kb, &Policy::default(), NOW);
        assert!(issues
            .iter()
            .any(|i| i.path.starts_with("/relations/0") && i.message.contains("days old")));
    }

    #[test]
    fn policy_thresholds_are_configurable() {
        let tight = Policy {
            warn_after_days: 1,
            stale_after_days: 5,
            require_citable_source: true,
        };
        let kb = kb_with(prov_on("2026-09-01"));
        let issues = check(&kb, &tight, NOW);
        assert_eq!(issues.len(), 6);
    }

    #[test]
    fn entity_kind_helper_used_for_fact_labels() {
        let kb = kb_with(prov_on("2026-09-01"));
        let all = facts(&kb);
        assert_eq!(all.len(), 6);
        assert_eq!(kb.entities[0].kind(), EntityKind::Vendor);
        assert!(all
            .iter()
            .any(|(f, _)| f.fact.starts_with("provides:acme->")));
    }
}
