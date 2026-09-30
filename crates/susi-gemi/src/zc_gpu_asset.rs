//! Pick GPU asset automatically for install/runtime.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuAsset {
    Cuda,
    Metal,
    Rocm,
    Cpu,
}

#[must_use]
pub fn pick_gpu_asset(has_nvidia: bool, has_metal: bool, has_rocm: bool) -> GpuAsset {
    if has_nvidia {
        GpuAsset::Cuda
    } else if has_metal {
        GpuAsset::Metal
    } else if has_rocm {
        GpuAsset::Rocm
    } else {
        GpuAsset::Cpu
    }
}

#[cfg(test)]
mod zc_gpu_asset_tests {
    use super::*;

    #[test]
    fn zc_gpu_asset_prefers_nvidia_then_metal_rocm_cpu() {
        assert_eq!(pick_gpu_asset(true, true, true), GpuAsset::Cuda);
        assert_eq!(pick_gpu_asset(false, true, true), GpuAsset::Metal);
        assert_eq!(pick_gpu_asset(false, false, true), GpuAsset::Rocm);
        assert_eq!(pick_gpu_asset(false, false, false), GpuAsset::Cpu);
    }
}
