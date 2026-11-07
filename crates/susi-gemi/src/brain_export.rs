//! Export and import brain evidence between hosts.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrainBundle {
    pub version: u32,
    pub entries: Vec<Value>,
}

/// Serialize evidence entries into a portable bundle.
#[must_use]
pub fn export_brain(entries: Vec<Value>) -> BrainBundle {
    BrainBundle {
        version: 1,
        entries,
    }
}

/// Import a bundle, rejecting unknown major versions.
pub fn import_brain(bundle: BrainBundle) -> Result<Vec<Value>, String> {
    if bundle.version != 1 {
        return Err(format!(
            "unsupported brain bundle version {}",
            bundle.version
        ));
    }
    Ok(bundle.entries)
}

#[cfg(test)]
mod brain_export_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn brain_export_roundtrips() {
        let bundle = export_brain(vec![json!({"id": "EV-1", "ok": true})]);
        let back = import_brain(bundle).unwrap();
        assert_eq!(back.len(), 1);
        assert!(import_brain(BrainBundle {
            version: 99,
            entries: vec![]
        })
        .is_err());
    }

    #[test]
    fn export_brain_empty_entries_sets_version_one() {
        let bundle = export_brain(vec![]);
        assert_eq!(bundle.version, 1);
        assert!(bundle.entries.is_empty());
        assert!(import_brain(bundle).unwrap().is_empty());
    }

    #[test]
    fn brain_bundle_serde_json_roundtrip_preserves_entries() {
        let bundle = export_brain(vec![
            json!({"id": "EV-1", "score": 0.95}),
            json!({"id": "EV-2", "score": 0.82}),
        ]);
        let serialized = serde_json::to_string(&bundle).expect("serialize bundle");
        let restored: BrainBundle = serde_json::from_str(&serialized).expect("deserialize bundle");
        assert_eq!(restored, bundle);
    }

    #[test]
    fn import_brain_rejects_version_zero_with_descriptive_error() {
        let err = import_brain(BrainBundle {
            version: 0,
            entries: vec![json!({"id": "EV-0"})],
        })
        .expect_err("version 0 must be rejected");
        assert!(
            err.contains("unsupported brain bundle version 0"),
            "err={err}"
        );
    }

    #[test]
    fn import_brain_rejects_future_schema_versions_with_version_in_error() {
        let err = import_brain(BrainBundle {
            version: 2,
            entries: vec![json!({"id": "EV-future"})],
        })
        .expect_err("version 2 must be rejected");
        assert!(
            err.contains("unsupported brain bundle version 2"),
            "err={err}"
        );
    }
}
