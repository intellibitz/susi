//! New-model detection from `/models` diffs (VC-201-088 / T-CLAUDE-339).
//!
//! A vendor's live model list drifts: models appear and disappear. Diffing
//! two snapshots yields provisional entity entries — `Speculative`
//! confidence, linked to the probe that saw them, marked `awaiting-profile`.
//! Nothing is merged into the store automatically; provisional entries are
//! reviewable proposals like everything else from the live edge.

use crate::eco_schema::{ComponentCategory, Confidence, Provenance, RelationKind};

/// What changed between two `/models` snapshots.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelListDiff {
    /// Model ids present now that were not before.
    pub added: Vec<String>,
    /// Model ids gone since the last snapshot.
    pub removed: Vec<String>,
}

/// Diff two sorted-or-unsorted id lists.
#[must_use]
pub fn diff_model_lists(before: &[String], after: &[String]) -> ModelListDiff {
    let mut added: Vec<String> = after
        .iter()
        .filter(|id| !before.contains(id))
        .cloned()
        .collect();
    let mut removed: Vec<String> = before
        .iter()
        .filter(|id| !after.contains(id))
        .cloned()
        .collect();
    added.sort();
    removed.sort();
    ModelListDiff { added, removed }
}

/// A provisional entity document for a freshly spotted model — the exact
/// shape `eco_schema::validate_document` accepts, with a `pending` marker in
/// the name so review UIs can filter for it.
#[must_use]
pub fn provisional_entity(
    vendor_id: &str,
    model_id: &str,
    probe_src: &Provenance,
) -> serde_json::Value {
    let prov = Provenance {
        source: probe_src.source.clone(),
        spec_version: probe_src.spec_version.clone(),
        retrieved: probe_src.retrieved.clone(),
        confidence: Confidence::Speculative,
    };
    serde_json::json!({
        "version": "eco-schema/v1",
        "entity": {
            "kind": "component",
            "id": model_id,
            "name": format!("{model_id} (provisional — awaiting profile)"),
            "category": ComponentCategory::Model,
            "provenance": prov,
        },
        "relations": [{
            "kind": RelationKind::Provides,
            "from": vendor_id,
            "to": model_id,
            "provenance": prov,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eco_schema::Confidence;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn eco_new_model_detect_diffs_snapshots() {
        let diff = diff_model_lists(
            &ids(&["gpt-4o", "gpt-4o-mini", "o1"]),
            &ids(&["gpt-4o", "o1", "o3", "gpt-4.1"]),
        );
        assert_eq!(diff.added, ids(&["gpt-4.1", "o3"]));
        assert_eq!(diff.removed, ids(&["gpt-4o-mini"]));
    }

    #[test]
    fn eco_new_model_detect_no_diff_when_stable() {
        let diff = diff_model_lists(&ids(&["a", "b"]), &ids(&["b", "a"]));
        assert!(diff.added.is_empty() && diff.removed.is_empty());
    }

    #[test]
    fn eco_new_model_detect_provisional_entity_is_schema_valid() {
        let probe = Provenance {
            source: "probe:GET https://api.openai.com/v1/models".into(),
            spec_version: "2024-06".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Verified,
        };
        let doc = provisional_entity("openai", "gpt-5-turbo", &probe);
        // the provisional file must parse as a real entity file
        let file: crate::eco_store::EntityFile =
            serde_json::from_value(doc.clone()).expect("entity-file schema");
        assert_eq!(file.entity.id(), "gpt-5-turbo");
        // and the provisional store it would land in stays consistent
        let dir = std::env::temp_dir().join(format!("eco-newmodel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let vendor = serde_json::json!({
            "version": "eco-schema/v1",
            "entity": {
                "kind": "vendor", "id": "openai", "name": "OpenAI",
                "home": "https://platform.openai.com",
                "provenance": {"source": "x", "spec_version": "v", "retrieved": "2026-09-01", "confidence": "verified"},
            },
            "relations": [],
        });
        std::fs::write(
            dir.join("openai.json"),
            serde_json::to_string(&vendor).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("gpt-5-turbo.json"),
            serde_json::to_string(&doc).unwrap(),
        )
        .unwrap();
        let kb = crate::eco_store::load_dir(&dir).expect("provisional store loads");
        assert!(kb.entity("gpt-5-turbo").is_some());
        let issues = crate::eco_consistency::check(&kb);
        assert!(issues.is_empty(), "{issues:?}");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(doc["entity"]["provenance"]["confidence"], "speculative");
        assert!(doc["entity"]["name"]
            .as_str()
            .unwrap()
            .contains("awaiting profile"));
        // linked to the probe that saw it
        assert!(doc["entity"]["provenance"]["source"]
            .as_str()
            .unwrap()
            .contains("probe:"));
    }

    #[test]
    fn eco_new_model_detect_provisional_is_low_confidence_not_auto_profiled() {
        let probe = Provenance {
            source: "probe:/models".into(),
            spec_version: "x".into(),
            retrieved: "2026-09-01".into(),
            confidence: Confidence::Reported,
        };
        let doc = provisional_entity("vllm", "mystery-model-9b", &probe);
        // regardless of the probe's confidence, the new fact is speculative
        assert_eq!(doc["entity"]["provenance"]["confidence"], "speculative");
        assert_eq!(
            doc["relations"][0]["provenance"]["confidence"],
            "speculative"
        );
    }
}
