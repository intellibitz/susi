//! Detect retired models from /models diffs and remap.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeprecationRemap {
    pub retired: Vec<String>,
    pub remaps: BTreeMap<String, String>,
}

/// Diff previous vs current model ids; map retired ids via a replacement table.
#[must_use]
pub fn detect_deprecations(
    previous: &[&str],
    current: &[&str],
    replacements: &BTreeMap<String, String>,
) -> DeprecationRemap {
    let mut retired = Vec::new();
    let mut remaps = BTreeMap::new();
    for p in previous {
        if !current.iter().any(|c| c == p) {
            retired.push((*p).to_string());
            if let Some(to) = replacements.get(*p) {
                remaps.insert((*p).to_string(), to.clone());
            }
        }
    }
    DeprecationRemap { retired, remaps }
}

#[cfg(test)]
mod model_deprecation_tests {
    use super::*;

    #[test]
    fn model_deprecation_detects_and_remaps() {
        let mut rep = BTreeMap::new();
        rep.insert("gpt-3.5".into(), "gpt-4o-mini".into());
        let d = detect_deprecations(&["gpt-3.5", "gpt-4"], &["gpt-4", "gpt-4o"], &rep);
        assert_eq!(d.retired, vec!["gpt-3.5".to_string()]);
        assert_eq!(
            d.remaps.get("gpt-3.5").map(String::as_str),
            Some("gpt-4o-mini")
        );
    }
}
