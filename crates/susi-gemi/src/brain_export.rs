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
}
