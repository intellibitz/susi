//! Inference device handle.
//!
//! Names CPU, CUDA, and Metal so hardware code can match exhaustively and
//! report that no built-in engine is linked in the slim core.

mod absent {
    /// Where a device lives. Debug text matches Candle's `DeviceLocation`
    /// for the CPU variant the slim core actually returns (`Cpu`).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum DeviceLocation {
        Cpu,
        Cuda { gpu_id: usize },
        Metal { gpu_id: usize },
    }

    /// Stand-in for `candle_core::Device` when the local engine is not linked.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Device {
        Cpu,
        Cuda(usize),
        Metal(usize),
    }

    impl Device {
        pub fn location(&self) -> DeviceLocation {
            match self {
                Device::Cpu => DeviceLocation::Cpu,
                Device::Cuda(gpu_id) => DeviceLocation::Cuda { gpu_id: *gpu_id },
                Device::Metal(gpu_id) => DeviceLocation::Metal { gpu_id: *gpu_id },
            }
        }

        pub fn same_device(&self, rhs: &Self) -> bool {
            self == rhs
        }

        pub fn is_cpu(&self) -> bool {
            matches!(self, Device::Cpu)
        }

        pub fn is_cuda(&self) -> bool {
            matches!(self, Device::Cuda(_))
        }

        pub fn is_metal(&self) -> bool {
            matches!(self, Device::Metal(_))
        }
    }
}

pub use absent::Device;

#[cfg(test)]
mod tests {
    use super::Device;

    #[test]
    fn slim_core_device_is_cpu_and_compares_by_value() {
        let device = Device::Cpu;
        assert!(device.is_cpu());
        assert!(!device.is_cuda());
        assert!(!device.is_metal());
        assert!(device.same_device(&Device::Cpu));
        assert!(!device.same_device(&Device::Cuda(0)));
        assert_eq!(format!("{:?}", device.location()), "Cpu");
    }
}
