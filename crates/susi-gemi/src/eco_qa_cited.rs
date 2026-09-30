//! Cited ecosystem Q&A (VC-201-088 / T-CLAUDE-349).
//!
//! "Does vendor X support Y?" is answered from knowledge-base facts with the
//! source, spec version and retrieval date attached — and an explicit
//! `Unknown` when the fact is absent or stale. An invented answer is worse
//! than an honest "we don't know".

use crate::models::eco_provenance::{freshness, Freshness, Policy};
use crate::models::eco_schema::{KnowledgeBase, Provenance, RelationKind};

/// A citation attached to an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Citation {
    pub source: String,
    pub spec_version: String,
    pub retrieved: String,
    pub confidence: String,
}

/// The answer to a capability/support question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// Recorded fact says yes — citations prove it.
    Yes { citations: Vec<Citation> },
    /// The subject exists but has no recorded fact for the question —
    /// reported honestly as unrecorded, not as "no".
    Unrecorded { subject: String },
    /// Recorded fact exists but its evidence is stale under policy.
    Stale {
        citations: Vec<Citation>,
        age_days: i64,
    },
    /// Nothing to answer from — subject unknown or question unanswerable.
    Unknown { reason: String },
}

fn cite(p: &Provenance) -> Citation {
    Citation {
        source: p.source.clone(),
        spec_version: p.spec_version.clone(),
        retrieved: p.retrieved.clone(),
        confidence: format!("{:?}", p.confidence).to_lowercase(),
    }
}

/// "Does `subject` provide `component`?" — vendor -> component.
#[must_use]
pub fn does_provide(
    kb: &KnowledgeBase,
    vendor: &str,
    component: &str,
    policy: &Policy,
    now: i64,
) -> Answer {
    answer_for(
        kb,
        component,
        kb.relations
            .iter()
            .filter(|r| r.kind == RelationKind::Provides && r.from == vendor && r.to == component),
        policy,
        now,
    )
}

/// "Does `component` support `capability`?" — component -> cap-*.
#[must_use]
pub fn supports(
    kb: &KnowledgeBase,
    component: &str,
    capability: &str,
    policy: &Policy,
    now: i64,
) -> Answer {
    answer_for(
        kb,
        component,
        kb.relations.iter().filter(|r| {
            r.kind == RelationKind::HasCapability && r.from == component && r.to == capability
        }),
        policy,
        now,
    )
}

/// "Does `component` implement `protocol`?" — component -> standard|protocol.
#[must_use]
pub fn implements(
    kb: &KnowledgeBase,
    component: &str,
    protocol: &str,
    policy: &Policy,
    now: i64,
) -> Answer {
    answer_for(
        kb,
        component,
        kb.relations.iter().filter(|r| {
            r.kind == RelationKind::Implements && r.from == component && r.to == protocol
        }),
        policy,
        now,
    )
}

fn answer_for<'a>(
    kb: &KnowledgeBase,
    subject: &str,
    relations: impl Iterator<Item = &'a crate::models::eco_schema::Relation>,
    policy: &Policy,
    now: i64,
) -> Answer {
    if kb.entity(subject).is_none() {
        return Answer::Unknown {
            reason: format!("{subject} is not in the knowledge base"),
        };
    }
    let relations: Vec<_> = relations.collect();
    if relations.is_empty() {
        return Answer::Unrecorded {
            subject: subject.to_string(),
        };
    }
    let mut oldest = 0i64;
    for r in &relations {
        match freshness(&r.provenance, policy, now) {
            Freshness::Stale { age_days } => oldest = oldest.max(age_days),
            Freshness::Unreadable => oldest = i64::MAX,
            Freshness::Fresh { .. } | Freshness::Aging { .. } | Freshness::Future { .. } => {}
        }
    }
    let citations: Vec<Citation> = relations.iter().map(|r| cite(&r.provenance)).collect();
    if oldest > 0 {
        Answer::Stale {
            citations,
            age_days: oldest,
        }
    } else {
        Answer::Yes { citations }
    }
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
    fn eco_qa_cited_supports_answers_yes_with_citations() {
        let kb = kb();
        match supports(&kb, "vllm", "cap-chat", &Policy::default(), 1_788_500_000) {
            Answer::Yes { citations } => {
                assert!(!citations.is_empty());
                assert!(!citations[0].source.is_empty());
                assert!(!citations[0].retrieved.is_empty());
            }
            other => panic!("expected yes, got {other:?}"),
        }
    }

    #[test]
    fn eco_qa_cited_unknown_subject_is_honest() {
        let kb = kb();
        match supports(&kb, "made-up-vendor", "cap-chat", &Policy::default(), 0) {
            Answer::Unknown { reason } => assert!(reason.contains("not in the knowledge base")),
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn eco_qa_cited_unrecorded_fact_is_not_a_no() {
        let kb = kb();
        // vllm exists but has no cap-audio fact — Unrecorded, never invented
        match supports(&kb, "vllm", "cap-audio", &Policy::default(), 0) {
            Answer::Unrecorded { subject } => assert_eq!(subject, "vllm"),
            other => panic!("expected unrecorded, got {other:?}"),
        }
    }

    #[test]
    fn eco_qa_cited_stale_fact_is_flagged_not_hidden() {
        let kb = kb();
        let p = Policy {
            stale_after_days: 0,
            warn_after_days: 0,
            ..Policy::default()
        };
        match supports(&kb, "vllm", "cap-chat", &p, i64::MAX / 4) {
            Answer::Stale { citations, .. } => assert!(!citations.is_empty()),
            other => panic!("expected stale, got {other:?}"),
        }
    }

    #[test]
    fn eco_qa_cited_provide_and_implement_forms() {
        let kb = kb();
        let now = i64::MAX / 4;
        match does_provide(&kb, "openai", "openai-api", &Policy::default(), now) {
            Answer::Yes { .. } | Answer::Stale { .. } => {}
            other => panic!("openai provides openai-api, got {other:?}"),
        }
        match implements(
            &kb,
            "vllm",
            "openai-chat-completions",
            &Policy::default(),
            now,
        ) {
            Answer::Yes { .. } | Answer::Stale { .. } => {}
            other => panic!("vllm implements chat completions, got {other:?}"),
        }
    }
}
