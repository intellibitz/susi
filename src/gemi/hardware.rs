// 100% Rust implementation for autonomous hardware profiling

use std::sync::OnceLock;
use serde::{Deserialize, Serialize};
use candle_core::Device;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareProfile {
    pub cpus: usize,
    pub cpu_brand: String,
    pub gpu_info: String,
    pub ram_gb: usize,
    pub available_ram_gb: usize,
    pub gpu_vram_gb: usize,
    pub swap_gb: usize,
    pub nvme_active: bool,
    pub acceleration_active: bool,
    pub native_acceleration: String,
    pub os_info: String,
    pub arch: String,
    pub disk_gb: usize,
    pub disk_usage_pct: u8,
    pub load_avg: String,
    pub uptime: String,
    pub hostname: String,
}

pub struct HardwareProfiler;

impl HardwareProfiler {
    pub fn get_profile() -> HardwareProfile {
        static CACHED_PROFILE: OnceLock<HardwareProfile> = OnceLock::new();
        CACHED_PROFILE.get_or_init(|| {
            let (cpus, _) = Self::profile();
            let ram_gb = Self::determine_total_ram_gb();
            let available_ram_gb = Self::determine_available_ram_gb();
            let gpu_vram_gb = Self::determine_gpu_vram_gb();
            let swap_gb = Self::determine_swap_gb();
            let nvme_active = Self::is_nvme_active();

            // 1. Direct Interrogation via Candle Substrate
            let (native_accel, gpu_name) = Self::interrogate_native_acceleration();

            let acceleration_active = !native_accel.contains("None") && !native_accel.contains("Cpu");
            let gpu_display = if acceleration_active {
                format!("{} ({} | {}GB VRAM)", native_accel, gpu_name, gpu_vram_gb)
            } else {
                // 2. Fallback to Meta-Parsing for diagnostics if native probe is inactive
                let (_, shell_gpu) = Self::profile();
                shell_gpu
            };

            HardwareProfile {
                cpus,
                cpu_brand: Self::get_cpu_brand(),
                gpu_info: gpu_display,
                ram_gb,
                available_ram_gb,
                gpu_vram_gb,
                swap_gb,
                nvme_active,
                acceleration_active,
                native_acceleration: native_accel,
                os_info: Self::get_os_info(),
                arch: std::env::consts::ARCH.to_string(),
                disk_gb: Self::determine_disk_gb(),
                disk_usage_pct: Self::determine_disk_usage_pct(),
                load_avg: Self::get_load_avg(),
                uptime: Self::get_uptime(),
                hostname: Self::get_hostname(),
            }
        }).clone()
    }

    pub fn check_oom_critical() -> bool {
        let profile = Self::get_profile();
        if profile.ram_gb == 0 { return false; }
        let usage_pct = ((profile.ram_gb - profile.available_ram_gb) as f32 / profile.ram_gb as f32) * 100.0;
        usage_pct > 90.0
    }

    fn get_cpu_brand() -> String {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
                for line in content.lines() {
                    if line.starts_with("model name") {
                        return line.split(':').nth(1).unwrap_or("Unknown CPU").trim().to_string();
                    }
                }
            }
        }
        "Generic Hardware Substrate".to_string()
    }

    fn get_load_avg() -> String {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/proc/loadavg") {
                let parts: Vec<&str> = content.split_whitespace().collect();
                if parts.len() >= 3 {
                    return format!("{}, {}, {}", parts[0], parts[1], parts[2]);
                }
            }
        }
        "N/A".to_string()
    }

    fn get_uptime() -> String {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/proc/uptime") {
                if let Some(secs_str) = content.split_whitespace().next() {
                    if let Ok(secs) = secs_str.parse::<f64>() {
                        let hours = (secs / 3600.0) as u64;
                        let mins = ((secs % 3600.0) / 60.0) as u64;
                        return format!("up {} hours, {} minutes", hours, mins);
                    }
                }
            }
        }
        "N/A".to_string()
    }

    fn get_hostname() -> String {
        std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| {
                std::fs::read_to_string("/etc/hostname")
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|_| "localhost".to_string())
            })
    }

    #[cfg(unix)]
    fn get_disk_stats() -> (usize, u8) {
        use std::ffi::CString;
        if let Ok(c_path) = CString::new("/") {
            let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } == 0 {
                let block_size = stat.f_frsize as u64;
                let total_blocks = stat.f_blocks as u64;
                let free_blocks = stat.f_bavail as u64;
                let total_bytes = total_blocks * block_size;
                let free_bytes = free_blocks * block_size;
                let used_bytes = total_bytes.saturating_sub(free_bytes);
                let total_gb = (total_bytes / (1024 * 1024 * 1024)) as usize;
                let usage_pct = if total_bytes > 0 {
                    ((used_bytes as f64 / total_bytes as f64) * 100.0) as u8
                } else {
                    0
                };
                return (total_gb, usage_pct);
            }
        }
        (256, 0)
    }

    #[cfg(not(unix))]
    fn get_disk_stats() -> (usize, u8) {
        (256, 0)
    }

    fn determine_disk_usage_pct() -> u8 {
        Self::get_disk_stats().1
    }

    pub fn get_caps_string() -> String {
        let profile = Self::get_profile();
        let mut caps = Vec::new();
        if profile.acceleration_active {
            caps.push("GPU".to_string());
        } else {
            caps.push("CPU".to_string());
        }
        caps.push(format!("{}GB", profile.ram_gb));
        caps.push(format!("{}V", crate::SUSI_VERSION));
        caps.join(",")
    }

    pub fn get_candle_device() -> Device {
        // Zero-Lock Device Cache (Aspiration 24 & Sub-2ms Mandate)
        static DEVICE_CACHE: OnceLock<Device> = OnceLock::new();
        DEVICE_CACHE.get_or_init(|| {
            #[cfg(feature = "cuda")]
            {
                // Attempt CUDA initialization with panic safety
                let cuda_attempt = std::panic::catch_unwind(|| {
                    Device::new_cuda(0)
                });
                if let Ok(Ok(cuda_dev)) = cuda_attempt {
                    return cuda_dev;
                }
            }

            // Attempt Metal initialization with panic safety
            #[cfg(feature = "metal")]
            {
                let metal_attempt = std::panic::catch_unwind(|| {
                    Device::new_metal(0)
                });
                if let Ok(Ok(metal_dev)) = metal_attempt {
                    return metal_dev;
                }
            }

            // Absolute Fallback: CPU
            Device::Cpu
        }).clone()
    }

    fn get_os_info() -> String {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
                for line in content.lines() {
                    if line.starts_with("PRETTY_NAME=") {
                        return line.trim_start_matches("PRETTY_NAME=").trim_matches('"').to_string();
                    }
                }
            }
            return "Linux".to_string();
        } else if cfg!(target_os = "macos") {
            return "macOS".to_string();
        } else if cfg!(target_os = "windows") {
             return "Windows".to_string();
        }
        "Unknown OS".to_string()
    }

    fn determine_disk_gb() -> usize {
        Self::get_disk_stats().0
    }

    fn interrogate_native_acceleration() -> (String, String) {
        if candle_core::utils::cuda_is_available() {
             return ("CUDA (Detected)".to_string(), "NVIDIA Driver found".to_string());
        }

        if candle_core::utils::metal_is_available() {
             return ("Metal (Detected)".to_string(), "Apple Silicon / macOS".to_string());
        }

        ("None".to_string(), "Cpu".to_string())
    }

    fn determine_gpu_vram_gb() -> usize {
        if cfg!(target_os = "linux") {
            if let Ok(entries) = std::fs::read_dir("/sys/class/drm") {
                for entry in entries.flatten() {
                    let vram_path = entry.path().join("device/mem_info_vram_total");
                    if vram_path.exists() {
                        if let Ok(content) = std::fs::read_to_string(&vram_path) {
                            if let Ok(bytes) = content.trim().parse::<u64>() {
                                if bytes > 0 {
                                    return (bytes / (1024 * 1024 * 1024)) as usize;
                                }
                            }
                        }
                    }
                }
            }
        }
        0
    }

    fn determine_swap_gb() -> usize {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
                for line in content.lines() {
                    if line.starts_with("SwapTotal:") {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if let Some(kb_str) = parts.get(1) {
                            if let Ok(kb) = kb_str.parse::<usize>() {
                                return kb / (1024 * 1024);
                            }
                        }
                    }
                }
            }
        }
        0
    }

    fn is_nvme_active() -> bool {
        if cfg!(target_os = "linux") {
            if let Ok(entries) = std::fs::read_dir("/sys/block/") {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().starts_with("nvme") {
                        return true;
                    }
                }
            }
        }
        false
    }

    pub fn profile() -> (usize, String) {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);

        let gpu_info = if candle_core::utils::cuda_is_available() {
            "CUDA Acceleration Substrate Active".to_string()
        } else if candle_core::utils::metal_is_available() {
            "Metal Acceleration Substrate Active".to_string()
        } else {
            format!("CPU Parallel Execution Substrate Active ({} Threads)", cpus)
        };

        (cpus, gpu_info)
    }

    pub fn determine_total_ram_gb() -> usize {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
                for line in content.lines() {
                    if line.starts_with("MemTotal:") {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if let Some(kb_str) = parts.get(1) {
                            if let Ok(kb) = kb_str.parse::<usize>() {
                                return kb / (1024 * 1024);
                            }
                        }
                    }
                }
            }
        } else if cfg!(target_os = "macos") {
            #[cfg(target_os = "macos")]
            {
                use std::ffi::CString;
                if let Ok(c_name) = CString::new("hw.memsize") {
                    let mut val: u64 = 0;
                    let mut size = std::mem::size_of::<u64>();
                    if unsafe { libc::sysctlbyname(c_name.as_ptr(), &mut val as *mut _ as *mut libc::c_void, &mut size, std::ptr::null_mut(), 0) } == 0 {
                        return (val / (1024 * 1024 * 1024)) as usize;
                    }
                }
            }
            return 16;
        } else if cfg!(target_os = "windows") {
            return 16;
        }
        8
    }

    pub fn determine_available_ram_gb() -> usize {
        if cfg!(target_os = "linux") {
            if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
                for line in content.lines() {
                    if line.starts_with("MemAvailable:") {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if let Some(kb_str) = parts.get(1) {
                            if let Ok(kb) = kb_str.parse::<usize>() {
                                return kb / (1024 * 1024);
                            }
                        }
                    }
                }
            }
        }
        Self::determine_total_ram_gb() // Fallback
    }

    pub fn get_progressive_model_ladder() -> Vec<ModelLadderStep> {
        let ram_gb = Self::determine_total_ram_gb();
        let cfg = crate::sandbox::manager::SusiConfig::load_global().unwrap_or_default();

        let mut ladder = Vec::new();
        for step in cfg.model_ladder {
            if ram_gb >= step.min_ram_gb {
                ladder.push(ModelLadderStep {
                    step: step.step,
                    label: step.label,
                    hf_repo: step.hf_repo,
                    hf_file: step.hf_file,
                });
            }
        }

        if ladder.is_empty() {
            ladder.push(ModelLadderStep {
                step: 1,
                label: "1.5B Parameters (Fast Local Edge)".to_string(),
                hf_repo: "susi-alpha/susi-alpha-1.5b-instruct-v0.1-GGUF".to_string(),
                hf_file: "susi-alpha-1.5b-instruct-q4_k_m.gguf".to_string(),
            });
        }

        ladder
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelLadderStep {
    pub step: usize,
    pub label: String,
    pub hf_repo: String,
    pub hf_file: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hardware_profile_generation() {
        let profile = HardwareProfiler::get_profile();
        assert!(profile.cpus > 0);
        assert!(profile.ram_gb > 0);
    }

    #[test]
    fn test_progressive_model_ladder_ordering() {
        let ladder = HardwareProfiler::get_progressive_model_ladder();
        assert!(!ladder.is_empty());
        let mut last_step = 0;
        for step in ladder {
            assert!(step.step > last_step);
            last_step = step.step;
            assert!(!step.label.is_empty());
            assert!(!step.hf_repo.is_empty());
            assert!(!step.hf_file.is_empty());
        }
    }

    #[test]
    fn test_candle_device_retrieval() {
        let device = HardwareProfiler::get_candle_device();
        // Simply ensure it doesn't panic and returns a valid variant
        match device {
            candle_core::Device::Cpu => {}
            candle_core::Device::Cuda(_) => {}
            candle_core::Device::Metal(_) => {}
        }
    }
}
