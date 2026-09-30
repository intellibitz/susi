//! Recorded-response replay corpus (VC-201-088 / T-CLAUDE-342).
//!
//! Real vendor responses — redacted, one file per (vendor, api-version,
//! endpoint) — replayed in tests so parsers and profiles are checked against
//! reality. The corpus lives outside the KB (`config/ecosystem/` stays
//! curated); entries carry their own provenance and a `redacted` flag that
//! is required, not optional.

use crate::eco_profile::Profile;
use crate::eco_schema::Provenance;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use susi_error::{EaiError, EaiResult};

/// One recorded, redacted response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CorpusEntry {
    pub id: String,
    pub vendor: String,
    /// API version the response was taken on (`2024-06`, `v1`).
    pub api_version: String,
    /// `METHOD path` — e.g. `POST /v1/chat/completions`.
    pub endpoint: String,
    pub recorded_at: String,
    /// The response body as received, with auth/secrets already stripped.
    pub response: serde_json::Value,
    /// Must be `true` — a corpus never holds unredacted user or key material.
    pub redacted: bool,
    pub provenance: Provenance,
}

/// How one replayed entry compares to its profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayDiff {
    pub entry_id: String,
    /// `response_shape` fields the profile declares but the corpus lacks.
    pub missing_fields: Vec<String>,
    /// Fields the corpus carries that no response shape declares.
    pub unexpected_fields: Vec<String>,
}

/// Persist one entry under `dir/<vendor>/<id>.json`.
///
/// # Errors
/// `EaiError::io` on write failure; `EaiError::config` on bad serialization.
pub fn record(dir: &Path, entry: &CorpusEntry) -> EaiResult<PathBuf> {
    let vdir = dir.join(&entry.vendor);
    std::fs::create_dir_all(&vdir).map_err(|e| EaiError::io(e.to_string()))?;
    let path = vdir.join(format!("{}.json", entry.id));
    let body = serde_json::to_string_pretty(entry).map_err(|e| EaiError::config(e.to_string()))?;
    std::fs::write(&path, body).map_err(|e| EaiError::io(e.to_string()))?;
    Ok(path)
}

/// Load every corpus entry under `dir` (recursively, sorted by path).
///
/// # Errors
/// `EaiError::io`/`config` on unreadable or malformed entries.
pub fn load_corpus(dir: &Path) -> EaiResult<Vec<CorpusEntry>> {
    let mut out = Vec::new();
    collect(dir, &mut out)?;
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn collect(dir: &Path, out: &mut Vec<CorpusEntry>) -> EaiResult<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir).map_err(|e| EaiError::io(e.to_string()))? {
        let path = entry.map_err(|e| EaiError::io(e.to_string()))?.path();
        if path.is_dir() {
            collect(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "json") {
            let text = std::fs::read_to_string(&path).map_err(|e| EaiError::io(e.to_string()))?;
            let e: CorpusEntry =
                serde_json::from_str(&text).map_err(|e| EaiError::config(e.to_string()))?;
            out.push(e);
        }
    }
    Ok(())
}

/// Replay a corpus entry against a profile: compare the recorded response's
/// top-level field names with every response shape the profile declares.
#[must_use]
pub fn replay(entry: &CorpusEntry, profile: &Profile) -> ReplayDiff {
    let fields: Vec<String> = entry
        .response
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let declared: Vec<String> = profile
        .response_shapes
        .iter()
        .flat_map(|s| s.fields.clone())
        .collect();
    ReplayDiff {
        entry_id: entry.id.clone(),
        missing_fields: declared
            .iter()
            .filter(|f| {
                !fields
                    .iter()
                    .any(|g| g == *f || g.starts_with(&format!("{f}.")))
            })
            .cloned()
            .collect(),
        unexpected_fields: fields
            .into_iter()
            .filter(|f| {
                !declared
                    .iter()
                    .any(|d| f == d || f.starts_with(&format!("{d}.")))
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::Confidence;

    fn entry(id: &str, response: serde_json::Value) -> CorpusEntry {
        CorpusEntry {
            id: id.into(),
            vendor: "openai".into(),
            api_version: "2024-06".into(),
            endpoint: "POST /v1/chat/completions".into(),
            recorded_at: "2026-09-01".into(),
            response,
            redacted: true,
            provenance: Provenance {
                source: "recorded-response".into(),
                spec_version: "2024-06".into(),
                retrieved: "2026-09-01".into(),
                confidence: Confidence::Verified,
            },
        }
    }

    #[test]
    fn eco_replay_corpus_roundtrip_via_disk() {
        let dir = std::env::temp_dir().join(format!("eco-corpus-{}", std::process::id()));
        let e = entry("chat-001", serde_json::json!({"id": "x", "choices": []}));
        record(&dir, &e).expect("records");
        let loaded = load_corpus(&dir).expect("loads");
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "chat-001");
        assert!(loaded[0].redacted);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eco_replay_corpus_replay_against_real_profile() {
        let p = crate::eco_profile::load_bundled("openai-chat-completions").unwrap();
        let e = entry(
            "chat-live",
            serde_json::json!({
                "id": "chatcmpl-1", "object": "chat.completion",
                "choices": [{"index": 0}], "usage": {"total_tokens": 3},
                "surprise_new_field": {"a": 1}
            }),
        );
        let diff = replay(&e, &p);
        assert_eq!(diff.entry_id, "chat-live");
        // a field the profile never declared is surfaced for review
        assert!(diff
            .unexpected_fields
            .contains(&"surprise_new_field".to_string()));
    }

    #[test]
    fn eco_replay_corpus_missing_declared_fields_are_flagged() {
        let p = crate::eco_profile::load_bundled("openai-chat-completions").unwrap();
        let e = entry("chat-thin", serde_json::json!({"id": "only-id"}));
        let diff = replay(&e, &p);
        assert!(
            !diff.missing_fields.is_empty(),
            "thin response must flag gaps"
        );
    }

    #[test]
    fn eco_replay_corpus_entries_carry_provenance() {
        let e = entry("x", serde_json::json!({}));
        assert_eq!(e.provenance.confidence, Confidence::Verified);
        assert!(!e.provenance.retrieved.is_empty());
        assert!(!e.api_version.is_empty());
    }
}
