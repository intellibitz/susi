//! Unknown-field gap logging (VC-201-088 / T-CLAUDE-343).
//!
//! Protocol parsers see fields, event types and error codes the KB does not
//! know. Rather than silently dropping them, each unknown is counted per
//! context (`profile.endpoint`); the most frequent surface as KB-gap
//! proposals so the data keeps pace with the wire.

use crate::eco_openapi_ingest::Proposal;
use crate::eco_schema::Provenance;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Field names a document carries that the parser does not recognise.
/// `known` is matched against top-level object keys; `a.b` dotted entries in
/// `known` accept a nested prefix.
#[must_use]
pub fn unknown_keys(doc: &serde_json::Value, known: &[&str]) -> Vec<String> {
    let Some(obj) = doc.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<String> = obj
        .keys()
        .filter(|k| {
            !known
                .iter()
                .any(|d| *k == d || k.starts_with(&format!("{d}.")))
        })
        .cloned()
        .collect();
    out.sort();
    out
}

/// Frequency counts of unknown fields per context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownFieldLog {
    /// `"<context>/<field>"` -> times seen.
    counts: BTreeMap<String, u64>,
}

impl UnknownFieldLog {
    /// Record one unrecognised field under a context.
    pub fn record(&mut self, context: &str, field: &str) {
        *self.counts.entry(format!("{context}/{field}")).or_insert(0) += 1;
    }

    /// Record every unknown key of `doc` under a context.
    pub fn record_doc(&mut self, context: &str, doc: &serde_json::Value, known: &[&str]) {
        for k in unknown_keys(doc, known) {
            self.record(context, &k);
        }
    }

    /// The `n` most frequent `context/field` pairs, ties broken by name.
    #[must_use]
    pub fn top(&self, n: usize) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self.counts.clone().into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    /// Anything seen at least `min` times is a knowledge-base gap worth a
    /// review task — returned as proposals, never silently patched.
    #[must_use]
    pub fn gaps(&self, min: u64, src: &Provenance) -> Vec<Proposal> {
        self.counts
            .iter()
            .filter(|(_, c)| **c >= min)
            .map(|(key, count)| {
                let (ctx, field) = key.split_once('/').unwrap_or((key, ""));
                Proposal {
                    kind: "unknown-field".into(),
                    subject: ctx.to_string(),
                    summary: format!("unrecognised field `{field}` seen {count}x on {ctx}"),
                    detail: serde_json::json!({"field": field, "count": count}),
                    provenance: src.clone(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::Confidence;

    #[test]
    fn eco_unknown_fields_diffs_against_declared_shape() {
        let doc = serde_json::json!({
            "id": "x", "choices": [], "usage": {},
            "service_tier": "auto", "new_thing": 1
        });
        let unknown = unknown_keys(&doc, &["id", "choices", "usage"]);
        assert_eq!(unknown, vec!["new_thing", "service_tier"]);
    }

    #[test]
    fn eco_unknown_fields_counts_per_context() {
        let mut log = UnknownFieldLog::default();
        for _ in 0..5 {
            log.record("openai.chat", "new_thing");
        }
        log.record("openai.chat", "rare_field");
        let top = log.top(2);
        assert_eq!(top[0], ("openai.chat/new_thing".to_string(), 5));
        assert_eq!(top[1].1, 1);
    }

    #[test]
    fn eco_unknown_fields_frequent_gaps_become_proposals() {
        let mut log = UnknownFieldLog::default();
        for _ in 0..3 {
            log.record("openai.models", "mystery");
        }
        log.record("openai.models", "once");
        let src = Provenance {
            source: "parser".into(),
            spec_version: "live".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Inferred,
        };
        let gaps = log.gaps(2, &src);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].kind, "unknown-field");
        assert!(gaps[0].summary.contains("mystery"));
        assert_eq!(gaps[0].detail["count"], 3);
    }

    #[test]
    fn eco_unknown_fields_record_doc_sweeps_all_unknowns() {
        let mut log = UnknownFieldLog::default();
        let doc = serde_json::json!({"ok": 1, "mystery_a": 2, "mystery_b": 3});
        log.record_doc("ctx", &doc, &["ok"]);
        let top = log.top(10);
        assert_eq!(top.len(), 2);
    }
}
