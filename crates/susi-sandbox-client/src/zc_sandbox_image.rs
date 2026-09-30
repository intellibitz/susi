//! Default sandbox image selection.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxImage {
    pub name: String,
}

#[must_use]
pub fn default_sandbox_image(prefer_gpu: bool) -> SandboxImage {
    if prefer_gpu {
        SandboxImage {
            name: "susi/sandbox:gpu".into(),
        }
    } else {
        SandboxImage {
            name: "susi/sandbox:cpu".into(),
        }
    }
}

#[cfg(test)]
mod zc_sandbox_image_tests {
    use super::*;

    #[test]
    fn zc_sandbox_image_picks_cpu_or_gpu() {
        assert!(default_sandbox_image(false).name.contains("cpu"));
        assert!(default_sandbox_image(true).name.contains("gpu"));
    }
}
