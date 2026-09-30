//! Local model scan paths discovered from the ecosystem, not configured.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanPaths {
    pub paths: Vec<PathBuf>,
}

/// Derive scan roots from home layout + known engine/HF cache locations.
#[must_use]
pub fn discover_scan_paths(home: &str, hf_home: Option<&str>) -> ScanPaths {
    let mut paths = vec![
        PathBuf::from(format!("{home}/models")),
        PathBuf::from(format!("{home}/engines")),
        PathBuf::from(format!("{home}/.cache/huggingface/hub")),
    ];
    if let Some(hf) = hf_home {
        let p = PathBuf::from(hf).join("hub");
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    // Common download dirs relative to the user home parent of ~/.susi.
    if let Some(user_home) = std::path::Path::new(home).parent() {
        for rel in ["Downloads", "models", ".ollama/models", ".cache/lm-studio"] {
            paths.push(user_home.join(rel));
        }
    }
    ScanPaths { paths }
}

#[cfg(test)]
mod zc_scan_paths_tests {
    use super::*;

    #[test]
    fn zc_scan_paths_derived_not_hand_edited() {
        let s = discover_scan_paths("/home/u/.susi", Some("/home/u/.cache/huggingface"));
        assert!(s.paths.iter().any(|p| p.ends_with("models")));
        assert!(s
            .paths
            .iter()
            .any(|p| p.to_string_lossy().contains("huggingface")));
        assert!(s.paths.iter().any(|p| p.ends_with("Downloads")));
    }
}
