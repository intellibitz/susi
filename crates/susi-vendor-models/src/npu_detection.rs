//! Detect NPUs and Apple/Intel/Qualcomm accelerators.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Accelerator {
    pub vendor: String,
    pub kind: String,
    pub name: String,
}

/// Classify accelerators from probe strings (sysfs/IOKit/WMI summaries).
#[must_use]
pub fn detect_accelerators(probes: &[&str]) -> Vec<Accelerator> {
    let mut out = Vec::new();
    for p in probes {
        let lower = p.to_ascii_lowercase();
        if lower.contains("ane") || lower.contains("apple neural") {
            out.push(Accelerator {
                vendor: "apple".into(),
                kind: "npu".into(),
                name: (*p).into(),
            });
        } else if lower.contains("intel") && (lower.contains("npu") || lower.contains("vpu")) {
            out.push(Accelerator {
                vendor: "intel".into(),
                kind: "npu".into(),
                name: (*p).into(),
            });
        } else if lower.contains("qualcomm") || lower.contains("hexagon") {
            out.push(Accelerator {
                vendor: "qualcomm".into(),
                kind: "npu".into(),
                name: (*p).into(),
            });
        }
    }
    out
}

#[cfg(test)]
mod npu_detection_tests {
    use super::*;

    #[test]
    fn npu_detection_classifies_vendors() {
        let found = detect_accelerators(&[
            "Apple Neural Engine",
            "Intel AI Boost NPU",
            "Qualcomm Hexagon",
            "NVIDIA RTX",
        ]);
        assert_eq!(found.len(), 3);
        assert!(found.iter().any(|a| a.vendor == "apple"));
        assert!(found.iter().any(|a| a.vendor == "intel"));
        assert!(found.iter().any(|a| a.vendor == "qualcomm"));
    }
}
