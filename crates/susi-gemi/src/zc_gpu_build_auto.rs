//! `cargo build` picks the GPU backend by itself.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuBuildPlan {
    pub features: Vec<String>,
}

/// Choose cargo features from detected GPU backends.
#[must_use]
pub fn gpu_build_features(has_cuda: bool, has_metal: bool, has_mkl: bool) -> GpuBuildPlan {
    let mut features = Vec::new();
    if has_cuda {
        features.push("cuda".into());
    } else if has_metal {
        features.push("metal".into());
    } else if has_mkl {
        features.push("mkl".into());
    }
    GpuBuildPlan { features }
}

#[cfg(test)]
mod zc_gpu_build_auto_tests {
    use super::*;

    #[test]
    fn zc_gpu_build_auto_prefers_cuda() {
        let p = gpu_build_features(true, true, true);
        assert_eq!(p.features, vec!["cuda".to_string()]);
        assert!(gpu_build_features(false, false, false).features.is_empty());
        assert_eq!(
            gpu_build_features(false, true, false).features,
            vec!["metal".to_string()]
        );
    }
}
