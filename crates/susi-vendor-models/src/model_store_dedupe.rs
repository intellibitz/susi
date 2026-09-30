//! Model store dedupe across Ollama / LM Studio / HF cache.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeightRef {
    pub store: String,
    pub path: String,
    pub digest: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateGroup {
    pub digest: String,
    pub paths: Vec<WeightRef>,
    pub wasted_bytes: u64,
    pub keep: String,
}

#[must_use]
pub fn find_duplicate_weights(refs: &[WeightRef]) -> Vec<DuplicateGroup> {
    let mut by: BTreeMap<&str, Vec<&WeightRef>> = BTreeMap::new();
    for r in refs {
        by.entry(r.digest.as_str()).or_default().push(r);
    }
    let mut out = Vec::new();
    for (digest, group) in by {
        if group.len() < 2 {
            continue;
        }
        let wasted_bytes = group.iter().skip(1).map(|g| g.size_bytes).sum();
        let keep = group[0].path.clone();
        out.push(DuplicateGroup {
            digest: digest.to_string(),
            paths: group.into_iter().cloned().collect(),
            wasted_bytes,
            keep,
        });
    }
    out
}

#[cfg(test)]
mod model_store_dedupe_tests {
    use super::*;

    #[test]
    fn model_store_dedupe_reports_cross_store_duplicates() {
        let refs = vec![
            WeightRef {
                store: "ollama".into(),
                path: "/o/a".into(),
                digest: "sha256:1".into(),
                size_bytes: 100,
            },
            WeightRef {
                store: "lmstudio".into(),
                path: "/l/a".into(),
                digest: "sha256:1".into(),
                size_bytes: 100,
            },
            WeightRef {
                store: "hf".into(),
                path: "/h/b".into(),
                digest: "sha256:2".into(),
                size_bytes: 50,
            },
        ];
        let dups = find_duplicate_weights(&refs);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].paths.len(), 2);
        assert_eq!(dups[0].wasted_bytes, 100);
        assert_eq!(dups[0].keep, "/o/a");
    }
}
