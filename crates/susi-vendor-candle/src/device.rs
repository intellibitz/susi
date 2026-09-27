//! Runtime Candle device probe (CUDA, then Metal, then CPU).
//!
//! Compile-time `cfg!(feature = "cuda")` / `metal_is_available()` helpers
//! report the feature matrix, not whether a device actually initialized.
//! Callers that place tensors must use [`cached_device`].

use candle_core::Device;
use std::sync::OnceLock;

/// Process-wide Candle device. The first successful CUDA or Metal init
/// wins; later calls reuse that handle so a failed GPU probe is not
/// retried on every generate.
pub fn cached_device() -> Device {
    static DEVICE_CACHE: OnceLock<Device> = OnceLock::new();
    DEVICE_CACHE.get_or_init(probe).clone()
}

/// Probe CUDA (when featured), then Metal (when featured), then CPU.
/// Each GPU constructor is wrapped in `catch_unwind` because a missing
/// driver or toolkit mismatch can abort inside the vendor FFI.
pub fn probe() -> Device {
    #[cfg(feature = "cuda")]
    {
        if let Ok(Ok(cuda_dev)) = std::panic::catch_unwind(|| Device::new_cuda(0)) {
            return cuda_dev;
        }
    }

    #[cfg(feature = "metal")]
    {
        if let Ok(Ok(metal_dev)) = std::panic::catch_unwind(|| Device::new_metal(0)) {
            return metal_dev;
        }
    }

    Device::Cpu
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_returns_a_known_device_kind() {
        match probe() {
            Device::Cpu | Device::Cuda(_) | Device::Metal(_) => {}
        }
    }

    #[test]
    fn cached_device_is_stable() {
        let first = cached_device();
        let second = cached_device();
        assert_eq!(first.is_cuda(), second.is_cuda());
        assert_eq!(first.is_metal(), second.is_metal());
    }
}
